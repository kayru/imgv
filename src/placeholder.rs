use anyhow::{anyhow, Result};
use com_ptr::ComPtr;
use winapi::shared::dxgi1_2::IDXGISwapChain1;
use winapi::um::d3d11::{ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWINDOWATTRIBUTE, DwmFlush, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use crate::graphics::{displayed_present_count, last_present_count, GraphicsD3D11};

const MAX_COMPOSITIONS_TO_PRESENT: u32 = 30;

pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

// Covers the main window while its geometry changes, until its swap chain has a frame on screen
// for the new geometry. Windows keeps an owned window above its owner.
pub struct Placeholder {
    hwnd: HWND,
    swapchain: Option<ComPtr<IDXGISwapChain1>>,
}

unsafe extern "system" fn placeholder_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl Placeholder {
    pub fn show(
        owner: *mut std::ffi::c_void,
        graphics: &GraphicsD3D11,
        rect: &ScreenRect,
        draw: impl Fn(*mut ID3D11RenderTargetView, *mut ID3D11Texture2D, (u32, u32)),
    ) -> Result<Self> {
        unsafe {
            let hinst = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(placeholder_proc),
                hInstance: hinst.into(),
                lpszClassName: w!("imgv_placeholder"),
                ..Default::default()
            };
            RegisterClassW(&class);

            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT,
                w!("imgv_placeholder"),
                w!(""),
                WS_POPUP,
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                Some(HWND(owner)),
                None,
                Some(hinst.into()),
                None,
            )?;
            let mut placeholder = Placeholder {
                hwnd,
                swapchain: None,
            };

            set_dwm_flag(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, true)?;

            // A new swap chain has no defined content before its first present, so the window stays
            // cloaked until DWM holds the first frame.
            set_dwm_flag(hwnd, DWMWA_CLOAK, true)?;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            let dim = (rect.w as u32, rect.h as u32);
            let (swapchain, texture, rtv) = graphics
                .create_swap_chain(hwnd.0 as _, dim)
                .ok_or_else(|| anyhow!("Placeholder swap chain creation failed"))?;
            placeholder.swapchain = Some(swapchain.clone());

            draw(rtv.as_ptr(), texture.as_ptr(), dim);
            swapchain.Present(0, 0);
            if !graphics.wait_for_gpu() {
                return Err(anyhow!("Placeholder frame timed out on the GPU"));
            }
            wait_for_composition();

            set_dwm_flag(hwnd, DWMWA_CLOAK, false)?;
            draw(rtv.as_ptr(), texture.as_ptr(), dim);
            swapchain.Present(1, 0);
            if !wait_until_presented(&swapchain) {
                return Err(anyhow!("Placeholder frame timed out before display"));
            }
            Ok(placeholder)
        }
    }
}

impl Drop for Placeholder {
    fn drop(&mut self) {
        self.swapchain = None;
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

pub fn set_transitions_disabled(hwnd: *mut std::ffi::c_void, disabled: bool) {
    if let Err(e) = set_dwm_flag(HWND(hwnd), DWMWA_TRANSITIONS_FORCEDISABLED, disabled) {
        log::warn!("Window transition setting failed: {e}");
    }
}

fn set_dwm_flag(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: bool) -> Result<()> {
    let value = value as i32;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            &value as *const i32 as _,
            std::mem::size_of::<i32>() as u32,
        )?;
    }
    Ok(())
}

fn wait_until_presented(swapchain: &ComPtr<IDXGISwapChain1>) -> bool {
    let target = last_present_count(swapchain);
    for _ in 0..MAX_COMPOSITIONS_TO_PRESENT {
        wait_for_composition();
        if displayed_present_count(swapchain).is_some_and(|count| count >= target) {
            return true;
        }
    }
    false
}

// The placeholder window belongs to this thread, so this thread has to dispatch its messages.
pub fn wait_for_composition() {
    unsafe {
        let _ = DwmFlush();
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
