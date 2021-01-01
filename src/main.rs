#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(unused_imports)]

//use std::time::{Duration};
use cgmath::{assert_ulps_eq, prelude::*};
use com_ptr::{hresult, ComPtr};
use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::prelude::*;
use std::ptr::null_mut;
use std::time::Instant;
use std::{ffi::OsStr, path::Path, path::PathBuf};
use winapi::ctypes::c_void;
use winapi::shared::dxgi::*;
use winapi::shared::dxgi1_2::*;
use winapi::shared::dxgiformat::*;
use winapi::shared::dxgitype::*;
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::ntdef::{HRESULT, LPCWSTR};
use winapi::shared::windef::{HBRUSH, HICON, HMENU, HWND};
use winapi::shared::windowsx::{GET_X_LPARAM, GET_Y_LPARAM};
use winapi::shared::winerror::S_OK;
use winapi::um::d3d11::*;
use winapi::um::d3d11sdklayers::*;
use winapi::um::d3dcommon::*;
use winapi::um::shellscalingapi::SetProcessDpiAwareness;
use winapi::um::winuser::*;
use winapi::Interface;

mod math;
use math::*;

const NUM_BACK_BUFFERS: u32 = 2;
const BACK_BUFFER_FORMAT: u32 = DXGI_FORMAT_R8G8B8A8_UNORM;

const WINDOW_MIN_WIDTH: i32 = 320;
const WINDOW_MIN_HEIGHT: i32 = 240;

const DXGI_MWA_NO_WINDOW_CHANGES: UINT = 1;
const DXGI_MWA_NO_ALT_ENTER: UINT = 2;
const DXGI_MWA_NO_PRINT_SCREEN: UINT = 4;

// TODO: can we generate this based on shader reflection or inject into shader code from rust?
#[repr(C)]
#[derive(Clone)]
struct Constants {
    image_dim: float2,
    window_dim: float2,
    mouse: float4, // float2 xy pos, uint buttons, uint unused
    xfm_viewport_to_image_uv: float4,
}

struct WindowCreatedData {
    hwnd: HWND,
}

struct NativeMessageData {
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
}

struct OpenFileData {
    filename: OsString,
}

enum WindowMessages {
    WindowCreated(WindowCreatedData),
    WindowClosed,
    NativeMessage(NativeMessageData),
    OpenFile(OpenFileData),
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

fn get_screen_dimensions() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

fn get_window_client_rect_dimensions(hwnd: HWND) -> (i32, i32) {
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
        (client_rect.right - client_rect.left),
        (client_rect.bottom - client_rect.top)
    );
    dimensions
}

fn compute_client_rect(dim: (i32, i32)) -> winapi::shared::windef::RECT {
    let screen_dim = get_screen_dimensions();
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
    let window_state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowThreadState;
    let window_state = window_state_ptr.as_mut();

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
            let window_state = window_state.unwrap();
            window_state
                .message_tx
                .send(WindowMessages::WindowClosed)
                .unwrap();
            window_state.is_window_closed = true;
            PostQuitMessage(0);
        }
        WM_GETMINMAXINFO => {
            if let Some(mmi) = (lparam as LPMINMAXINFO).as_mut() {
                mmi.ptMinTrackSize.x = WINDOW_MIN_WIDTH;
                mmi.ptMinTrackSize.y = WINDOW_MIN_HEIGHT;
            }
            return 0;
        }
        WM_DROPFILES => {
            if let Some(window_state) = window_state {
                use winapi::um::shellapi::*;
                let hdrop = wparam as HDROP;
                let filename_len = DragQueryFileW(hdrop, 0, null_mut(), 0) as usize;
                if filename_len > 0 {
                    let mut filename_bytes = Vec::<u16>::with_capacity(filename_len + 1);
                    filename_bytes.set_len(filename_len + 1);
                    DragQueryFileW(
                        hdrop,
                        0,
                        filename_bytes.as_mut_ptr(),
                        filename_bytes.len() as u32,
                    );
                    filename_bytes.set_len(filename_bytes.len() - 1);
                    let filename = OsString::from_wide(&filename_bytes);
                    window_state
                        .message_tx
                        .send(WindowMessages::OpenFile(OpenFileData { filename }))
                        .unwrap();
                }
                DragFinish(hdrop);
            }
        }
        _ => {
            if let Some(window_state) = window_state {
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
                    SetProcessDpiAwareness(1);

                    let window_name: Vec<u16> = OsStr::new("imgv\0").encode_wide().collect();
                    let icon_name: Vec<u16> = OsStr::new("imgv\0").encode_wide().collect();
                    let window_class_name: Vec<u16> =
                        OsStr::new("imgv_window_class\0").encode_wide().collect();

                    let hinst = winapi::um::libloaderapi::GetModuleHandleW(null_mut());
                    let hicon: HICON = LoadIconW(hinst, icon_name.as_ptr());
                    assert!(hicon != (0 as HICON), "failed to load icon");

                    let window_class = WNDCLASSW {
                        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS | CS_OWNDC | CS_SAVEBITS,
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

struct BackBuffer {
    rtv: ComPtr<ID3D11RenderTargetView>,
    tex: ComPtr<ID3D11Texture2D>,
    dim: (u32, u32),
}

struct GraphicsD3D11 {
    device: ComPtr<ID3D11Device>,
    info_queue: Option<ComPtr<ID3D11InfoQueue>>,
    context: ComPtr<ID3D11DeviceContext>,
    swapchain: ComPtr<IDXGISwapChain1>,
    backbuffer: Option<BackBuffer>,
    blit_vs: ComPtr<ID3D11VertexShader>,
    blit_ps: ComPtr<ID3D11PixelShader>,
    constants: ComPtr<ID3D11Buffer>,
    smp_linear: ComPtr<ID3D11SamplerState>,
    smp_point: ComPtr<ID3D11SamplerState>,
}

impl GraphicsD3D11 {
    unsafe fn new(hwnd: HWND) -> Result<Self, ()> {
        let device_flags = 0u32 | { D3D11_CREATE_DEVICE_DEBUG * cfg!(debug_assertions) as u32 };

        let feature_levels: D3D_FEATURE_LEVEL = D3D_FEATURE_LEVEL_11_1;
        let num_feature_levels: UINT = 1;

        let swapchain_desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 0,
            Height: 0,
            Format: BACK_BUFFER_FORMAT,
            Stereo: 0,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: NUM_BACK_BUFFERS,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };

        let mut device: *mut ID3D11Device = null_mut();
        let mut context: *mut ID3D11DeviceContext = null_mut();

        let hr: HRESULT = D3D11CreateDevice(
            null_mut(),
            D3D_DRIVER_TYPE_HARDWARE,
            null_mut(),
            device_flags,
            &feature_levels,
            num_feature_levels,
            D3D11_SDK_VERSION,
            &mut device,
            null_mut(),
            &mut context,
        );
        assert!(hr == S_OK, "D3D11 device creation failed");
        let device = ComPtr::from_raw(device);

        let dxgi_device: ComPtr<IDXGIDevice1> = device
            .query_interface::<IDXGIDevice1>()
            .expect("Failed to aquire DXGI device");
        let dxgi_adapter: ComPtr<IDXGIAdapter> = ComPtr::new(|| {
            let mut obj: *mut IDXGIAdapter = null_mut();
            let hr: HRESULT = dxgi_device.GetAdapter(&mut obj);
            hresult(obj, hr)
        })
        .unwrap();
        let dxgi_factory: ComPtr<IDXGIFactory2> = ComPtr::new(|| {
            let mut obj: *mut IDXGIFactory2 = null_mut();
            let hr: HRESULT = dxgi_adapter.GetParent(
                &IDXGIFactory2::uuidof(),
                &mut obj as *mut *mut IDXGIFactory2 as _,
            );
            hresult(obj, hr)
        })
        .unwrap();

        let mut swapchain: *mut IDXGISwapChain1 = null_mut();
        let hr: HRESULT = dxgi_factory.CreateSwapChainForHwnd(
            device.as_ptr() as _,
            hwnd,
            &swapchain_desc,
            null_mut(),
            null_mut(),
            &mut swapchain,
        );
        assert!(hr == S_OK);

        dxgi_factory.MakeWindowAssociation(
            hwnd,
            DXGI_MWA_NO_WINDOW_CHANGES | DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_PRINT_SCREEN,
        );

        let swapchain = ComPtr::from_raw(swapchain);

        let mut info_queue: *mut ID3D11InfoQueue = null_mut();

        if (device_flags & D3D11_CREATE_DEVICE_DEBUG) != 0 {
            device.QueryInterface(
                &ID3D11InfoQueue::uuidof(),
                &mut info_queue as *mut *mut ID3D11InfoQueue as _,
            );
            if let Some(info_queue) = info_queue.as_ref() {
                println!("D3D debug layer active");
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_CORRUPTION, 1);
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_ERROR, 1);
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_WARNING, 1);
            }
        }

        let mut blit_vs = null_mut();
        let shader_blit_vs = include_bytes!(concat!(env!("OUT_DIR"), "/blit_vs.dxbc"));
        let hr: HRESULT = device.CreateVertexShader(
            shader_blit_vs.as_ptr() as *const c_void,
            shader_blit_vs.len(),
            null_mut(),
            &mut blit_vs as *mut *mut ID3D11VertexShader,
        );
        assert!(hr == S_OK);

        let mut blit_ps = null_mut();
        let shader_blit_ps = include_bytes!(concat!(env!("OUT_DIR"), "/blit_ps.dxbc"));
        let hr: HRESULT = device.CreatePixelShader(
            shader_blit_ps.as_ptr() as *const c_void,
            shader_blit_ps.len(),
            null_mut(),
            &mut blit_ps as *mut *mut ID3D11PixelShader,
        );
        assert!(hr == S_OK);

        let constants = ComPtr::new(|| {
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Constants>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                StructureByteStride: std::mem::size_of::<Constants>() as u32,
            };
            let mut obj = null_mut();
            let hr = device.CreateBuffer(&desc, std::ptr::null(), &mut obj);
            hresult(obj, hr)
        })
        .expect("Failed to create constant buffer");

        let mut smp_linear = null_mut();
        let mut smp_point = null_mut();

        {
            let smp_desc_base = D3D11_SAMPLER_DESC {
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

            {
                let smp_desc = smp_desc_base.clone();
                let hr = device.CreateSamplerState(&smp_desc, &mut smp_linear);
                assert!(hr == S_OK);
            }

            {
                let mut smp_desc = smp_desc_base.clone();
                smp_desc.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
                let hr = device.CreateSamplerState(&smp_desc, &mut smp_point);
                assert!(hr == S_OK);
            }
        }

        let mut result = GraphicsD3D11 {
            device,
            info_queue: if info_queue.is_null() {
                None
            } else {
                Some(ComPtr::from_raw(info_queue))
            },
            context: ComPtr::from_raw(context),
            swapchain,
            backbuffer: None,
            blit_vs: ComPtr::from_raw(blit_vs),
            blit_ps: ComPtr::from_raw(blit_ps),
            constants,
            smp_linear: ComPtr::from_raw(smp_linear),
            smp_point: ComPtr::from_raw(smp_point),
        };

        result.update_backbuffer(hwnd);

        Ok(result)
    }

    fn update_backbuffer(&mut self, hwnd: HWND) {
        let mut new_dim = get_window_client_rect_dimensions(hwnd);
        let screen_dim = get_screen_dimensions();
        new_dim.0 = new_dim.0.max(screen_dim.0);
        new_dim.1 = new_dim.1.max(screen_dim.1);

        if let Some(backbuffer) = &self.backbuffer {
            if backbuffer.dim.0 as i32 >= new_dim.0 && backbuffer.dim.1 as i32 >= new_dim.1 {
                return;
            }
        }

        // Release old render target view before resizing back buffer
        self.backbuffer = None;

        assert!(new_dim.0 < 8192);
        assert!(new_dim.1 < 8192);

        println!("update_backbuffer {:?}", new_dim);

        let hr: HRESULT = unsafe {
            self.swapchain.ResizeBuffers(
                NUM_BACK_BUFFERS,
                new_dim.0 as u32,
                new_dim.1 as u32,
                BACK_BUFFER_FORMAT,
                0,
            )
        };
        assert!(hr == S_OK);

        let mut tex: *mut ID3D11Texture2D = null_mut();
        let mut rtv: *mut ID3D11RenderTargetView = null_mut();

        unsafe {
            self.swapchain.GetBuffer(
                0,
                &ID3D11Texture2D::uuidof(),
                &mut tex as *mut *mut ID3D11Texture2D as _,
            );
            self.device
                .CreateRenderTargetView(tex as _, null_mut(), &mut rtv);
        }

        self.backbuffer = Some(BackBuffer {
            tex: unsafe { ComPtr::from_raw(tex) },
            rtv: unsafe { ComPtr::from_raw(rtv) },
            dim: (new_dim.0 as u32, new_dim.1 as u32),
        });
    }
}

fn decode_mouse_pos(lparam: isize) -> float2 {
    let x = GET_X_LPARAM(lparam) as f32;
    let y = GET_Y_LPARAM(lparam) as f32;
    float2 { x, y }
}

#[derive(Debug, PartialEq)]
enum StepDirection {
    Backward,
    Forward,
}

fn is_compatible_file(path: &Path) -> bool {
    let extensions = [
        "jpg", "jpeg", "png", "gif", "webp", "tif", "tiff", "tga", "dds", "bmp", "ico", "hdr",
        "pbm", "pam", "ppm", "pgm", "ff",
    ];
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_ascii_lowercase();
        for it in &extensions {
            if *it == ext {
                return true;
            }
        }
    }
    false
}

fn get_next_file(path: &Path, direction: StepDirection) -> Option<PathBuf> {
    let file_dir = path.parent().unwrap();
    let file_name = path.file_name().unwrap();
    let dir = std::fs::read_dir(file_dir);
    if let Ok(dir) = dir {
        let files: Vec<_> = dir
            .filter_map(|f| {
                if f.is_ok() {
                    Some(f.unwrap().path())
                } else {
                    None
                }
            })
            .filter(|f| is_compatible_file(f))
            .map(|f| f.file_name().unwrap().to_owned())
            .collect();
        if let Some(i) = files.iter().position(|f| f == file_name) {
            return match direction {
                StepDirection::Backward if i > 0 => Some(files[i - 1].clone().into()),
                StepDirection::Forward if i + 1 < files.len() => Some(files[i + 1].clone().into()),
                _ => None,
            };
        }
    }
    None
}
struct Texture {
    tex: ComPtr<ID3D11Texture2D>,
    srv: ComPtr<ID3D11ShaderResourceView>,
    dim: (u32, u32),
}

impl Texture {
    fn new(device: &ComPtr<ID3D11Device>, image: image::DynamicImage) -> Self {
        let mut image_tex: *mut ID3D11Texture2D = null_mut();
        let mut image_srv: *mut ID3D11ShaderResourceView = null_mut();
        let img_buf = image.into_rgba8();
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
        Self {
            tex: unsafe { ComPtr::from_raw(image_tex) },
            srv: unsafe { ComPtr::from_raw(image_srv) },
            dim,
        }
    }
}

fn main() {
    let main_begin_time = Instant::now();

    let mut image_path: Option<PathBuf> = None;

    let (load_req_tx, load_req_rx) = std::sync::mpsc::channel();
    let (image_tx, image_rx) = std::sync::mpsc::channel();

    if std::env::args().len() > 1 {
        let args: Vec<String> = std::env::args().collect();
        let path: PathBuf = args[1].clone().into();
        image_path = Some(path.clone());
        load_req_tx.send(path).unwrap();
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
        image_dim: FLOAT2_ZERO,
        window_dim: FLOAT2_ZERO,
        mouse: FLOAT4_ZERO,
        xfm_viewport_to_image_uv: Transform2D::new_identity().into(),
    };

    let mut texture = None;
    let mut is_resizing = false;
    let mut pending_window_dim = FLOAT2_ONE;
    let mut is_dragging = false;
    let mut drag_origin = FLOAT2_ZERO;
    let mut mouse_pos = FLOAT2_ZERO;
    let mut should_draw = true;
    let mut should_block = true;
    let mut xfm_window_to_image = Transform2D::new_identity();
    let mut handled_events = 0;
    let mut last_frame_draw_time = Instant::now();

    let switch_to_next_image = |current_image_path: &Path, direction: StepDirection| {
        let file_name = get_next_file(current_image_path, direction);
        let file_dir = current_image_path.parent();
        if file_name.is_some() && file_dir.is_some() {
            let file_name = file_name.unwrap();
            let file_dir = file_dir.unwrap().to_path_buf();
            let path = file_dir.join(file_name);
            load_req_tx.send(path.clone()).unwrap();
            Some(path)
        } else {
            Some(current_image_path.into())
        }
    };

    while !should_exit {
        if let Some(x) = process_window_messages(&main_window, should_block) {
            should_block = false;
            match x {
                WindowMessages::OpenFile(data) => {
                    image_path = Some(data.filename.clone().into());
                    load_req_tx.send(data.filename.into()).unwrap();
                }
                WindowMessages::WindowClosed => {
                    should_exit = true;
                }
                WindowMessages::NativeMessage(native_msg) => {
                    let lparam = native_msg.lparam;
                    let wparam = native_msg.wparam;
                    match native_msg.msg {
                        WM_PAINT => {
                            should_draw = true;
                        }
                        WM_MOUSEWHEEL => {
                            let scroll_delta = GET_WHEEL_DELTA_WPARAM(wparam);
                            let zoom = if scroll_delta > 0 {
                                float2::new(0.8, 0.8)
                            } else {
                                float2::new(1.2, 1.2)
                            };
                            let mouse_pos_img = xfm_window_to_image.transform_point(mouse_pos);
                            let zoom_transform = Transform2D::new_translate(-mouse_pos_img)
                                .concatenate(Transform2D::new_scale(zoom))
                                .concatenate(Transform2D::new_translate(mouse_pos_img));
                            xfm_window_to_image.inplace_concatenate(zoom_transform);
                            should_draw = true;
                            is_dragging = false;
                        }
                        WM_LBUTTONDOWN => {
                            is_dragging = true;
                            drag_origin = mouse_pos - xfm_window_to_image.inverse().offset;
                        }
                        WM_LBUTTONUP => {
                            is_dragging = false;
                        }
                        WM_XBUTTONDOWN | WM_XBUTTONDBLCLK => {
                            let button_index = winapi::shared::minwindef::HIWORD(wparam as u32);
                            if let Some(image_path_local) = &image_path {
                                match button_index {
                                    1 => {
                                        image_path = switch_to_next_image(
                                            image_path_local,
                                            StepDirection::Backward,
                                        );
                                    }
                                    2 => {
                                        image_path = switch_to_next_image(
                                            image_path_local,
                                            StepDirection::Forward,
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                        WM_XBUTTONUP => {
                            // println!("WM_XBUTTONUP");
                        }
                        WM_MOUSEMOVE => {
                            mouse_pos = decode_mouse_pos(lparam);
                            constants.mouse.x = mouse_pos.x;
                            constants.mouse.y = mouse_pos.y;
                            let drag_delta: float2 = drag_origin - mouse_pos;
                            if is_dragging {
                                xfm_window_to_image.offset =
                                    drag_delta.mul_element_wise(xfm_window_to_image.scale);
                                should_draw = true;
                            }
                        }
                        WM_KEYDOWN => {
                            match (wparam as i32, wparam as u8 as char) {
                                (VK_ESCAPE, _) => {
                                    should_exit = true;
                                }
                                (VK_HOME, _) => {
                                    xfm_window_to_image = Transform2D::new_identity();
                                }
                                (VK_LEFT, _) if image_path.is_some() => {
                                    image_path = switch_to_next_image(
                                        &image_path.unwrap(),
                                        StepDirection::Backward,
                                    );
                                }
                                (VK_RIGHT, _) if image_path.is_some() => {
                                    image_path = switch_to_next_image(
                                        &image_path.unwrap(),
                                        StepDirection::Forward,
                                    );
                                }
                                (_, '1') => {
                                    let s = 1.0;
                                    xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '2') => {
                                    let s = 1.0 / 2.0;
                                    xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '3') => {
                                    let s = 1.0 / 4.0;
                                    xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '4') => {
                                    let s = 1.0 / 8.0;
                                    xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '5') => {
                                    let s = 1.0 / 16.0;
                                    xfm_window_to_image.scale = float2::new(s, s);
                                }
                                _ => {}
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
                                    constants.window_dim = pending_window_dim.clone();
                                    graphics.update_backbuffer(main_window.hwnd);
                                }
                            }
                            should_draw = true;
                        }
                        WM_ENTERSIZEMOVE => {
                            is_resizing = true;
                        }
                        WM_EXITSIZEMOVE => {
                            constants.window_dim = pending_window_dim.clone();
                            graphics.update_backbuffer(main_window.hwnd);
                            should_draw = true;
                            is_resizing = false;
                        }
                        _ => {
                            // println!("msg: {}", native_msg.msg);
                        }
                    }

                    if should_draw {
                        handled_events += 1;
                    }
                }
                _ => {
                    panic!("unhandled windows message type");
                }
            }
        } else {
            should_block = true;
        }

        if !should_draw || !should_block {
            continue;
        }

        let xfm_viewport_to_image_uv = Transform2D {
            scale: 1.0 / constants.window_dim,
            offset: FLOAT2_ZERO,
        };

        constants.xfm_viewport_to_image_uv = xfm_window_to_image
            .concatenate(xfm_viewport_to_image_uv)
            .into();

        if let Ok(img) = image_rx.try_recv() {
            if let Ok(img) = img {
                texture = Some(Texture::new(&graphics.device, img));

                let dim = texture.as_ref().unwrap().dim;

                constants.image_dim.x = dim.0 as f32;
                constants.image_dim.y = dim.1 as f32;

                let image_load_time = Instant::now() - main_begin_time;
                println!(
                    "Time to load image {} ({:?})",
                    image_load_time.as_secs_f32() * 1000.0,
                    dim
                );
                main_window.set_image_size((dim.0 as i32, dim.1 as i32));
                xfm_window_to_image = Transform2D::new_identity();
            } else {
                println!("Failed to load image: {:?}", img.err());
            };

            unsafe {
                InvalidateRect(main_window_handle as HWND, null_mut(), 1);
            }
        }

        if frame_number == 0 {
            let init_time = Instant::now() - main_begin_time;
            println!("Init time: {:.2}ms", init_time.as_secs_f32() * 1000.0);
        }

        unsafe {
            let frame_delta_time = Instant::now() - last_frame_draw_time;
            last_frame_draw_time = Instant::now();
            println!(
                "Draw dt: {:.2}ms, frame_number: {}, handled_events: {}",
                frame_delta_time.as_secs_f32() * 1000.0,
                frame_number,
                handled_events
            );

            let context = &graphics.context;

            context.UpdateSubresource(
                graphics.constants.as_ptr() as _,
                0,
                null_mut(),
                &constants as *const Constants as _,
                0,
                0,
            );

            let backbuffer = graphics
                .backbuffer
                .as_ref()
                .expect("Back buffer must be created before rendering a frame");

            let rtvs: [*mut ID3D11RenderTargetView; 1] = [backbuffer.rtv.as_ptr()];
            context.OMSetRenderTargets(1, rtvs.as_ptr(), null_mut());

            let viewport = D3D11_VIEWPORT {
                Width: backbuffer.dim.0 as f32,
                Height: backbuffer.dim.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
                TopLeftX: 0.0,
                TopLeftY: 0.0,
            };
            context.RSSetViewports(1, &viewport);

            let clear_color: [f32; 4] = [0.1, 0.2, 0.3, 1.0];
            context.ClearRenderTargetView(backbuffer.rtv.as_ptr(), &clear_color);

            let cbvs: [*mut ID3D11Buffer; 1] = [graphics.constants.as_ptr()];
            let srvs: [*mut ID3D11ShaderResourceView; 1] = [if let Some(texture) = &texture {
                texture.srv.as_ptr()
            } else {
                null_mut()
            }];
            let samplers: [*mut ID3D11SamplerState; 3] = [
                graphics.smp_linear.as_ptr(), // g_default_sampler
                graphics.smp_linear.as_ptr(), // g_linear_sampler
                graphics.smp_point.as_ptr(),  // g_point_sampler
            ];

            context.PSSetConstantBuffers(0, cbvs.len() as u32, cbvs.as_ptr());
            context.VSSetConstantBuffers(0, cbvs.len() as u32, cbvs.as_ptr());

            context.VSSetShader(graphics.blit_vs.as_ptr(), null_mut(), 0);
            context.PSSetShader(graphics.blit_ps.as_ptr(), null_mut(), 0);

            context.VSSetShaderResources(0, srvs.len() as u32, srvs.as_ptr());
            context.PSSetShaderResources(0, srvs.len() as u32, srvs.as_ptr());
            context.PSSetSamplers(0, samplers.len() as u32, samplers.as_ptr());

            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.Draw(3, 0);

            context.ClearState();

            graphics.swapchain.Present(0, 0);
        };

        frame_number += 1;
        should_draw = false;
        handled_events = 0;
    }
}
