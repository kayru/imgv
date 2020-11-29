// #![windows_subsystem = "windows"]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::time::Instant;
use winapi::shared::dxgi::*;
use winapi::shared::dxgiformat::*;
use winapi::shared::dxgitype::*;
use winapi::shared::minwindef::{HINSTANCE, LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::ntdef::HRESULT;
use winapi::shared::ntdef::LPCWSTR;
use winapi::shared::windef::{HBRUSH, HICON, HMENU, HWND};
use winapi::um::d3d11::*;
use winapi::um::d3dcommon::*;
use winapi::um::winuser::*;
use winapi::Interface;

struct WindowCreatedData {
    hwnd: HWND,
}

enum WindowMessages {
    WindowCreated(WindowCreatedData),
    WindowClosed,
}

unsafe impl std::marker::Send for WindowCreatedData {}

struct Window {
    message_receiver: std::sync::mpsc::Receiver<WindowMessages>,
    hwnd: HWND,
}

struct WindowThreadState {
    message_sender: std::sync::mpsc::Sender<WindowMessages>,
    is_window_closed: bool,
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: UINT,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let create_struct = l_param as *mut winapi::um::winuser::CREATESTRUCTW;
        let window_state_ptr =
            create_struct.as_ref().unwrap().lpCreateParams as *mut WindowThreadState;
        let window_state: &mut WindowThreadState = window_state_ptr.as_mut().unwrap();
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, window_state_ptr as isize);
        window_state
            .message_sender
            .send(WindowMessages::WindowCreated(WindowCreatedData { hwnd }))
            .unwrap();
    }

    if msg == WM_DESTROY {
        let window_state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowThreadState;
        let window_state: &mut WindowThreadState = window_state_ptr.as_mut().unwrap();

        window_state
            .message_sender
            .send(WindowMessages::WindowClosed)
            .unwrap();
        window_state.is_window_closed = true;

        PostQuitMessage(0);
    }

    DefWindowProcW(hwnd, msg, w_param, l_param)
}

fn create_window() -> Result<Window, ()> {
    let (channel_sender, channel_receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let mut window_state = WindowThreadState {
            message_sender: channel_sender,
            is_window_closed: false,
        };

        unsafe {
            let window_name: Vec<u16> = OsStr::new("imgv\0").encode_wide().collect();

            let window_class_name: Vec<u16> =
                OsStr::new("imgv_window_class\0").encode_wide().collect();

            let window_class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: 0 as HINSTANCE,
                hIcon: 0 as HICON,
                hCursor: 0 as HICON,
                hbrBackground: 16 as HBRUSH,
                lpszMenuName: 0 as LPCWSTR,
                lpszClassName: window_class_name.as_ptr(),
            };

            let error_code = RegisterClassW(&window_class);

            assert!(error_code != 0, "failed to register the window class");

            let hwnd_window = CreateWindowExW(
                0,
                window_class_name.as_ptr(),
                window_name.as_ptr(),
                WS_OVERLAPPED | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU,
                0,
                0,
                512,
                512,
                0 as HWND,
                0 as HMENU,
                0 as HINSTANCE,
                &mut window_state as *mut WindowThreadState as *mut winapi::ctypes::c_void,
            );

            assert!(hwnd_window != (0 as HWND), "failed to open the window");

            // ShowWindow(hwnd_window, SW_SHOW);

            let mut msg: MSG = std::mem::zeroed();

            while !window_state.is_window_closed {
                if PeekMessageA(&mut msg, hwnd_window, 0, 0, PM_REMOVE) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageA(&msg);
                }
            }
        }
    });

    if let WindowMessages::WindowCreated(data) = channel_receiver.recv().unwrap() {
        return Ok(Window {
            message_receiver: channel_receiver,
            hwnd: data.hwnd,
        });
    }

    Err(())
}

fn process_window_messages(window: &Window) -> Option<WindowMessages> {
    if let Ok(x) = window.message_receiver.try_recv() {
        return Some(x);
    }

    None
}

struct GraphicsD3D11 {
    device: *mut ID3D11Device,
    context: *mut ID3D11DeviceContext,
    swapchain: *mut IDXGISwapChain,
    backbuffer_rtv: *mut ID3D11RenderTargetView,
    backbuffer_tex: *mut ID3D11Texture2D,
}

impl GraphicsD3D11 {
    unsafe fn new(hwnd: HWND) -> Result<Self, ()> {
        let mut result = GraphicsD3D11 {
            device: std::ptr::null_mut(),
            context: std::ptr::null_mut(),
            swapchain: std::ptr::null_mut(),
            backbuffer_rtv: std::ptr::null_mut(),
            backbuffer_tex: std::ptr::null_mut(),
        };

        let adapter: *mut IDXGIAdapter = std::ptr::null_mut();
        let device_flags: UINT = 0;

        let feature_levels: D3D_FEATURE_LEVEL = D3D_FEATURE_LEVEL_11_0;
        let num_feature_levels: UINT = 1;

        let swapchain_desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: 0,
                Height: 0,
                RefreshRate: DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                },
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
                ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
            },
            BufferCount: 1,
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            Flags: 0,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            SwapEffect: DXGI_SWAP_EFFECT_SEQUENTIAL,
            OutputWindow: hwnd,
            Windowed: 1,
        };

        let hr: HRESULT = D3D11CreateDeviceAndSwapChain(
            adapter,
            D3D_DRIVER_TYPE_HARDWARE,
            std::ptr::null_mut(),
            device_flags,
            &feature_levels,
            num_feature_levels,
            D3D11_SDK_VERSION,
            &swapchain_desc,
            &mut result.swapchain,
            &mut result.device,
            std::ptr::null_mut(),
            &mut result.context,
        );

        assert!(
            hr == winapi::shared::winerror::S_OK,
            "D3D11 device creation failed"
        );

        result.swapchain.as_ref().unwrap().GetBuffer(
            0,
            &ID3D11Texture2D::uuidof(),
            &mut result.backbuffer_tex as *mut *mut ID3D11Texture2D
                as *mut *mut winapi::ctypes::c_void,
        );

        result.device.as_ref().unwrap().CreateRenderTargetView(
            result.backbuffer_tex as *mut winapi::um::d3d11::ID3D11Resource,
            std::ptr::null_mut(),
            &mut result.backbuffer_rtv,
        );

        Ok(result)
    }
}

fn main() {
    let main_begin_time = Instant::now();

    let main_window: Window = create_window().unwrap();

    {
        let window_time = Instant::now() - main_begin_time;
        println!("Time to window: {}ms", window_time.as_secs_f32() * 1000.0);
    }

    let graphics: GraphicsD3D11 = unsafe { GraphicsD3D11::new(main_window.hwnd).unwrap() };

    // Delay showing the window until the first frame can be drawn to avoid showing default blank frame
    unsafe { 
        let hwnd = main_window.hwnd;
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }

    let mut should_exit = false;
    let mut frame_number = 0;
    while !should_exit {
        while let Some(x) = process_window_messages(&main_window) {
            match x {
                WindowMessages::WindowClosed => {
                    should_exit = true;
                }
                _ => {
                    panic!();
                }
            }
        }

        if frame_number == 0 {
            let init_time = Instant::now() - main_begin_time;
            println!("Init time: {}ms", init_time.as_secs_f32() * 1000.0);
        }

        let clear_color: [f32; 4] = [0.1, 0.2, 0.3, 1.0];

        unsafe { 
            let context = graphics.context.as_ref().unwrap();
            context.ClearRenderTargetView(graphics.backbuffer_rtv, &clear_color);
            graphics.swapchain.as_ref().unwrap().Present(1, 0);
        };

        frame_number += 1;
    }
}
