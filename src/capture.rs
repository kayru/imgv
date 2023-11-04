use crate::get_client_rect_absolute;
use anyhow::{anyhow, Result};
use display_info::DisplayInfo;
use widestring::U16CString;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDIBits, GetMonitorInfoW, GetObjectW, MapWindowPoints, SelectObject,
    SetStretchBltMode, StretchBlt, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP,
    HDC, HMONITOR, MONITORINFOEXW, RGBQUAD, SRCCOPY, STRETCH_HALFTONE,
};

macro_rules! scoped_drop {
    ($type:tt, $value:expr, $drop:expr) => {{
        struct ScopedDrop($type);

        impl std::ops::Deref for ScopedDrop {
            type Target = $type;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl Drop for ScopedDrop {
            fn drop(&mut self) {
                $drop(self.0);
            }
        }

        ScopedDrop($value)
    }};
}

fn get_monitor_info(h_monitor: HMONITOR) -> Result<MONITORINFOEXW> {
    let mut monitor_info_exw: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    monitor_info_exw.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let monitor_info_exw_ptr = <*mut _>::cast(&mut monitor_info_exw);

    unsafe {
        GetMonitorInfoW(h_monitor, monitor_info_exw_ptr).ok()?;
    };
    Ok(monitor_info_exw)
}

extern "system" fn monitor_enum_proc(
    h_monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    state: LPARAM,
) -> BOOL {
    let box_monitor_info_exw = unsafe { Box::from_raw(state.0 as *mut Vec<MONITORINFOEXW>) };
    let state = Box::leak(box_monitor_info_exw);

    match get_monitor_info(h_monitor) {
        Ok(monitor_info_exw) => {
            state.push(monitor_info_exw);
            BOOL::from(true)
        }
        Err(_) => BOOL::from(false),
    }
}

fn get_monitor_info_by_id(id: u32) -> Result<MONITORINFOEXW> {
    let monitor_info: *mut Vec<MONITORINFOEXW> = Box::into_raw(Box::default());

    unsafe {
        EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_enum_proc),
            LPARAM(monitor_info as isize),
        )
        .ok()?;
    };

    let monitor_info_borrow = unsafe { &Box::from_raw(monitor_info) };

    let monitor_info_res = monitor_info_borrow
        .iter()
        .find(|&&monitor_info| {
            let sz_device_ptr = monitor_info.szDevice.as_ptr();
            let sz_device_string =
                unsafe { U16CString::from_ptr_str(sz_device_ptr).to_string_lossy() };
            fxhash::hash32(sz_device_string.as_bytes()) == id
        })
        .ok_or_else(|| anyhow!("Can't find a display by id {id}"))?;

    Ok(*monitor_info_res)
}

#[derive(Debug, Clone, Copy)]
struct CaptureArea {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

fn calc_capture_area(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    display_info: &DisplayInfo,
) -> Result<CaptureArea> {
    let screen_x = display_info.x + display_info.width as i32;
    let screen_y = display_info.y + display_info.height as i32;

    let mut x1 = x + display_info.x;
    let mut y1 = y + display_info.y;

    let mut x2 = x1 + w as i32;
    let mut y2 = y1 + h as i32;

    if x1 < display_info.x {
        x1 = display_info.x;
    } else if x1 > screen_x {
        x1 = screen_x
    }

    if y1 < display_info.y {
        y1 = display_info.y;
    } else if y1 > screen_y {
        y1 = screen_y;
    }

    if x2 > screen_x {
        x2 = screen_x;
    }

    if y2 > screen_y {
        y2 = screen_y;
    }

    if x1 >= x2 || y1 >= y2 {
        return Err(anyhow!("Invalid capture area dimensions"));
    }

    Ok(CaptureArea {
        x: x1 - display_info.x,
        y: y1 - display_info.y,
        w: (x2 - x1) as u32,
        h: (y2 - y1) as u32,
    })
}

pub fn capture_window(hwnd: isize) -> Result<image::RgbaImage> {
    let mut mapped_point: [POINT; 1] = [POINT::default()];
    unsafe {
        MapWindowPoints(HWND(hwnd), HWND(0), &mut mapped_point);
    }

    let region_x = mapped_point[0].x;
    let region_y = mapped_point[0].y;

    let display_info =
        DisplayInfo::from_point(region_x, region_y).expect("Failed to look up display info");
    let monitor_info = get_monitor_info_by_id(display_info.id)?;

    let client_rect = get_client_rect_absolute(hwnd as *mut _);

    let capture_area = calc_capture_area(
        region_x,
        region_y,
        client_rect.right,
        client_rect.bottom,
        &display_info,
    )
    .expect("Failed to calculate capture area");

    let x = ((capture_area.x as f32) * display_info.scale_factor) as i32;
    let y = ((capture_area.y as f32) * display_info.scale_factor) as i32;
    let w = ((capture_area.w as f32) * display_info.scale_factor) as i32;
    let h = ((capture_area.h as f32) * display_info.scale_factor) as i32;

    let sz_device = monitor_info.szDevice;
    let sz_device_ptr = sz_device.as_ptr();

    let scoped_dcw = scoped_drop!(
        HDC,
        unsafe {
            CreateDCW(
                PCWSTR(sz_device_ptr),
                PCWSTR(sz_device_ptr),
                PCWSTR(std::ptr::null()),
                None,
            )
        },
        |dcw| unsafe { DeleteDC(dcw) }
    );

    let scoped_compat_dc = scoped_drop!(
        HDC,
        unsafe { CreateCompatibleDC(*scoped_dcw) },
        |compatible_dc| unsafe { DeleteDC(compatible_dc) }
    );

    let scoped_compat_bm = scoped_drop!(
        HBITMAP,
        unsafe { CreateCompatibleBitmap(*scoped_dcw, w, h) },
        |h_bitmap| unsafe { DeleteObject(h_bitmap) }
    );

    unsafe {
        SelectObject(*scoped_compat_dc, *scoped_compat_bm);
        SetStretchBltMode(*scoped_dcw, STRETCH_HALFTONE);
    };

    unsafe {
        StretchBlt(
            *scoped_compat_dc,
            0,
            0,
            w,
            h,
            *scoped_dcw,
            x,
            y,
            w,
            h,
            SRCCOPY,
        )
        .ok()?;
    };

    let mut bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD::default(); 1],
    };

    let data = vec![0u8; (w * h) as usize * 4];
    let buf_prt = data.as_ptr() as *mut _;

    let is_success = unsafe {
        GetDIBits(
            *scoped_dcw,
            *scoped_compat_bm,
            0,
            h as u32,
            Some(buf_prt),
            &mut bitmap_info,
            DIB_RGB_COLORS,
        ) == 0
    };

    if is_success {
        return Err(anyhow!("Get RGBA data failed"));
    }

    let mut bitmap = BITMAP::default();
    let bitmap_ptr = <*mut _>::cast(&mut bitmap);

    unsafe {
        GetObjectW(
            *scoped_compat_bm,
            std::mem::size_of::<BITMAP>() as i32,
            Some(bitmap_ptr),
        );
    }

    let mut chunks: Vec<Vec<u8>> = data.chunks(w as usize * 4).map(|x| x.to_vec()).collect();

    chunks.reverse();

    let rgba_buf = chunks
        .concat()
        .chunks_exact(4)
        .take((w * h) as usize)
        .flat_map(|bgra| [bgra[2], bgra[1], bgra[0], bgra[3]])
        .collect();

    image::RgbaImage::from_vec(w as u32, h as u32, rgba_buf)
        .ok_or(anyhow!("Image buffer is not large enough"))
}
