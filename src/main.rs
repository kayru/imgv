#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(unused_imports)]

use cgmath::{assert_ulps_eq, prelude::*};
use com_ptr::{hresult, ComPtr};
use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::prelude::*;
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use std::{ffi::OsStr, path::Path, path::PathBuf};
use winapi::ctypes::c_void;
use winapi::shared::dxgi::*;
use winapi::shared::dxgi1_2::*;
use winapi::shared::dxgi1_3::*;
use winapi::shared::dxgiformat::*;
use winapi::shared::dxgitype::*;
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::ntdef::{HRESULT, LPCWSTR};
use winapi::shared::windef::{HBITMAP, HBRUSH, HICON, HMENU, HWND, POINT, RECT};
use winapi::shared::windowsx::{GET_X_LPARAM, GET_Y_LPARAM};
use winapi::shared::winerror::S_OK;
use winapi::um::d3d11::*;
use winapi::um::d3d11sdklayers::*;
use winapi::um::d3dcommon::*;
use winapi::um::shellscalingapi::SetProcessDpiAwareness;
use winapi::um::winbase::{GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock, GHND};
use winapi::um::winuser::*;
use winapi::Interface;

use display_info::DisplayInfo;

mod math;
use math::*;

mod graphics;
use graphics::*;

mod capture;
use capture::*;

mod clipboard;
use clipboard::*;

const VERBOSE_LOG: bool = false;

const WINDOW_MIN_WIDTH: i32 = 320;
const WINDOW_MIN_HEIGHT: i32 = 240;

trait Dimensions {
    fn dim(&self) -> (i32, i32);
}

impl Dimensions for RECT {
    fn dim(&self) -> (i32, i32) {
        (self.right - self.left, self.bottom - self.top)
    }
}

struct WindowCreatedData {
    hwnd: HWND,
}

struct NativeMessageData {
    timestamp: Instant,
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

fn make_empty_rect() -> RECT {
    RECT {
        left: 0,
        right: 0,
        top: 0,
        bottom: 0,
    }
}

struct Window {
    message_rx: std::sync::mpsc::Receiver<WindowMessages>,
    hwnd: HWND,
    window_style: u32,
    window_rect: RECT,
    windowed_client_rect: RECT,
    window_dim: (i32, i32),
    full_screen: bool,
}

struct WindowThreadState {
    message_tx: std::sync::mpsc::Sender<WindowMessages>,
    is_window_closed: bool,
}

fn get_screen_dimensions() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

fn get_client_rect_absolute(hwnd: HWND) -> RECT {
    let mut client_rect = make_empty_rect();
    unsafe {
        GetClientRect(hwnd, &mut client_rect);
    }
    client_rect
}

fn get_window_rect_absolute(hwnd: HWND) -> RECT {
    let mut window_rect = make_empty_rect();
    unsafe {
        GetWindowRect(hwnd, &mut window_rect);
    }
    window_rect
}

fn get_client_rect(hwnd: HWND) -> RECT {
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

fn get_window_client_rect_dimensions(hwnd: HWND) -> (i32, i32) {
    let client_rect = get_client_rect(hwnd);
    (
        client_rect.right - client_rect.left,
        client_rect.bottom - client_rect.top,
    )
}

fn compute_client_rect(dim: (i32, i32)) -> RECT {
    let screen_dim = get_screen_dimensions();
    let window_pos = (screen_dim.0 / 2 - dim.0 / 2, screen_dim.1 / 2 - dim.1 / 2);
    RECT {
        left: window_pos.0,
        top: window_pos.1,
        right: window_pos.0 + dim.0,
        bottom: window_pos.1 + dim.1,
    }
}

fn get_desktop_work_area() -> RECT {
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
        WM_SYSCHAR => {
            // Ignore Alt + <key> inputs
            // main_window.set_full_screen(true);
            return 0;
        }
        WM_DROPFILES => {
            if let Some(window_state) = window_state {
                use winapi::um::shellapi::*;
                let hdrop = wparam as HDROP;
                let filename_len = DragQueryFileW(hdrop, 0, null_mut(), 0) as usize;
                if filename_len > 0 {
                    let mut filename_bytes = vec![0u16; filename_len + 1];
                    DragQueryFileW(
                        hdrop,
                        0,
                        filename_bytes.as_mut_ptr(),
                        filename_bytes.len() as u32,
                    );
                    filename_bytes.pop();
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
                let _ = window_state.message_tx.send(WindowMessages::NativeMessage(
                    NativeMessageData {
                        timestamp: Instant::now(),
                        msg,
                        wparam,
                        lparam,
                    },
                ));
            }
        }
    };

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn to_wide_string(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>()
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

                    let window_name = to_wide_string("imgv");
                    let icon_name = to_wide_string("imgv");
                    let window_class_name = to_wide_string("imgv_window_class");

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

                    let window_user_data = &mut window_state as *mut WindowThreadState as _;
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
                        window_user_data,
                    );
                    assert!(hwnd != (0 as HWND), "failed to open the window");

                    winapi::um::shellapi::DragAcceptFiles(hwnd, 1);

                    // Delay showing this window until D3D is ready to draw something
                    // ShowWindow(hwnd, SW_SHOW);

                    loop {
                        if window_state.is_window_closed {
                            break;
                        }
                        let mut msg: MSG = std::mem::zeroed();
                        if GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
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
                window_rect: get_window_rect_absolute(data.hwnd),
                windowed_client_rect: get_client_rect(data.hwnd),
                window_dim,
                full_screen: false,
            });
        }

        Err(())
    }

    pub fn set_window_name(&mut self, name: &str) {
        unsafe {
            SetWindowTextW(self.hwnd, to_wide_string(name).as_ptr());
        }
    }

    pub fn set_image_size(&mut self, dim: (i32, i32)) {
        if self.full_screen {
            self.set_full_screen(false);
        }
        let mut rect = compute_client_rect(dim);
        unsafe {
            let desktop_rect = get_desktop_work_area().dim();
            AdjustWindowRect(&mut rect, self.window_style, 0);
            if dim.0 < desktop_rect.0 && dim.1 < desktop_rect.1 {
                let placement = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    flags: 0,
                    showCmd: SW_RESTORE as u32,
                    ptMinPosition: POINT { x: 0, y: 0 },
                    ptMaxPosition: POINT { x: 0, y: 0 },
                    rcNormalPosition: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                };
                //ShowWindow(self.hwnd, SW_RESTORE);
                SetWindowPlacement(self.hwnd, &placement);
                SetWindowPos(
                    self.hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    0,
                );
                self.window_dim = dim;
                self.window_rect = get_window_rect_absolute(self.hwnd);
            } else {
                //ShowWindow(self.hwnd, SW_MAXIMIZE);
                let placement = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    flags: 0,
                    showCmd: SW_MAXIMIZE as u32,
                    ptMinPosition: POINT { x: 0, y: 0 },
                    ptMaxPosition: POINT { x: 0, y: 0 },
                    rcNormalPosition: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                };
                SetWindowPlacement(self.hwnd, &placement);
            }
        }
    }

    pub fn set_full_screen(&mut self, state: bool) {
        if state {
            self.windowed_client_rect = get_client_rect(self.hwnd);
            let (w, h) = get_screen_dimensions();
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_VISIBLE | WS_POPUP) as isize);
                SetWindowPos(self.hwnd, HWND_TOP, 0, 0, w, h, SWP_FRAMECHANGED);
            }
            self.window_dim = (w, h);
            self.full_screen = true;
        } else {
            unsafe {
                SetWindowLongPtrW(
                    self.hwnd,
                    GWL_STYLE,
                    (WS_VISIBLE | self.window_style) as isize,
                );
                let mut rect = self.windowed_client_rect;
                AdjustWindowRect(&mut rect, self.window_style, 0);
                let w = rect.right - rect.left;
                let h = rect.bottom - rect.top;
                SetWindowPos(
                    self.hwnd,
                    null_mut(),
                    self.windowed_client_rect.left,
                    self.windowed_client_rect.top,
                    w,
                    h,
                    SWP_FRAMECHANGED,
                );
                self.window_dim = (w, h);
            }
            self.full_screen = false;
        }
    }

    pub fn screenshot(&self) {
        let image = capture_window(self.hwnd as isize).expect("Failed to capture window image");

        image
            .save("C:\\Temp\\screenshot.png")
            .expect("Failed to save screenshot image to file");
    }

    pub fn clipboard_save(&self) {
        save_to_clipboard(self.hwnd as isize).expect("Failed to capture image to clipboard");
    }
}

fn process_window_messages(window: &Window, should_block: bool) -> Option<WindowMessages> {
    profiling::scope!("RcvWindowMessages");
    if should_block {
        profiling::scope!("Block");
        if let Ok(x) = window.message_rx.recv() {
            return Some(x);
        }
    } else if let Ok(x) = window.message_rx.try_recv() {
        return Some(x);
    }
    None
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
            .filter_map(|f| f.ok().map(|f| f.path()))
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

fn to_milliseconds(t: Duration) -> f32 {
    t.as_secs_f32() * 1000.0
}

struct ViewerState {
    texture: Option<Texture>,
    frame_number: u32,
    is_resizing: bool,
    is_dragging: bool,
    drag_origin: float2,
    mouse_pos: float2,
    viewport_dim: float2,
    image_dim: float2,
    xfm_window_to_image: Transform2D,
}

impl ViewerState {
    fn new() -> Self {
        Self {
            texture: None,
            frame_number: 0,
            is_resizing: false,
            is_dragging: false,
            drag_origin: FLOAT2_ZERO,
            mouse_pos: FLOAT2_ZERO,
            viewport_dim: FLOAT2_ZERO,
            image_dim: FLOAT2_ZERO,
            xfm_window_to_image: Transform2D::new_identity(),
        }
    }

    fn reset_image_transform(&mut self) {
        self.xfm_window_to_image = Transform2D::new_identity();
        self.xfm_window_to_image.offset = 0.5 * self.image_dim - 0.5 * self.viewport_dim;
    }
}

fn apply_loaded_image(
    state: &mut ViewerState,
    main_window: &mut Window,
    graphics: &GraphicsD3D11,
    constants: &mut Constants,
    img: image::DynamicImage,
    image_name: Option<&str>,
) -> (u32, u32) {
    state.texture = Some(Texture::new(&graphics.device, img));

    let dim = state.texture.as_ref().unwrap().dim;

    let pending_image_dim: float2 = float2::new(dim.0 as f32, dim.1 as f32);
    if constants.image_dim != pending_image_dim {
        constants.image_dim = pending_image_dim;
        if !main_window.full_screen {
            main_window.set_image_size((dim.0 as i32, dim.1 as i32));
        }
        state.xfm_window_to_image = Transform2D::new_identity();
        let window_dim = float2::new(
            main_window.window_dim.0 as f32,
            main_window.window_dim.1 as f32,
        );
        state.xfm_window_to_image.offset = 0.5 * constants.image_dim - 0.5 * window_dim;
    }

    if let Some(image_name) = image_name {
        main_window.set_window_name(image_name);
    }

    dim
}

fn main() {
    profiling::register_thread!("main");

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

    let mut state = ViewerState::new();

    let mut main_window: Window = Window::new((500, 500)).unwrap();
    let main_window_handle = main_window.hwnd as u64;
    std::thread::spawn(move || {
        while let Ok(x) = load_req_rx.recv() {
            profiling::scope!("LoadImage");
            let load_begin_time = Instant::now();
            println!("Loading image {:?}", x);
            let img = image::open(&x);
            let _ = image_tx.send((img, load_begin_time, x));
            unsafe {
                InvalidateRect(main_window_handle as HWND, null_mut(), 1);
            }
        }
        println!("Loading thread done");
    });

    {
        let window_time = Instant::now() - main_begin_time;
        println!("Time to window: {} ms", to_milliseconds(window_time));
    }

    let mut graphics: GraphicsD3D11 = unsafe { GraphicsD3D11::new(main_window.hwnd).unwrap() };

    // Delay showing the window until the first frame can be drawn to avoid showing default blank frame
    unsafe {
        let hwnd = main_window.hwnd;
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }

    let mut constants = Constants {
        image_dim: FLOAT2_ZERO,
        window_dim: FLOAT2_ZERO,
        mouse: FLOAT4_ZERO,
        xfm_viewport_to_image_uv: Transform2D::new_identity().into(),
    };

    let switch_to_next_image = |current_image_path: &Path, direction: StepDirection| {
        let file_name = get_next_file(current_image_path, direction);
        let file_dir = current_image_path.parent();
        if let (Some(file_name), Some(file_dir)) = (file_name, file_dir) {
            let file_dir = file_dir.to_path_buf();
            let path = file_dir.join(file_name);
            load_req_tx.send(path.clone()).unwrap();
            Some(path)
        } else {
            Some(current_image_path.into())
        }
    };

    let mut draw_begin_time = Instant::now();
    let mut draw_end_time = Instant::now();
    let mut last_frame_draw_time = Instant::now();
    let mut should_draw = true;
    let mut should_block = true;
    let mut should_exit = false;
    let mut handled_events = 0;
    let mut last_verbose_log_time = Instant::now();
    while !should_exit {
        profiling::scope!("MainLoop");
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
                    let latency = Instant::now() - native_msg.timestamp;
                    let time_since_verbose_log = Instant::now() - last_verbose_log_time;
                    if latency > Duration::from_millis(20)
                        && time_since_verbose_log > Duration::from_millis(100)
                    {
                        profiling::scope!(
                            "Hitch",
                            format!("{} ms", to_milliseconds(latency)).as_str()
                        );
                        println!("Hitch: {} ms", to_milliseconds(latency));
                        last_verbose_log_time = Instant::now();
                    }
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
                            let mouse_pos_img =
                                state.xfm_window_to_image.transform_point(state.mouse_pos);
                            let zoom_transform = Transform2D::new_translate(-mouse_pos_img)
                                .concatenate(Transform2D::new_scale(zoom))
                                .concatenate(Transform2D::new_translate(mouse_pos_img));
                            state
                                .xfm_window_to_image
                                .inplace_concatenate(zoom_transform);
                            should_draw = true;
                            state.is_dragging = false;
                        }
                        WM_LBUTTONDOWN => {
                            state.is_dragging = true;
                            state.drag_origin =
                                state.mouse_pos - state.xfm_window_to_image.inverse().offset;
                        }
                        WM_LBUTTONUP => {
                            state.is_dragging = false;
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
                            state.mouse_pos = decode_mouse_pos(lparam);
                            constants.mouse.x = state.mouse_pos.x;
                            constants.mouse.y = state.mouse_pos.y;
                            let drag_delta: float2 = state.drag_origin - state.mouse_pos;
                            if state.is_dragging {
                                state.xfm_window_to_image.offset =
                                    drag_delta.mul_element_wise(state.xfm_window_to_image.scale);
                            }
                            should_draw = true;
                        }
                        WM_KEYDOWN => {
                            let ctrl_down = unsafe {
                                (GetKeyState(VK_LCONTROL) < 0) || (GetKeyState(VK_RCONTROL) < 0)
                            };

                            match (wparam as i32, wparam as u8 as char) {
                                (VK_ESCAPE, _) => {
                                    should_exit = true;
                                }
                                (VK_HOME, _) => {
                                    state.xfm_window_to_image = Transform2D::new_identity();
                                    let window_dim = float2::new(
                                        main_window.window_dim.0 as f32,
                                        main_window.window_dim.1 as f32,
                                    );
                                    state.xfm_window_to_image.offset =
                                        0.5 * constants.image_dim - 0.5 * window_dim;
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
                                (VK_RETURN, _) => {
                                    main_window.set_full_screen(!main_window.full_screen);
                                }
                                (_, '1') => {
                                    let s = 1.0;
                                    state.xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '2') => {
                                    let s = 1.0 / 2.0;
                                    state.xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '3') => {
                                    let s = 1.0 / 4.0;
                                    state.xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '4') => {
                                    let s = 1.0 / 8.0;
                                    state.xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, '5') => {
                                    let s = 1.0 / 16.0;
                                    state.xfm_window_to_image.scale = float2::new(s, s);
                                }
                                (_, 'V') if ctrl_down => {
                                    if let Ok(Some(path)) = get_clipboard_file_path() {
                                        image_path = Some(path.clone());
                                        load_req_tx.send(path).unwrap();
                                    } else if let Ok(Some(img)) = get_clipboard_image() {
                                        image_path = None;
                                        let dim = apply_loaded_image(
                                            &mut state,
                                            &mut main_window,
                                            &graphics,
                                            &mut constants,
                                            img,
                                            Some("Clipboard Image"),
                                        );
                                        println!(
                                            "Loaded clipboard image {:?}x{:?}",
                                            dim.0, dim.1
                                        );
                                    }
                                }
                                (_, 'C') | (VK_INSERT, _) if ctrl_down => {
                                    main_window.clipboard_save();
                                }
                                _ => {}
                            }
                            should_draw = true;
                        }
                        WM_SIZE => {
                            let width = winapi::shared::minwindef::LOWORD(lparam as u32) as i32;
                            let height = winapi::shared::minwindef::HIWORD(lparam as u32) as i32;
                            state.viewport_dim = float2::new(width as f32, height as f32);
                            main_window.window_dim = (width, height);
                            let new_window_rect = get_window_rect_absolute(main_window.hwnd);
                            let edge_delta = float2::new(
                                (new_window_rect.left - main_window.window_rect.left) as f32,
                                (new_window_rect.top - main_window.window_rect.top) as f32,
                            );
                            if edge_delta != FLOAT2_ZERO {
                                state.xfm_window_to_image.offset +=
                                    edge_delta.mul_element_wise(state.xfm_window_to_image.scale);
                            }
                            main_window.window_rect = new_window_rect;
                            graphics.update_backbuffer(main_window.hwnd);
                            should_draw = true;
                        }
                        WM_ENTERSIZEMOVE => {
                            state.is_resizing = true;
                        }
                        WM_EXITSIZEMOVE => {
                            should_draw = true;
                            state.is_resizing = false;
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
            scale: 1.0 / constants.image_dim,
            offset: FLOAT2_ZERO,
        };

        constants.window_dim.x = main_window.window_dim.0 as f32;
        constants.window_dim.y = main_window.window_dim.1 as f32;

        let xfm_window_to_image_quantized = if state.xfm_window_to_image.scale.x >= 1.0
            || state.xfm_window_to_image.scale.y >= 1.0
        {
            Transform2D {
                scale: state.xfm_window_to_image.scale,
                offset: float2_round(state.xfm_window_to_image.offset),
            }
        } else {
            state.xfm_window_to_image
        };

        constants.xfm_viewport_to_image_uv = xfm_window_to_image_quantized
            .concatenate(xfm_viewport_to_image_uv)
            .into();

        if let Ok((img, load_begin_time, image_filename)) = image_rx.try_recv() {
            if let Ok(img) = img {
                // Image loaded
                let image_name = image_filename.to_string_lossy().into_owned();
                let dim = apply_loaded_image(
                    &mut state,
                    &mut main_window,
                    &graphics,
                    &mut constants,
                    img,
                    Some(&image_name),
                );

                let image_load_time = Instant::now() - load_begin_time;
                println!(
                    "Time to load image {} ms ({:?})",
                    to_milliseconds(image_load_time),
                    dim
                );
            } else {
                println!("Failed to load image: {:?}", img.err());
            };

            unsafe {
                InvalidateRect(main_window_handle as HWND, null_mut(), 1);
            }
        }

        if state.frame_number == 0 {
            let init_time = Instant::now() - main_begin_time;
            println!("Init time: {:.2}ms", to_milliseconds(init_time));
        }

        unsafe {
            profiling::scope!("Draw");

            if VERBOSE_LOG {
                let frame_delta_time = Instant::now() - last_frame_draw_time;
                last_frame_draw_time = Instant::now();
                let draw_time = draw_end_time - draw_begin_time;
                println!(
                    "Draw dt: {:.2}ms, frame_number: {}, handled_events: {}, draw time: {:.2}ms",
                    to_milliseconds(frame_delta_time),
                    state.frame_number,
                    handled_events,
                    to_milliseconds(draw_time)
                );
            }

            draw_begin_time = Instant::now();

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
            let srvs: [*mut ID3D11ShaderResourceView; 1] =
                [if let Some(texture) = &state.texture {
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

            let present_interval = if state.is_dragging { 0 } else { 1 };
            graphics.present(present_interval);

            draw_end_time = Instant::now();
            profiling::finish_frame!();
        };

        state.frame_number += 1;
        should_draw = false;
        handled_events = 0;
    }
}
