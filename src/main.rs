#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(unused_imports)]

use cgmath::{assert_ulps_eq, prelude::*};
use com_ptr::{hresult, ComPtr};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use std::{path::Path, path::PathBuf};
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

mod logging;
use logging::{init_logging, maybe_alloc_console};

mod window;
use window::*;

mod loader;
use loader::{load_image_with_metadata, ImageData};

mod caption;
use caption::{Caption, CaptionButton, CaptionRenderer, CaptionState, Theme};

mod placeholder;
use placeholder::{set_transitions_disabled, wait_for_composition, Placeholder, ScreenRect};

mod browse;
use browse::{get_next_file, StepDirection};

const WINDOW_MIN_WIDTH: i32 = 320;
const WINDOW_MIN_HEIGHT: i32 = 240;

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

struct Window {
    message_rx: std::sync::mpsc::Receiver<WindowMessages>,
    hwnd: HWND,
    window_style: u32,
    client_rect: RECT,
    content_anchor: (i32, i32),
    windowed_client_rect: RECT,
    window_dim: (i32, i32),
    full_screen: bool,
    caption: Arc<CaptionState>,
    title: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowPlacement {
    Centered,
    KeepCenter,
}

struct ImageWindowPlan {
    client_rect: RECT,
    caption_height: i32,
    normal_window_rect: Option<RECT>,
}

fn rect_tuple(r: &RECT) -> (i32, i32, i32, i32) {
    (r.left, r.top, r.right, r.bottom)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContentAnchor {
    FollowWindowMove,
    FixedOnScreen,
}

struct WindowThreadState {
    message_tx: std::sync::mpsc::Sender<WindowMessages>,
    is_window_closed: bool,
    caption: Arc<CaptionState>,
}

fn caption_syscommand(button: CaptionButton, hwnd: HWND) -> Option<usize> {
    match button {
        CaptionButton::Minimize => Some(SC_MINIMIZE),
        CaptionButton::Maximize if unsafe { IsZoomed(hwnd) } != 0 => Some(SC_RESTORE),
        CaptionButton::Maximize => Some(SC_MAXIMIZE),
        CaptionButton::Close => Some(SC_CLOSE),
        CaptionButton::None => None,
    }
}

unsafe fn handle_caption_message(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
    caption: &CaptionState,
) -> Option<LRESULT> {
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    if !caption::has_caption(style) {
        return None;
    }

    let redraw = |changed: bool| {
        if changed {
            InvalidateRect(hwnd, null_mut(), 0);
        }
    };

    match msg {
        WM_NCHITTEST => {
            let hit = DefWindowProcW(hwnd, msg, wparam, lparam);
            if hit != HTCLIENT {
                return Some(hit);
            }
            Some(caption::hit_test_client(hwnd, style, lparam))
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
            let button = CaptionButton::from_hit(wparam as isize);
            if button == CaptionButton::None {
                return None;
            }
            redraw(caption.set_pressed(button));
            Some(0)
        }
        WM_NCLBUTTONUP => {
            let button = CaptionButton::from_hit(wparam as isize);
            if button == CaptionButton::None {
                redraw(caption.set_pressed(CaptionButton::None));
                return None;
            }
            if caption.pressed() == button {
                if let Some(command) = caption_syscommand(button, hwnd) {
                    PostMessageW(hwnd, WM_SYSCOMMAND, command, 0);
                }
            }
            redraw(caption.set_pressed(CaptionButton::None));
            Some(0)
        }
        WM_NCMOUSEMOVE => {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_NONCLIENT,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut track);
            redraw(caption.set_hovered(CaptionButton::from_hit(wparam as isize)));
            None
        }
        WM_NCMOUSELEAVE => {
            let hovered = caption.set_hovered(CaptionButton::None);
            let pressed = caption.set_pressed(CaptionButton::None);
            redraw(hovered || pressed);
            None
        }
        WM_MOUSEMOVE => {
            redraw(caption.set_hovered(CaptionButton::None));
            None
        }
        WM_NCACTIVATE => {
            redraw(caption.set_active(wparam != 0));
            Some(DefWindowProcW(hwnd, msg, wparam, -1))
        }
        _ => None,
    }
}

unsafe fn is_color_scheme_change(lparam: LPARAM) -> bool {
    let name = lparam as *const u16;
    if name.is_null() {
        return false;
    }

    "ImmersiveColorScheme"
        .encode_utf16()
        .chain(Some(0))
        .enumerate()
        .all(|(i, c)| *name.add(i) == c)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let window_state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowThreadState;
    let window_state = window_state_ptr.as_mut();

    if msg == WM_NCCALCSIZE {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if caption::has_caption(style) {
            let rect = if wparam != 0 {
                &mut (*(lparam as *mut NCCALCSIZE_PARAMS)).rgrc[0]
            } else {
                &mut *(lparam as *mut RECT)
            };
            let maximized = IsZoomed(hwnd) != 0;
            *rect = caption::window_to_client(*rect, &caption::frame_insets(style), maximized);
            return 0;
        }
    }

    if let Some(window_state) = window_state.as_ref() {
        if let Some(result) =
            handle_caption_message(hwnd, msg, wparam, lparam, &window_state.caption)
        {
            return result;
        }
    }

    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            SetCapture(hwnd);
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
        }
        _ => {}
    }

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
        WM_SETTINGCHANGE if is_color_scheme_change(lparam) => {
            if let Some(window_state) = window_state {
                let theme = Theme::from_system();
                if window_state.caption.set_theme(theme) {
                    set_frame_theme(hwnd, theme);
                    InvalidateRect(hwnd, null_mut(), 0);
                }
            }
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

impl Window {
    fn new(window_dim: (i32, i32)) -> Result<Window, ()> {
        let (channel_sender, channel_receiver) = std::sync::mpsc::channel();
        let caption = Arc::new(CaptionState::new());
        let thread_caption = caption.clone();

        let window_style: u32 =
            WS_MAXIMIZEBOX | WS_MINIMIZEBOX | WS_SYSMENU | WS_SIZEBOX | WS_CAPTION;

        std::thread::Builder::new()
            .name("window".to_owned())
            .spawn(move || {
                let mut window_state = WindowThreadState {
                    message_tx: channel_sender,
                    is_window_closed: false,
                    caption: thread_caption,
                };

                unsafe {
                    SetProcessDpiAwareness(1);

                    let window_name = to_wide_string(concat!("imgv ", env!("CARGO_PKG_VERSION")));
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

                    let client_rect = caption::client_to_window(
                        compute_client_rect(window_dim),
                        &caption::frame_insets(window_style),
                    );

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
                    disable_rounded_corners(hwnd);
                    set_frame_theme(hwnd, window_state.caption.theme());

                    // Delay showing this window until D3D is ready to draw something
                    // ShowWindow(hwnd, SW_SHOW);

                    loop {
                        if window_state.is_window_closed {
                            break;
                        }
                        let mut msg: MSG = std::mem::zeroed();
                        let result = GetMessageW(&mut msg, null_mut(), 0, 0);
                        if result > 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        } else if result == 0 {
                            break;
                        } else {
                            let _ = window_state.message_tx.send(WindowMessages::WindowClosed);
                            window_state.is_window_closed = true;
                            break;
                        }
                    }
                }
            })
            .unwrap();

        if let WindowMessages::WindowCreated(data) = channel_receiver.recv().unwrap() {
            let client_rect = get_client_rect(data.hwnd);
            return Ok(Window {
                message_rx: channel_receiver,
                hwnd: data.hwnd,
                window_style,
                client_rect,
                content_anchor: (client_rect.left, client_rect.top),
                windowed_client_rect: client_rect,
                window_dim,
                full_screen: false,
                caption,
                title: concat!("imgv ", env!("CARGO_PKG_VERSION")).to_string(),
            });
        }

        Err(())
    }

    pub fn set_window_name(&mut self, name: &str) {
        self.title = name.to_string();
        unsafe {
            SetWindowTextW(self.hwnd, to_wide_string(name).as_ptr());
        }
    }

    pub fn plan_image_size(&self, dim: (i32, i32), placement: WindowPlacement) -> ImageWindowPlan {
        let work = get_monitor_work_rect(self.hwnd);
        let insets = caption::frame_insets(self.window_style);

        let window_w = (dim.0 + insets.left + insets.right).max(WINDOW_MIN_WIDTH);
        let window_h = (dim.1 + insets.top + insets.bottom).max(WINDOW_MIN_HEIGHT);
        let (work_w, work_h) = work.dim();

        if window_w <= work_w && window_h <= work_h {
            let (left, top) = match placement {
                WindowPlacement::KeepCenter => {
                    let current = get_window_rect(self.hwnd);
                    let center_x = (current.left + current.right) / 2;
                    let center_y = (current.top + current.bottom) / 2;
                    (
                        (center_x - window_w / 2).clamp(work.left, work.right - window_w),
                        (center_y - window_h / 2).clamp(work.top, work.bottom - window_h),
                    )
                }
                WindowPlacement::Centered => (
                    work.left + (work_w - window_w) / 2,
                    work.top + (work_h - window_h) / 2,
                ),
            };
            let window_rect = RECT {
                left,
                top,
                right: left + window_w,
                bottom: top + window_h,
            };
            ImageWindowPlan {
                client_rect: caption::window_to_client(window_rect, &insets, false),
                caption_height: caption::caption_height(&insets, false),
                normal_window_rect: Some(window_rect),
            }
        } else {
            ImageWindowPlan {
                client_rect: work,
                caption_height: caption::caption_height(&insets, true),
                normal_window_rect: None,
            }
        }
    }
    pub fn set_normal_client_rect(&mut self, client_rect: &RECT) {
        let rect = caption::client_to_window(*client_rect, &caption::frame_insets(self.window_style));
        let (w, h) = rect.dim();
        unsafe {
            SetWindowPos(self.hwnd, null_mut(), rect.left, rect.top, w, h, 0);
        }
        self.sync_client_rect(ContentAnchor::FixedOnScreen);
    }

    pub fn set_image_size(&mut self, plan: &ImageWindowPlan) {
        unsafe {
            match plan.normal_window_rect {
                Some(rect) => {
                    let (w, h) = rect.dim();
                    SetWindowPos(
                        self.hwnd,
                        null_mut(),
                        rect.left,
                        rect.top,
                        w,
                        h,
                        0,
                    );
                }
                None => {
                    let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
                    placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
                    GetWindowPlacement(self.hwnd, &mut placement);
                    placement.flags = 0;
                    placement.showCmd = SW_MAXIMIZE as u32;
                    SetWindowPlacement(self.hwnd, &placement);
                }
            }
        }

        self.sync_client_rect(ContentAnchor::FixedOnScreen);
        if !rects_equal(&self.client_rect, &plan.client_rect) {
            debug!(
                "Client rect {:?} differs from planned {:?}",
                rect_tuple(&self.client_rect),
                rect_tuple(&plan.client_rect)
            );
        }
    }

    pub fn sync_client_rect(&mut self, anchor: ContentAnchor) {
        if unsafe { IsIconic(self.hwnd) } != 0 {
            return;
        }

        let new_rect = get_client_rect(self.hwnd);
        let moved_only = new_rect.dim() == self.client_rect.dim();
        if anchor == ContentAnchor::FollowWindowMove && moved_only {
            self.content_anchor.0 += new_rect.left - self.client_rect.left;
            self.content_anchor.1 += new_rect.top - self.client_rect.top;
        }

        self.client_rect = new_rect;
        self.window_dim = new_rect.dim();
    }

    pub fn content_offset(&self) -> [i32; 2] {
        self.content_offset_at(&self.client_rect)
    }

    pub fn content_offset_at(&self, client_rect: &RECT) -> [i32; 2] {
        [
            client_rect.left - self.content_anchor.0,
            client_rect.top - self.content_anchor.1,
        ]
    }

    pub fn caption_height(&self) -> i32 {
        if self.full_screen {
            return 0;
        }
        let maximized = unsafe { IsZoomed(self.hwnd) } != 0;
        caption::caption_height(&caption::frame_insets(self.window_style), maximized)
    }

    pub fn is_maximized(&self) -> bool {
        unsafe { IsZoomed(self.hwnd) != 0 }
    }

    pub fn center_content_at(&self, client_rect: &RECT, caption_height: i32) -> float2 {
        let offset = self.content_offset_at(client_rect);
        let (w, h) = client_rect.dim();
        float2::new(
            offset[0] as f32 + 0.5 * w as f32,
            offset[1] as f32 + caption_height as f32 + 0.5 * (h - caption_height) as f32,
        )
    }

    pub fn center_content(&self) -> float2 {
        self.center_content_at(&self.client_rect, self.caption_height())
    }

    pub fn screen_to_content(&self, p: POINT) -> float2 {
        float2::new(
            (p.x - self.content_anchor.0) as f32,
            (p.y - self.content_anchor.1) as f32,
        )
    }

    fn zoom_anchor(&self) -> float2 {
        let mut cursor = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut cursor) } == 0 {
            return self.center_content();
        }

        let mut viewport = self.client_rect;
        viewport.top += self.caption_height();
        if unsafe { PtInRect(&viewport, cursor) } == 0 {
            return self.center_content();
        }

        self.screen_to_content(cursor)
    }

    pub fn client_rect_for_full_screen(&self, full_screen: bool) -> (RECT, i32) {
        if full_screen {
            (get_monitor_rect(self.hwnd), 0)
        } else {
            let insets = caption::frame_insets(self.window_style);
            (self.windowed_client_rect, caption::caption_height(&insets, false))
        }
    }

    pub fn content_offset_f(&self) -> float2 {
        let offset = self.content_offset();
        float2::new(offset[0] as f32, offset[1] as f32)
    }

    pub fn client_dim(&self) -> float2 {
        let dim = self.client_rect.dim();
        float2::new(dim.0 as f32, dim.1 as f32)
    }

    pub fn set_full_screen(&mut self, state: bool) {
        if state {
            self.windowed_client_rect = get_client_rect(self.hwnd);
            let monitor = get_monitor_rect(self.hwnd);
            let (w, h) = monitor.dim();
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_VISIBLE | WS_POPUP) as isize);
                SetWindowPos(self.hwnd, HWND_TOP, monitor.left, monitor.top, w, h, SWP_FRAMECHANGED);
            }
            self.full_screen = true;
        } else {
            unsafe {
                SetWindowLongPtrW(
                    self.hwnd,
                    GWL_STYLE,
                    (WS_VISIBLE | self.window_style) as isize,
                );
                let rect = caption::client_to_window(
                    self.windowed_client_rect,
                    &caption::frame_insets(self.window_style),
                );
                let w = rect.right - rect.left;
                let h = rect.bottom - rect.top;
                SetWindowPos(self.hwnd, null_mut(), rect.left, rect.top, w, h, SWP_FRAMECHANGED);
            }
            self.full_screen = false;
        }

        self.sync_client_rect(ContentAnchor::FixedOnScreen);
    }

    pub fn screenshot(&self) {
        let image = capture_window(self.hwnd as isize, self.caption_height()).expect("Failed to capture window image");

        image
            .save("C:\\Temp\\screenshot.png")
            .expect("Failed to save screenshot image to file");
    }

    pub fn clipboard_save(&self) {
        save_to_clipboard(self.hwnd as isize, self.caption_height()).expect("Failed to capture image to clipboard");
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

fn to_milliseconds(t: Duration) -> f32 {
    t.as_secs_f32() * 1000.0
}

struct ViewerState {
    texture: Option<Texture>,
    frame_number: u32,
    is_resizing: bool,
    is_dragging: bool,
    drag_last_mouse_pos: float2,
    mouse_pos: float2,
    show_transparency: bool,
    dds_data: Option<DdsTextureData>,
    dds_slice: u32,
    dds_mip: u32,
    xfm_content_to_image: Transform2D,
    shown_image: Option<PathBuf>,
    saved_views: HashMap<PathBuf, ImageView>,
}

#[derive(Clone, Copy)]
struct ImageView {
    xfm_content_to_image: Transform2D,
    window_center: float2,
}

const DEFAULT_VIEW_TOLERANCE: f32 = 1e-3;

const ZOOM_STEPS_PER_OCTAVE: f32 = 4.0;

fn zoom_level(scale: f32) -> f32 {
    -scale.log2() * ZOOM_STEPS_PER_OCTAVE
}

fn zoom_scale(level: f32) -> f32 {
    let nearest = level.round();
    let level = if (level - nearest).abs() < 1e-3 {
        nearest
    } else {
        level
    };
    (-level / ZOOM_STEPS_PER_OCTAVE).exp2()
}

#[test]
fn test_zoom_ladder() {
    let mut scale = 1.0;
    for _ in 0..8 {
        scale = zoom_scale(zoom_level(scale) + 1.0);
    }
    assert_eq!(scale, 0.25);
    for _ in 0..12 {
        scale = zoom_scale(zoom_level(scale) - 1.0);
    }
    assert_eq!(scale, 2.0);
}

impl ImageView {
    fn is_default(&self, image_dim: float2) -> bool {
        let xfm = &self.xfm_content_to_image;
        let center = xfm.transform_point(self.window_center);
        let max_diff = |a: float2, b: float2| (a.x - b.x).abs().max((a.y - b.y).abs());
        max_diff(xfm.scale, FLOAT2_ONE) <= DEFAULT_VIEW_TOLERANCE
            && max_diff(center, 0.5 * image_dim) <= DEFAULT_VIEW_TOLERANCE
    }
}

impl ViewerState {
    fn new() -> Self {
        Self {
            texture: None,
            frame_number: 0,
            is_resizing: false,
            is_dragging: false,
            drag_last_mouse_pos: FLOAT2_ZERO,
            mouse_pos: FLOAT2_ZERO,
            show_transparency: false,
            dds_data: None,
            dds_slice: 0,
            dds_mip: 0,
            xfm_content_to_image: Transform2D::new_identity(),
            shown_image: None,
            saved_views: HashMap::new(),
        }
    }

    fn center_image(&mut self, scale: float2, image_dim: float2, window_center: float2) {
        self.xfm_content_to_image = Transform2D {
            scale,
            offset: 0.5 * image_dim - window_center.mul_element_wise(scale),
        };
    }

    fn zoom_at(&mut self, scale: f32, anchor_content: float2) {
        let xfm = &mut self.xfm_content_to_image;
        let anchor_image = xfm.transform_point(anchor_content);
        xfm.scale = float2::new(scale, scale);
        xfm.offset = anchor_image - anchor_content * scale;
    }

    fn view(&self, window: &Window) -> ImageView {
        ImageView {
            xfm_content_to_image: self.xfm_content_to_image,
            window_center: window.center_content(),
        }
    }

    fn set_view(&mut self, view: ImageView, window_center: float2) {
        let mut xfm = view.xfm_content_to_image;
        let shift = view.window_center - window_center;
        if shift != FLOAT2_ZERO {
            xfm.offset += shift.mul_element_wise(xfm.scale);
        }
        self.xfm_content_to_image = xfm;
    }

    fn save_shown_image_view(&mut self, window: &Window, image_dim: float2) {
        let Some(path) = self.shown_image.take() else {
            return;
        };

        let view = self.view(window);
        if view.is_default(image_dim) {
            self.saved_views.remove(&path);
        } else {
            self.saved_views.insert(path, view);
        }
    }

    fn mouse_pos_content(&self, window: &Window) -> float2 {
        self.mouse_pos + window.content_offset_f()
    }
}

fn update_view_constants(
    constants: &mut Constants,
    state: &ViewerState,
    client_dim: (i32, i32),
    content_offset: [i32; 2],
) {
    constants.window_dim = float2::new(client_dim.0 as f32, client_dim.1 as f32);
    constants.content_offset = content_offset;

    let xfm = state.xfm_content_to_image;
    let xfm_quantized = if xfm.scale.x >= 1.0 || xfm.scale.y >= 1.0 {
        Transform2D {
            scale: xfm.scale,
            offset: float2_round(xfm.offset),
        }
    } else {
        xfm
    };
    let xfm_image_to_uv = Transform2D {
        scale: 1.0 / constants.image_dim,
        offset: FLOAT2_ZERO,
    };
    constants.xfm_content_to_image_uv = xfm_quantized.concatenate(xfm_image_to_uv).into();
}

fn draw_frame(
    graphics: &GraphicsD3D11,
    state: &ViewerState,
    constants: &Constants,
    rtv: *mut ID3D11RenderTargetView,
    dim: (u32, u32),
) {
    unsafe {
        let context = &graphics.context;

        context.UpdateSubresource(
            graphics.constants.as_ptr() as _,
            0,
            null_mut(),
            constants as *const Constants as _,
            0,
            0,
        );

        let rtvs: [*mut ID3D11RenderTargetView; 1] = [rtv];
        context.OMSetRenderTargets(1, rtvs.as_ptr(), null_mut());

        let viewport = D3D11_VIEWPORT {
            Width: dim.0 as f32,
            Height: dim.1 as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
            TopLeftX: 0.0,
            TopLeftY: 0.0,
        };
        context.RSSetViewports(1, &viewport);

        let clear_color: [f32; 4] = [0.1, 0.2, 0.3, 1.0];
        context.ClearRenderTargetView(rtv, &clear_color);

        let cbvs: [*mut ID3D11Buffer; 1] = [graphics.constants.as_ptr()];
        let (srv_array, srv_volume) = if let Some(texture) = &state.texture {
            match texture.image_type {
                ImageType::Image2DArray => (texture.srv.as_ptr(), graphics.dummy_srv_3d.as_ptr()),
                ImageType::Volume => (graphics.dummy_srv_2d_array.as_ptr(), texture.srv.as_ptr()),
            }
        } else {
            (graphics.dummy_srv_2d_array.as_ptr(), graphics.dummy_srv_3d.as_ptr())
        };
        let srvs: [*mut ID3D11ShaderResourceView; 2] = [srv_array, srv_volume];
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
    }
}

#[derive(Clone, Copy)]
struct FrameTarget {
    client_rect: RECT,
    caption_height: i32,
    maximized: bool,
}

impl FrameTarget {
    fn current(window: &Window) -> Self {
        Self {
            client_rect: window.client_rect,
            caption_height: window.caption_height(),
            maximized: window.is_maximized(),
        }
    }

    fn normal_state(&self) -> bool {
        !self.maximized && self.caption_height > 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceholderContent {
    NextFrame,
    LastFrame,
}

fn caption_for<'a>(window: &'a Window, target: &FrameTarget) -> Caption<'a> {
    Caption {
        width: target.client_rect.dim().0,
        height: target.caption_height,
        title: &window.title,
        active: window.caption.active(),
        hovered: window.caption.hovered(),
        pressed: window.caption.pressed(),
        maximized: target.maximized,
        theme: window.caption.theme(),
    }
}

fn draw_caption(graphics: &GraphicsD3D11, texture: *mut ID3D11Texture2D, caption: &Caption) {
    if let Some(renderer) = &graphics.caption {
        if !unsafe { renderer.draw(texture as _, caption) } {
            warn!("Caption draw failed");
        }
    }
}

fn draw_to_back_buffer(
    graphics: &mut GraphicsD3D11,
    state: &ViewerState,
    constants: &Constants,
    window: &Window,
) {
    let backbuffer = graphics
        .backbuffer
        .as_ref()
        .expect("Back buffer must be created before rendering a frame");
    let (rtv, texture, dim) = (backbuffer.rtv.as_ptr(), backbuffer.tex.as_ptr(), backbuffer.dim);
    draw_frame(graphics, state, constants, rtv, dim);
    draw_caption(graphics, texture, &caption_for(window, &FrameTarget::current(window)));
    graphics.save_last_frame();
}

fn show_placeholder(
    window: &Window,
    graphics: &GraphicsD3D11,
    state: &ViewerState,
    constants: &Constants,
    target: &FrameTarget,
    content: PlaceholderContent,
) -> Option<Placeholder> {
    let client_rect = &target.client_rect;
    let (w, h) = client_rect.dim();
    let mut target_constants = constants.clone();
    update_view_constants(
        &mut target_constants,
        state,
        (w, h),
        window.content_offset_at(client_rect),
    );
    let caption = caption_for(window, target);

    let rect = ScreenRect {
        x: client_rect.left,
        y: client_rect.top,
        w,
        h,
    };
    let draw = |rtv, texture, dim| match content {
        PlaceholderContent::NextFrame => {
            draw_frame(graphics, state, &target_constants, rtv, dim);
            draw_caption(graphics, texture, &caption);
        }
        PlaceholderContent::LastFrame => {
            if !graphics.copy_last_frame(texture, dim) {
                warn!("Last frame unavailable");
            }
        }
    };
    match Placeholder::show(window.hwnd as _, graphics, &rect, draw) {
        Ok(placeholder) => Some(placeholder),
        Err(e) => {
            warn!("Placeholder failed: {e}");
            None
        }
    }
}

fn rect_contains(outer: &RECT, inner: &RECT) -> bool {
    outer.left <= inner.left
        && outer.top <= inner.top
        && outer.right >= inner.right
        && outer.bottom >= inner.bottom
}

fn rect_intersection(a: &RECT, b: &RECT) -> RECT {
    RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    }
}

fn present_window_change(
    window: &mut Window,
    graphics: &mut GraphicsD3D11,
    state: &mut ViewerState,
    constants: &mut Constants,
    set_view: &dyn Fn(&mut ViewerState, float2),
    placeholder: Option<Placeholder>,
) {
    set_view(state, window.center_content());
    graphics.update_backbuffer(window.hwnd);
    update_view_constants(constants, state, window.window_dim, window.content_offset());
    draw_to_back_buffer(graphics, state, constants, window);
    graphics.present(1);

    if placeholder.is_some() {
        if !graphics.wait_for_gpu() {
            warn!("Frame timed out on the GPU");
        }

        wait_for_composition();
    }
}

// Keeps every composed frame showing one complete window. A growing window is covered by its
// next frame and a shrinking window by its last frame. Other changes shrink to the overlap first.
fn change_window_geometry(
    window: &mut Window,
    graphics: &mut GraphicsD3D11,
    state: &mut ViewerState,
    constants: &mut Constants,
    target: &FrameTarget,
    set_view: &dyn Fn(&mut ViewerState, float2),
    change: &mut dyn FnMut(&mut Window),
) {
    set_transitions_disabled(window.hwnd as _, true);
    change_window_geometry_steps(window, graphics, state, constants, target, set_view, change);
    set_transitions_disabled(window.hwnd as _, false);
}

fn change_window_geometry_steps(
    window: &mut Window,
    graphics: &mut GraphicsD3D11,
    state: &mut ViewerState,
    constants: &mut Constants,
    target: &FrameTarget,
    set_view: &dyn Fn(&mut ViewerState, float2),
    change: &mut dyn FnMut(&mut Window),
) {
    let old = window.client_rect;
    let new = target.client_rect;
    let hidden = unsafe { IsIconic(window.hwnd) != 0 || IsWindowVisible(window.hwnd) == 0 };

    if hidden || rects_equal(&new, &old) {
        change(window);
        present_window_change(window, graphics, state, constants, set_view, None);
        return;
    }

    if rect_contains(&new, &old) {
        set_view(state, window.center_content_at(&new, target.caption_height));
        let placeholder = show_placeholder(
            window,
            graphics,
            state,
            constants,
            target,
            PlaceholderContent::NextFrame,
        );
        change(window);
        present_window_change(window, graphics, state, constants, set_view, placeholder);
        return;
    }

    let (old_w, old_h) = old.dim();
    let has_last_frame = graphics.has_last_frame((old_w as u32, old_h as u32));
    if rect_contains(&old, &new) && has_last_frame {
        let current = FrameTarget::current(window);
        let placeholder = show_placeholder(
            window,
            graphics,
            state,
            constants,
            &current,
            PlaceholderContent::LastFrame,
        );
        change(window);
        present_window_change(window, graphics, state, constants, set_view, placeholder);
        return;
    }

    let overlap = rect_intersection(&old, &new);
    let insets = caption::frame_insets(window.window_style);
    let overlap_window = caption::client_to_window(overlap, &insets);
    let (overlap_w, overlap_h) = overlap_window.dim();
    let normal = !window.full_screen && !window.is_maximized() && target.normal_state();
    let mixed = !rect_contains(&old, &new);
    if mixed && normal && overlap_w >= WINDOW_MIN_WIDTH && overlap_h >= WINDOW_MIN_HEIGHT {
        let overlap_target = FrameTarget {
            client_rect: overlap,
            caption_height: caption::caption_height(&insets, false),
            maximized: false,
        };
        change_window_geometry_steps(
            window,
            graphics,
            state,
            constants,
            &overlap_target,
            set_view,
            &mut |window| window.set_normal_client_rect(&overlap),
        );
        change_window_geometry_steps(window, graphics, state, constants, target, set_view, change);
        return;
    }

    set_view(state, window.center_content_at(&new, target.caption_height));
    let placeholder = show_placeholder(
        window,
        graphics,
        state,
        constants,
        target,
        PlaceholderContent::NextFrame,
    );
    change(window);
    present_window_change(window, graphics, state, constants, set_view, placeholder);
}

fn toggle_full_screen(
    window: &mut Window,
    graphics: &mut GraphicsD3D11,
    state: &mut ViewerState,
    constants: &mut Constants,
) {
    let full_screen = !window.full_screen;
    let (client_rect, caption_height) = window.client_rect_for_full_screen(full_screen);
    let target = FrameTarget {
        client_rect,
        caption_height,
        maximized: false,
    };
    change_window_geometry(
        window,
        graphics,
        state,
        constants,
        &target,
        &|_, _| {},
        &mut |window| window.set_full_screen(full_screen),
    );
}
fn apply_loaded_image(
    state: &mut ViewerState,
    main_window: &mut Window,
    graphics: &mut GraphicsD3D11,
    constants: &mut Constants,
    texture: Texture,
    image_path: Option<PathBuf>,
    image_name: Option<&str>,
) -> (u32, u32) {
    state.save_shown_image_view(main_window, constants.image_dim);

    let placement = if state.texture.is_some() {
        WindowPlacement::KeepCenter
    } else {
        WindowPlacement::Centered
    };
    let dim = texture.dim;
    state.texture = Some(texture);

    let saved_view = image_path
        .as_ref()
        .and_then(|path| state.saved_views.get(path).copied());
    state.shown_image = image_path;

    let image_dim = float2::new(dim.0 as f32, dim.1 as f32);
    let dim_changed = constants.image_dim != image_dim;
    constants.image_dim = image_dim;

    let set_view = |state: &mut ViewerState, window_center: float2| {
        if let Some(view) = saved_view {
            state.set_view(view, window_center);
        } else if dim_changed {
            state.center_image(FLOAT2_ONE, image_dim, window_center);
        }
    };

    if let Some(image_name) = image_name {
        main_window.set_window_name(image_name);
    }

    if dim_changed && !main_window.full_screen && !main_window.is_maximized() {
        let plan = main_window.plan_image_size((dim.0 as i32, dim.1 as i32), placement);
        let target = FrameTarget {
            client_rect: plan.client_rect,
            caption_height: plan.caption_height,
            maximized: plan.normal_window_rect.is_none(),
        };
        change_window_geometry(
            main_window,
            graphics,
            state,
            constants,
            &target,
            &set_view,
            &mut |window| window.set_image_size(&plan),
        );
    } else {
        set_view(state, main_window.center_content());
    }

    dim
}

fn update_dds_view(
    state: &mut ViewerState,
    constants: &mut Constants,
    main_window: &mut Window,
) {
    let dds = state.dds_data.as_ref().unwrap();
    let is_volume = dds.depth > 1;

    // For volume textures, depth shrinks with mip level
    if is_volume {
        let max_depth = dds.depth_at_mip(state.dds_mip);
        if state.dds_slice >= max_depth {
            state.dds_slice = max_depth - 1;
        }
    }

    constants.current_slice = state.dds_slice;
    constants.current_mip = state.dds_mip;
    constants.slice_count = if is_volume {
        dds.depth_at_mip(state.dds_mip)
    } else {
        dds.array_size
    };

    let label = if is_volume {
        format!(
            "DDS depth {}/{} mip {}/{}",
            state.dds_slice + 1, constants.slice_count,
            state.dds_mip + 1, dds.mip_count,
        )
    } else {
        format!(
            "DDS slice {}/{} mip {}/{}",
            state.dds_slice + 1, dds.array_size,
            state.dds_mip + 1, dds.mip_count,
        )
    };
    info!("{}", label);
    main_window.set_window_name(&label);
}

fn main() {
    profiling::register_thread!("main");

    let main_begin_time = Instant::now();

    let mut image_path: Option<PathBuf> = None;
    let mut verbose_log = false;
    let mut console_requested = false;
    let mut device_type = DeviceType::Hardware;

    let (load_req_tx, load_req_rx) = std::sync::mpsc::channel();
    let (image_tx, image_rx) = std::sync::mpsc::channel();

    for arg in std::env::args().skip(1) {
        if arg == "-v" || arg == "--verbose" {
            verbose_log = true;
            continue;
        }
        if arg == "--console" {
            console_requested = true;
            continue;
        }

        if arg == "--warp" {
            device_type = DeviceType::Warp;
            continue;
        }

        if image_path.is_none() {
            let path: PathBuf = arg.into();
            image_path = Some(path.clone());
            load_req_tx.send(path).unwrap();
        }
    }

    maybe_alloc_console(console_requested);
    init_logging(verbose_log);

    let mut state = ViewerState::new();

    let mut main_window: Window = Window::new((500, 500)).unwrap();
    let main_window_handle = main_window.hwnd as u64;
    std::thread::spawn(move || {
        while let Ok(x) = load_req_rx.recv() {
            profiling::scope!("LoadImage");
            let load_begin_time = Instant::now();
            info!("Loading image {:?}", x);
            let img = load_image_with_metadata(&x);
            info!("Time to decode image {} ms", to_milliseconds(load_begin_time.elapsed()));
            let _ = image_tx.send((img, load_begin_time, x));
            unsafe {
                InvalidateRect(main_window_handle as HWND, null_mut(), 1);
            }
        }
        info!("Loading thread done");
    });

    {
        let window_time = Instant::now() - main_begin_time;
        info!("Time to window: {} ms", to_milliseconds(window_time));
    }

    let mut graphics: GraphicsD3D11 = unsafe { GraphicsD3D11::new(main_window.hwnd, device_type).unwrap() };

    // Delay showing the window until the first frame can be drawn to avoid showing default blank frame
    unsafe {
        let hwnd = main_window.hwnd;
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }

    let mut constants = Constants {
        image_dim: FLOAT2_ZERO,
        window_dim: FLOAT2_ZERO,
        mouse_pos: FLOAT2_ZERO,
        mouse_buttons: 0,
        show_transparency: 0,
        xfm_content_to_image_uv: Transform2D::new_identity().into(),
        image_type: 0,
        current_slice: 0,
        current_mip: 0,
        slice_count: 1,
        premultiplied_alpha: 0,
        content_offset: [0, 0],
        _pad: 0,
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
                        warn!("Hitch: {} ms", to_milliseconds(latency));
                        last_verbose_log_time = Instant::now();
                    }
                    let lparam = native_msg.lparam;
                    let wparam = native_msg.wparam;
                    match native_msg.msg {
                        WM_PAINT => {
                            should_draw = true;
                        }
                        WM_MOUSEWHEEL => {
                            let notches =
                                GET_WHEEL_DELTA_WPARAM(wparam) as f32 / WHEEL_DELTA as f32;
                            let level = zoom_level(state.xfm_content_to_image.scale.x) + notches;
                            let cursor = POINT {
                                x: GET_X_LPARAM(lparam),
                                y: GET_Y_LPARAM(lparam),
                            };
                            state.zoom_at(zoom_scale(level), main_window.screen_to_content(cursor));
                            should_draw = true;
                        }
                        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                            state.mouse_pos = decode_mouse_pos(lparam);
                            state.is_dragging = true;
                            state.drag_last_mouse_pos = state.mouse_pos_content(&main_window);
                        }
                        WM_LBUTTONUP | WM_CAPTURECHANGED => {
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
                            constants.mouse_pos = state.mouse_pos;
                            if state.is_dragging {
                                let mouse_pos_content = state.mouse_pos_content(&main_window);
                                let drag_delta = state.drag_last_mouse_pos - mouse_pos_content;
                                state.xfm_content_to_image.offset +=
                                    drag_delta.mul_element_wise(state.xfm_content_to_image.scale);
                                state.drag_last_mouse_pos = mouse_pos_content;
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
                                    state.center_image(
                                        FLOAT2_ONE,
                                        constants.image_dim,
                                        main_window.center_content(),
                                    );
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
                                    toggle_full_screen(
                                        &mut main_window,
                                        &mut graphics,
                                        &mut state,
                                        &mut constants,
                                    );
                                }
                                (_, '1') | (_, '2') | (_, '3') | (_, '4') | (_, '5') => {
                                    let octave = (wparam as u8 - b'1') as f32;
                                    state.zoom_at(
                                        zoom_scale(octave * ZOOM_STEPS_PER_OCTAVE),
                                        main_window.zoom_anchor(),
                                    );
                                }
                                (_, 'V') if ctrl_down => {
                                    if let Ok(Some(path)) = get_clipboard_file_path() {
                                        image_path = Some(path.clone());
                                        load_req_tx.send(path).unwrap();
                                    } else if let Ok(Some(img)) = get_clipboard_image() {
                                        image_path = None;
                                        state.dds_data = None;
                                        state.dds_slice = 0;
                                        state.dds_mip = 0;
                                        constants.image_type = 0;
                                        constants.current_slice = 0;
                                        constants.current_mip = 0;
                                        constants.slice_count = 1;
                                        constants.premultiplied_alpha = 0;
                                        if let Some(texture) = Texture::new(&graphics.device, img) {
                                            let dim = apply_loaded_image(
                                                &mut state,
                                                &mut main_window,
                                                &mut graphics,
                                                &mut constants,
                                                texture,
                                                None,
                                                Some("Clipboard Image"),
                                            );
                                            info!(
                                                "Loaded clipboard image {:?}x{:?}",
                                                dim.0, dim.1
                                            );
                                        } else {
                                            error!("Failed to create texture from clipboard image");
                                        }
                                    }
                                }
                                (_, 'C') | (VK_INSERT, _) if ctrl_down => {
                                    main_window.clipboard_save();
                                }
                                (_, 'T') => {
                                    state.show_transparency = !state.show_transparency;
                                    constants.show_transparency = state.show_transparency as u32;
                                }
                                (VK_UP, _) if ctrl_down && state.dds_data.is_some() => {
                                    if state.dds_mip > 0 {
                                        state.dds_mip -= 1;
                                        update_dds_view(
                                            &mut state, &mut constants, &mut main_window,
                                        );
                                    }
                                }
                                (VK_DOWN, _) if ctrl_down && state.dds_data.is_some() => {
                                    let mip_count = state.dds_data.as_ref().unwrap().mip_count;
                                    if state.dds_mip + 1 < mip_count {
                                        state.dds_mip += 1;
                                        update_dds_view(
                                            &mut state, &mut constants, &mut main_window,
                                        );
                                    }
                                }
                                (VK_UP, _) if state.dds_data.is_some() => {
                                    let dds = state.dds_data.as_ref().unwrap();
                                    let total = if dds.depth > 1 {
                                        dds.depth_at_mip(state.dds_mip)
                                    } else {
                                        dds.array_size
                                    };
                                    if state.dds_slice > 0 {
                                        state.dds_slice -= 1;
                                    } else {
                                        state.dds_slice = total - 1;
                                    }
                                    update_dds_view(
                                        &mut state, &mut constants, &mut main_window,
                                    );
                                }
                                (VK_DOWN, _) if state.dds_data.is_some() => {
                                    let dds = state.dds_data.as_ref().unwrap();
                                    let total = if dds.depth > 1 {
                                        dds.depth_at_mip(state.dds_mip)
                                    } else {
                                        dds.array_size
                                    };
                                    state.dds_slice = (state.dds_slice + 1) % total;
                                    update_dds_view(
                                        &mut state, &mut constants, &mut main_window,
                                    );
                                }
                                _ => {}
                            }
                            should_draw = true;
                        }
                        WM_SIZE | WM_MOVE => {
                            main_window.sync_client_rect(ContentAnchor::FollowWindowMove);
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

        if let Ok((img, load_begin_time, image_filename)) = image_rx.try_recv() {
            if let Ok(img) = img {
                let image_name = image_filename.to_string_lossy().into_owned();
                let texture = match img.image_data {
                    ImageData::Decoded(dynamic_image) => {
                        state.dds_data = None;
                        state.dds_slice = 0;
                        state.dds_mip = 0;
                        constants.image_type = 0;
                        constants.current_slice = 0;
                        constants.current_mip = 0;
                        constants.slice_count = 1;
                        constants.premultiplied_alpha = 0;
                        Texture::new(&graphics.device, dynamic_image)
                    }
                    ImageData::Dds(dds_data) => {
                        let tex = Texture::from_dds(&graphics.device, &dds_data);
                        let is_volume = dds_data.depth > 1;
                        constants.image_type = if is_volume { 1 } else { 0 };
                        constants.current_slice = 0;
                        constants.current_mip = 0;
                        constants.slice_count = if is_volume {
                            dds_data.depth
                        } else {
                            dds_data.array_size
                        };
                        constants.premultiplied_alpha = dds_data.premultiplied_alpha as u32;
                        state.dds_slice = 0;
                        state.dds_mip = 0;
                        state.dds_data = Some(dds_data);
                        tex
                    }
                };
                let Some(texture) = texture else {
                    error!("Failed to create texture for {:?}", image_filename);
                    continue;
                };
                let image_key =
                    std::fs::canonicalize(&image_filename).unwrap_or(image_filename.clone());
                let dim = apply_loaded_image(
                    &mut state,
                    &mut main_window,
                    &mut graphics,
                    &mut constants,
                    texture,
                    Some(image_key),
                    Some(&image_name),
                );

                if img.exif.is_some()
                    || img.icc_profile.is_some()
                    || img.orientation.is_some()
                    || img.exif_info.is_some()
                    || img.icc_info.is_some()
                {
                    info!(
                        "Metadata: exif={} bytes, icc={} bytes, orientation={:?}",
                        img.exif.as_ref().map_or(0, |v| v.len()),
                        img.icc_profile.as_ref().map_or(0, |v| v.len()),
                        img.orientation
                    );
                }

                if let Some(exif) = &img.exif_info {
                    info!(
                        "EXIF: make={:?}, model={:?}, software={:?}, datetime={:?}, original={:?}, lens={:?}, orientation={:?}",
                        exif.make,
                        exif.model,
                        exif.software,
                        exif.datetime,
                        exif.datetime_original,
                        exif.lens_model,
                        exif.orientation
                    );
                    if let Some(gps) = &exif.gps_iso6709 {
                        info!("EXIF GPS: {}", gps);
                    }
                }

                if let Some(icc) = &img.icc_info {
                    info!(
                        "ICC: size={} bytes, class={}, color_space={}, pcs={}, cmm={}, version={}, platform={}, manufacturer={}, model={}, intent={}, created={}",
                        icc.size,
                        icc.profile_class,
                        icc.color_space,
                        icc.pcs,
                        icc.cmm_type,
                        icc.version,
                        icc.platform,
                        icc.manufacturer,
                        icc.model,
                        icc.rendering_intent,
                        icc.created.as_deref().unwrap_or("n/a")
                    );
                }

                let image_load_time = Instant::now() - load_begin_time;
                info!(
                    "Time to load image {} ms ({:?})",
                    to_milliseconds(image_load_time),
                    dim
                );
            } else {
                error!("Failed to load image: {:?}", img.err());
            };

            unsafe {
                InvalidateRect(main_window_handle as HWND, null_mut(), 1);
            }
        }

        if state.frame_number == 0 {
            let init_time = Instant::now() - main_begin_time;
            info!("Init time: {:.2}ms", to_milliseconds(init_time));
        }

        {
            profiling::scope!("Draw");

            if verbose_log {
                let frame_delta_time = Instant::now() - last_frame_draw_time;
                last_frame_draw_time = Instant::now();
                let draw_time = draw_end_time - draw_begin_time;
                debug!(
                    "Draw dt: {:.2}ms, frame_number: {}, handled_events: {}, draw time: {:.2}ms",
                    to_milliseconds(frame_delta_time),
                    state.frame_number,
                    handled_events,
                    to_milliseconds(draw_time)
                );
            }

            draw_begin_time = Instant::now();

            update_view_constants(
                &mut constants,
                &state,
                main_window.window_dim,
                main_window.content_offset(),
            );
            draw_to_back_buffer(&mut graphics, &state, &constants, &main_window);

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
