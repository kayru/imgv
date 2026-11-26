use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::um::winuser::{
    ClientToScreen, GetClientRect, GetMonitorInfoW, GetSystemMetrics, GetWindowRect, MonitorFromWindow,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, SM_CXSCREEN,
    SM_CYSCREEN,
};

pub trait Dimensions {
    fn dim(&self) -> (i32, i32);
}

impl Dimensions for RECT {
    fn dim(&self) -> (i32, i32) {
        (self.right - self.left, self.bottom - self.top)
    }
}

pub fn to_wide_string(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>()
}

pub fn get_screen_dimensions() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

pub fn get_client_rect_absolute(hwnd: HWND) -> RECT {
    let mut client_rect = make_empty_rect();
    unsafe {
        GetClientRect(hwnd, &mut client_rect);
    }
    client_rect
}

pub fn get_window_rect(hwnd: HWND) -> RECT {
    let mut rect = make_empty_rect();
    unsafe {
        GetWindowRect(hwnd, &mut rect);
    }
    rect
}

pub fn get_client_rect(hwnd: HWND) -> RECT {
    let mut client_rect = make_empty_rect();
    unsafe {
        GetClientRect(hwnd, &mut client_rect);
        let mut origin = POINT { x: 0, y: 0 };
        ClientToScreen(hwnd, &mut origin);
        client_rect.left += origin.x;
        client_rect.right += origin.x;
        client_rect.top += origin.y;
        client_rect.bottom += origin.y;
    }
    client_rect
}

fn get_monitor_info(hwnd: HWND) -> MONITORINFO {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: make_empty_rect(),
            rcWork: make_empty_rect(),
            dwFlags: 0,
        };
        GetMonitorInfoW(monitor, &mut info);
        info
    }
}

pub fn get_monitor_rect(hwnd: HWND) -> RECT {
    get_monitor_info(hwnd).rcMonitor
}

pub fn get_monitor_work_rect(hwnd: HWND) -> RECT {
    get_monitor_info(hwnd).rcWork
}

pub fn rects_equal(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

pub fn get_window_client_rect_dimensions(hwnd: HWND) -> (i32, i32) {
    let client_rect = get_client_rect(hwnd);
    (
        client_rect.right - client_rect.left,
        client_rect.bottom - client_rect.top,
    )
}

pub fn compute_client_rect(dim: (i32, i32)) -> RECT {
    let screen_dim = get_screen_dimensions();
    let window_pos = (screen_dim.0 / 2 - dim.0 / 2, screen_dim.1 / 2 - dim.1 / 2);
    RECT {
        left: window_pos.0,
        top: window_pos.1,
        right: window_pos.0 + dim.0,
        bottom: window_pos.1 + dim.1,
    }
}

fn make_empty_rect() -> RECT {
    RECT {
        left: 0,
        right: 0,
        top: 0,
        bottom: 0,
    }
}

pub fn disable_rounded_corners(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    };

    let preference = DWMWCP_DONOTROUND;
    let result = unsafe {
        DwmSetWindowAttribute(
            windows::Win32::Foundation::HWND(hwnd as _),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as _,
            std::mem::size_of_val(&preference) as u32,
        )
    };
    if let Err(e) = result {
        log::debug!("Window corner preference unavailable: {e}");
    }
}
