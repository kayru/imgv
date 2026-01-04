use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use winapi::shared::windef::{HWND, RECT};
use winapi::um::winuser::{
    GetClientRect, GetSystemMetrics, GetWindowRect, SM_CXMAXIMIZED, SM_CXSCREEN, SM_CYMAXIMIZED,
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

pub fn get_window_rect_absolute(hwnd: HWND) -> RECT {
    let mut window_rect = make_empty_rect();
    unsafe {
        GetWindowRect(hwnd, &mut window_rect);
    }
    window_rect
}

pub fn get_client_rect(hwnd: HWND) -> RECT {
    let mut client_rect = make_empty_rect();
    unsafe {
        let mut window_rect = make_empty_rect();
        GetWindowRect(hwnd, &mut window_rect);
        GetClientRect(hwnd, &mut client_rect);
        client_rect.left += window_rect.left;
        client_rect.right += window_rect.left;
        client_rect.top += window_rect.top;
        client_rect.bottom += window_rect.top;
    }
    client_rect
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

pub fn get_desktop_work_area() -> RECT {
    let dim = unsafe {
        let ix = GetSystemMetrics(SM_CXMAXIMIZED);
        let iy = GetSystemMetrics(SM_CYMAXIMIZED);
        (ix, iy)
    };
    RECT {
        left: 0,
        top: 0,
        right: dim.0,
        bottom: dim.1,
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
