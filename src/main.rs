#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsStr;
use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::prelude::*;
use std::ptr::null_mut;
use std::time::Instant;
use winapi::ctypes::c_void;
use winapi::shared::dxgi::*;
use winapi::shared::dxgiformat::*;
use winapi::shared::dxgitype::*;
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::ntdef::{HRESULT, LPCWSTR};
use winapi::shared::windef::{HBRUSH, HICON, HMENU, HWND};
use winapi::shared::windowsx::{GET_X_LPARAM, GET_Y_LPARAM};
use winapi::um::d3d11::*;
use winapi::um::d3d11sdklayers::*;
use winapi::um::d3dcommon::*;
use winapi::um::shellscalingapi::SetProcessDpiAwareness;
use winapi::um::winuser::*;
use winapi::Interface;
//use std::time::{Duration};

const NUM_BACK_BUFFERS: u32 = 2;
const WINDOW_MIN_WIDTH: i32 = 200;
const WINDOW_MIN_HEIGHT: i32 = 150;

#[repr(C)]
#[derive(Clone)]
struct float2 {
    x: f32,
    y: f32,
}

#[repr(C)]
#[derive(Clone)]
struct float4 {
    x: f32,
    y: f32,
    z: f32,
    w: f32,
}

// TODO: can we generate this based on shader reflection or inject into shader code from rust?
#[repr(C)]
#[derive(Clone)]
struct Constants {
    image_dim: float2,
    window_dim: float2,
    mouse: float4, // float2 xy pos, uint buttons, uint unused
}

struct WindowCreatedData {
    hwnd: HWND,
}

struct NativeMessageData {
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
}

enum WindowMessages {
    WindowCreated(WindowCreatedData),
    WindowClosed,
    NativeMessage(NativeMessageData),
}

unsafe impl std::marker::Send for WindowCreatedData {}

struct Window {
    message_rx: std::sync::mpsc::Receiver<WindowMessages>,
    hwnd: HWND,
    window_style: u32,
}

struct WindowThreadState {
    message_tx: std::sync::mpsc::Sender<WindowMessages>,
    is_window_closed: bool,
}

fn get_window_client_rect_dimensions(hwnd: HWND) -> (u32, u32) {
    let mut client_rect = winapi::shared::windef::RECT {
        left: 0,
        right: 0,
        top: 0,
        bottom: 0,
    };
    unsafe {
        GetClientRect(hwnd, &mut client_rect);
    }
    let dimensions = (
        (client_rect.right - client_rect.left) as u32,
        (client_rect.bottom - client_rect.top) as u32,
    );
    dimensions
}

fn compute_client_rect(dim: (i32, i32)) -> winapi::shared::windef::RECT {
    let screen_dim = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let window_pos = (screen_dim.0 / 2 - dim.0 / 2, screen_dim.1 / 2 - dim.1 / 2);
    winapi::shared::windef::RECT {
        left: window_pos.0,
        top: window_pos.1,
        right: window_pos.0 + dim.0,
        bottom: window_pos.1 + dim.1,
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let create_struct = lparam as *mut winapi::um::winuser::CREATESTRUCTW;
            let window_state_ptr =
                create_struct.as_ref().unwrap().lpCreateParams as *mut WindowThreadState;
            let window_state: &mut WindowThreadState = window_state_ptr.as_mut().unwrap();
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, window_state_ptr as isize);
            window_state
                .message_tx
                .send(WindowMessages::WindowCreated(WindowCreatedData { hwnd }))
                .unwrap();
        }
        WM_DESTROY => {
            let window_state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowThreadState;
            let window_state: &mut WindowThreadState = window_state_ptr.as_mut().unwrap();
            window_state
                .message_tx
                .send(WindowMessages::WindowClosed)
                .unwrap();
            window_state.is_window_closed = true;
            PostQuitMessage(0);
        }
        _ => {
            let window_state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowThreadState;
            if !window_state_ptr.is_null() {
                let window_state: &mut WindowThreadState = window_state_ptr.as_mut().unwrap();
                window_state
                    .message_tx
                    .send(WindowMessages::NativeMessage(NativeMessageData {
                        msg,
                        wparam,
                        lparam,
                    }))
                    .unwrap();
            }
        }
    };

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

impl Window {
    fn new(window_dim: (i32, i32)) -> Result<Window, ()> {
        let (channel_sender, channel_receiver) = std::sync::mpsc::channel();

        let window_style: u32 =
            WS_MAXIMIZEBOX | WS_MINIMIZEBOX | WS_SYSMENU | WS_SIZEBOX | WS_CAPTION;

        std::thread::Builder::new()
            .name("window".to_owned())
            .spawn(move || {
                let mut window_state = WindowThreadState {
                    message_tx: channel_sender,
                    is_window_closed: false,
                };

                unsafe {
                    let window_name: Vec<u16> = OsStr::new("imgv\0").encode_wide().collect();
                    let icon_name: Vec<u16> = OsStr::new("imgv\0").encode_wide().collect();
                    let window_class_name: Vec<u16> =
                        OsStr::new("imgv_window_class\0").encode_wide().collect();

                    let hinst = winapi::um::libloaderapi::GetModuleHandleW(null_mut());
                    let hicon: HICON = LoadIconW(hinst, icon_name.as_ptr());
                    assert!(hicon != (0 as HICON), "failed to load icon");

                    let window_class = WNDCLASSW {
                        style: 0, //CS_DBLCLKS | CS_OWNDC | CS_HREDRAW | CS_VREDRAW,
                        lpfnWndProc: Some(window_proc),
                        cbClsExtra: 0,
                        cbWndExtra: 0,
                        hInstance: hinst,
                        hIcon: hicon,
                        hCursor: LoadCursorW(null_mut(), IDC_ARROW) as HICON,
                        hbrBackground: 16 as HBRUSH,
                        lpszMenuName: 0 as LPCWSTR,
                        lpszClassName: window_class_name.as_ptr(),
                    };

                    let error_code = RegisterClassW(&window_class);

                    assert!(error_code != 0, "failed to register the window class");

                    let mut client_rect = compute_client_rect(window_dim);

                    AdjustWindowRect(&mut client_rect, window_style, 0);

                    let hwnd = CreateWindowExW(
                        0,
                        window_class_name.as_ptr(),
                        window_name.as_ptr(),
                        window_style,
                        client_rect.left,
                        client_rect.top,
                        client_rect.right - client_rect.left,
                        client_rect.bottom - client_rect.top,
                        0 as HWND,
                        0 as HMENU,
                        hinst,
                        &mut window_state as *mut WindowThreadState as _,
                    );
                    assert!(hwnd != (0 as HWND), "failed to open the window");

                    winapi::um::shellapi::DragAcceptFiles(hwnd, 1);

                    // Delay showing this window until D3D is ready to draw something
                    // ShowWindow(hwnd, SW_SHOW);

                    let mut msg: MSG = std::mem::zeroed();

                    while !window_state.is_window_closed {
                        if GetMessageW(&mut msg, hwnd, 0, 0) > 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
            })
            .unwrap();

        if let WindowMessages::WindowCreated(data) = channel_receiver.recv().unwrap() {
            return Ok(Window {
                message_rx: channel_receiver,
                hwnd: data.hwnd,
                window_style,
            });
        }

        Err(())
    }

    pub fn set_image_size(&mut self, dim: (i32, i32)) {
        let mut rect = compute_client_rect(dim);
        unsafe {
            AdjustWindowRect(&mut rect, self.window_style, 0);
            SetWindowPos(
                self.hwnd,
                null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                0,
            );
        }
    }
}

fn process_window_messages(window: &Window, should_block: bool) -> Option<WindowMessages> {
    if should_block {
        if let Ok(x) = window.message_rx.recv() {
            return Some(x);
        }
    } else {
        if let Ok(x) = window.message_rx.try_recv() {
            return Some(x);
        }
    }
    None
}

struct GraphicsD3D11 {
    device: *mut ID3D11Device,
    info_queue: *mut ID3D11InfoQueue,
    context: *mut ID3D11DeviceContext,
    swapchain: *mut IDXGISwapChain,
    backbuffer_rtv: *mut ID3D11RenderTargetView,
    backbuffer_tex: *mut ID3D11Texture2D,
    backbuffer_dim: (u32, u32),
    blit_vs: *mut ID3D11VertexShader,
    blit_ps: *mut ID3D11PixelShader,
    constants: *mut ID3D11Buffer,
    smp_linear: *mut ID3D11SamplerState,
}

impl GraphicsD3D11 {
    unsafe fn new(hwnd: HWND) -> Result<Self, ()> {
        let mut result = GraphicsD3D11 {
            device: null_mut(),
            info_queue: null_mut(),
            context: null_mut(),
            swapchain: null_mut(),
            backbuffer_rtv: null_mut(),
            backbuffer_tex: null_mut(),
            backbuffer_dim: (0, 0),
            blit_vs: null_mut(),
            blit_ps: null_mut(),
            constants: null_mut(),
            smp_linear: null_mut(),
        };

        let adapter: *mut IDXGIAdapter = null_mut();

        let device_flags = D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS | {
            D3D11_CREATE_DEVICE_DEBUG * cfg!(debug_assertions) as u32
        };

        let feature_levels: D3D_FEATURE_LEVEL = D3D_FEATURE_LEVEL_11_1;
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
            BufferCount: NUM_BACK_BUFFERS,
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            Flags: 0,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            OutputWindow: hwnd,
            Windowed: 1,
        };

        let hr: HRESULT = D3D11CreateDeviceAndSwapChain(
            adapter,
            D3D_DRIVER_TYPE_HARDWARE,
            null_mut(),
            device_flags,
            &feature_levels,
            num_feature_levels,
            D3D11_SDK_VERSION,
            &swapchain_desc,
            &mut result.swapchain,
            &mut result.device,
            null_mut(),
            &mut result.context,
        );

        assert!(
            hr == winapi::shared::winerror::S_OK,
            "D3D11 device creation failed"
        );

        let device = result.device.as_ref().unwrap();

        if (device_flags & D3D11_CREATE_DEVICE_DEBUG) != 0 {
            device.QueryInterface(
                &ID3D11InfoQueue::uuidof(),
                &mut result.info_queue as *mut *mut ID3D11InfoQueue as _,
            );
            if let Some(info_queue) = result.info_queue.as_ref() {
                println!("D3D debug layer active");
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_CORRUPTION, 1);
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_ERROR, 1);
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_WARNING, 1);
            }
        }

        let shader_blit_vs = include_bytes!(concat!(env!("OUT_DIR"), "/blit_vs.dxbc"));
        let hr: HRESULT = device.CreateVertexShader(
            shader_blit_vs.as_ptr() as *const c_void,
            shader_blit_vs.len(),
            null_mut(),
            &mut result.blit_vs as *mut *mut ID3D11VertexShader,
        );
        assert!(hr == winapi::shared::winerror::S_OK);

        let shader_blit_ps = include_bytes!(concat!(env!("OUT_DIR"), "/blit_ps.dxbc"));
        let hr: HRESULT = device.CreatePixelShader(
            shader_blit_ps.as_ptr() as *const c_void,
            shader_blit_ps.len(),
            null_mut(),
            &mut result.blit_ps as *mut *mut ID3D11PixelShader,
        );
        assert!(hr == winapi::shared::winerror::S_OK);

        {
            let buffer_desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Constants>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                StructureByteStride: std::mem::size_of::<Constants>() as u32,
            };
            let hr = device.CreateBuffer(&buffer_desc, std::ptr::null(), &mut result.constants);
            assert!(hr == winapi::shared::winerror::S_OK);
        }

        {
            let smp_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                BorderColor: [1.0, 1.0, 1.0, 1.0],
                MinLOD: -D3D11_FLOAT32_MAX,
                MaxLOD: D3D11_FLOAT32_MAX,
            };
            let hr = device.CreateSamplerState(&smp_desc, &mut result.smp_linear);
            assert!(hr == winapi::shared::winerror::S_OK);
        }

        result.update_backbuffer(hwnd);

        Ok(result)
    }

    unsafe fn update_backbuffer(&mut self, hwnd: HWND) {
        let new_dim = get_window_client_rect_dimensions(hwnd);
        if self.backbuffer_dim != new_dim {
            assert!(new_dim.0 < 8192);
            assert!(new_dim.1 < 8192);

            println!("update_backbuffer {:?}", new_dim);

            let swapchain = self.swapchain.as_ref().unwrap();

            if self.backbuffer_dim != (0, 0) {
                self.backbuffer_tex.as_ref().unwrap().Release();
                self.backbuffer_rtv.as_ref().unwrap().Release();
            }

            let hr: HRESULT = swapchain.ResizeBuffers(
                NUM_BACK_BUFFERS,
                new_dim.0,
                new_dim.1,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                0,
            );
            assert!(hr == winapi::shared::winerror::S_OK);

            self.swapchain.as_ref().unwrap().GetBuffer(
                0,
                &ID3D11Texture2D::uuidof(),
                &mut self.backbuffer_tex as *mut *mut ID3D11Texture2D as _,
            );

            self.device.as_ref().unwrap().CreateRenderTargetView(
                self.backbuffer_tex as *mut ID3D11Resource,
                null_mut(),
                &mut self.backbuffer_rtv,
            );

            self.backbuffer_dim = new_dim;
        }
    }
}

fn main() {
    let main_begin_time = Instant::now();

    unsafe { SetProcessDpiAwareness(1) };

    let (load_req_tx, load_req_rx) = std::sync::mpsc::channel();
    let (image_tx, image_rx) = std::sync::mpsc::channel();

    if std::env::args().len() > 1 {
        let args: Vec<String> = std::env::args().collect();
        let filename: OsString = args[1].clone().into();
        let _ = load_req_tx.send(filename);
    }

    let mut main_window: Window = Window::new((500, 500)).unwrap();
    let main_window_handle = main_window.hwnd as u64;
    std::thread::spawn(move || {
        while let Ok(x) = load_req_rx.recv() {
            println!("Loading image {:?}", x);
            let img = image::open(x);
            let _ = image_tx.send(img);
            unsafe {
                InvalidateRect(main_window_handle as HWND, null_mut(), 1);
            }
        }
        println!("Loading thread done");
    });

    {
        let window_time = Instant::now() - main_begin_time;
        println!("Time to window: {}ms", window_time.as_secs_f32() * 1000.0);
    }

    let mut graphics: GraphicsD3D11 = unsafe { GraphicsD3D11::new(main_window.hwnd).unwrap() };

    // Delay showing the window until the first frame can be drawn to avoid showing default blank frame
    unsafe {
        let hwnd = main_window.hwnd;
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }

    let mut should_exit = false;
    let mut frame_number = 0;

    let mut constants = Constants {
        image_dim: float2 { x: 0.0, y: 0.0 },
        window_dim: float2 { x: 0.0, y: 0.0 },
        mouse: float4 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 0.0,
        },
    };

    let mut image_tex: *mut ID3D11Texture2D = null_mut();
    let mut image_srv: *mut ID3D11ShaderResourceView = null_mut();

    let mut is_resizing = false;

    let mut should_block = true;

    let mut pending_window_dim = float2 { x: 1.0, y: 1.0 };

    while !should_exit {
        let mut should_draw = false;
        if let Some(x) = process_window_messages(&main_window, should_block) {
            should_block = false;
            match x {
                WindowMessages::WindowClosed => {
                    should_exit = true;
                }
                WindowMessages::NativeMessage(native_msg) => {
                    let lparam = native_msg.lparam;
                    let wparam = native_msg.wparam;
                    match native_msg.msg {
                        WM_GETMINMAXINFO => unsafe {
                            if let Some(mmi) = (lparam as LPMINMAXINFO).as_mut(){
                                mmi.ptMinTrackSize.x = WINDOW_MIN_WIDTH;
                                mmi.ptMinTrackSize.y = WINDOW_MIN_HEIGHT;
                            }
                        }
                        WM_PAINT => {
                            should_draw = true;
                        }
                        WM_MOUSEWHEEL => {
                            let _mx = GET_X_LPARAM(lparam) as f32;
                            let _my = GET_Y_LPARAM(lparam) as f32;
                            let zdelta = GET_WHEEL_DELTA_WPARAM(wparam);
                            println!("zdelta: {}", zdelta);
                        }
                        WM_MOUSEMOVE => {
                            let mx = GET_X_LPARAM(lparam) as f32;
                            let my = GET_Y_LPARAM(lparam) as f32;
                            constants.mouse.x = mx;
                            constants.mouse.y = my;
                            should_draw = true;
                        }
                        WM_KEYDOWN => {
                            let key = wparam as i32;
                            if key == VK_ESCAPE {
                                should_exit = true;
                            }
                            should_draw = true;
                        }
                        WM_SIZE => {
                            let width = winapi::shared::minwindef::LOWORD(lparam as u32);
                            let height = winapi::shared::minwindef::HIWORD(lparam as u32);
                            pending_window_dim.x = width as f32;
                            pending_window_dim.y = height as f32;
                            if wparam == SIZE_MAXIMIZED || wparam == SIZE_RESTORED {
                                if !is_resizing {
                                    constants.window_dim = pending_window_dim.to_owned();
                                    unsafe { graphics.update_backbuffer(main_window.hwnd) };
                                }
                            }
                            should_draw = true;
                        }
                        WM_ENTERSIZEMOVE => {
                            is_resizing = true;
                        }
                        WM_EXITSIZEMOVE => {
                            constants.window_dim = pending_window_dim.to_owned();
                            unsafe { graphics.update_backbuffer(main_window.hwnd) };
                            should_draw = true;
                            is_resizing = false;
                        }
                        WM_DROPFILES => {
                            use winapi::um::shellapi::*;
                            let hdrop = wparam as HDROP;
                            unsafe {
                                let filename_len = DragQueryFileW(hdrop, 0, null_mut(), 0) as usize;
                                if filename_len > 0 {
                                    let mut filename_bytes =
                                        Vec::<u16>::with_capacity(filename_len + 1);
                                    filename_bytes.set_len(filename_len + 1);
                                    DragQueryFileW(
                                        hdrop,
                                        0,
                                        filename_bytes.as_mut_ptr(),
                                        filename_bytes.len() as u32,
                                    );
                                    filename_bytes.set_len(filename_bytes.len() - 1);
                                    let filename = OsString::from_wide(&filename_bytes);
                                    let _ = load_req_tx.send(filename);
                                }
                                DragFinish(hdrop);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {
                    panic!("unhandled windows message type");
                }
            }
        } else {
            should_block = true;
        }

        if !should_draw {
            continue;
        }

        let device = unsafe { graphics.device.as_ref().unwrap() };
        if let Ok(img) = image_rx.try_recv() {
            if let Ok(img) = img {
                let img_buf = img.into_rgba8();
                let dim = img_buf.dimensions();
                let img_container = img_buf.as_raw();
                let texture_desc = D3D11_TEXTURE2D_DESC {
                    Width: dim.0,
                    Height: dim.1,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let image_data = D3D11_SUBRESOURCE_DATA {
                    pSysMem: img_container.as_ptr() as *mut c_void,
                    SysMemPitch: 4 * texture_desc.Width,
                    SysMemSlicePitch: 0,
                };
                unsafe {
                    device.CreateTexture2D(
                        &texture_desc as *const D3D11_TEXTURE2D_DESC,
                        &image_data as *const D3D11_SUBRESOURCE_DATA,
                        &mut image_tex as *mut *mut ID3D11Texture2D,
                    );
                    device.CreateShaderResourceView(
                        image_tex as *mut ID3D11Resource,
                        null_mut(),
                        &mut image_srv as *mut *mut ID3D11ShaderResourceView,
                    );
                };
                let image_load_time = Instant::now() - main_begin_time;
                println!(
                    "Time to load image {} ({:?})",
                    image_load_time.as_secs_f32() * 1000.0,
                    dim
                );
                main_window.set_image_size((dim.0 as i32, dim.1 as i32));
            } else {
                println!("Failed to load image");
            };
        }

        if frame_number == 0 {
            let init_time = Instant::now() - main_begin_time;
            println!("Init time: {}ms", init_time.as_secs_f32() * 1000.0);
        }

        unsafe {
            let context = graphics.context.as_ref().unwrap();

            context.UpdateSubresource(
                graphics.constants as _,
                0,
                null_mut(),
                &constants as *const Constants as _,
                0,
                0,
            );

            let rtvs: [*mut ID3D11RenderTargetView; 1] = [graphics.backbuffer_rtv];
            context.OMSetRenderTargets(1, rtvs.as_ptr(), null_mut());

            let viewport: D3D11_VIEWPORT = D3D11_VIEWPORT {
                Width: graphics.backbuffer_dim.0 as f32,
                Height: graphics.backbuffer_dim.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
                TopLeftX: 0.0,
                TopLeftY: 0.0,
            };
            context.RSSetViewports(1, &viewport);

            let clear_color: [f32; 4] = [0.1, 0.2, 0.3, 1.0];
            context.ClearRenderTargetView(graphics.backbuffer_rtv, &clear_color);

            let cbvs: [*mut ID3D11Buffer; 1] = [graphics.constants];
            let srvs: [*mut ID3D11ShaderResourceView; 1] = [image_srv];
            let samplers: [*mut ID3D11SamplerState; 1] = [graphics.smp_linear];

            context.PSSetConstantBuffers(0, cbvs.len() as u32, cbvs.as_ptr());
            context.VSSetConstantBuffers(0, cbvs.len() as u32, cbvs.as_ptr());

            context.VSSetShader(graphics.blit_vs, null_mut(), 0);
            context.PSSetShader(graphics.blit_ps, null_mut(), 0);

            context.VSSetShaderResources(0, srvs.len() as u32, srvs.as_ptr());
            context.PSSetShaderResources(0, srvs.len() as u32, srvs.as_ptr());
            context.PSSetSamplers(0, samplers.len() as u32, samplers.as_ptr());

            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.Draw(3, 0);

            context.ClearState();

            graphics.swapchain.as_ref().unwrap().Present(0, 0);
        };

        frame_number += 1;
    }
}
