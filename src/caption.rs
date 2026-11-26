use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use winapi::shared::minwindef::{LPARAM, LRESULT};
use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::shared::windowsx::{GET_X_LPARAM, GET_Y_LPARAM};
use winapi::um::winuser::{
    AdjustWindowRect, GetClientRect, GetSystemMetrics, IsZoomed, ScreenToClient, HTCAPTION,
    HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON, HTTOP, HTTOPLEFT, HTTOPRIGHT, SM_CYFRAME,
    WS_CAPTION,
};

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED,
    D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::{Interface, PCWSTR, w};

const BUTTON_WIDTH: f32 = 46.0;
const GLYPH_SIZE: f32 = 10.0;
const ICON_SIZE: u32 = 16;
const ICON_LEFT: f32 = 8.0;
const TITLE_LEFT: f32 = 30.0;

struct Palette {
    background: u32,
    text_active: u32,
    text_inactive: u32,
    button_hover: u32,
    button_pressed: u32,
}

const LIGHT_PALETTE: Palette = Palette {
    background: 0xf3f3f3,
    text_active: 0x000000,
    text_inactive: 0x929292,
    button_hover: 0xe5e5e5,
    button_pressed: 0xcccccc,
};

const DARK_PALETTE: Palette = Palette {
    background: 0x202020,
    text_active: 0xffffff,
    text_inactive: 0x717171,
    button_hover: 0x2d2d2d,
    button_pressed: 0x292929,
};

const CLOSE_HOVER: u32 = 0xc42b1c;
const CLOSE_PRESSED: u32 = 0xc83d2f;
const CLOSE_GLYPH_HOT: u32 = 0xffffff;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum CaptionButton {
    None = 0,
    Minimize = 1,
    Maximize = 2,
    Close = 3,
}

impl CaptionButton {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Minimize,
            2 => Self::Maximize,
            3 => Self::Close,
            _ => Self::None,
        }
    }

    pub fn from_hit(hit: isize) -> Self {
        match hit {
            HTMINBUTTON => Self::Minimize,
            HTMAXBUTTON => Self::Maximize,
            HTCLOSE => Self::Close,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn from_system() -> Self {
        let mut apps_use_light_theme: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut apps_use_light_theme as *mut u32 as *mut std::ffi::c_void),
                Some(&mut size),
            )
        };
        if result == ERROR_SUCCESS && apps_use_light_theme == 0 {
            Theme::Dark
        } else {
            Theme::Light
        }
    }

    fn palette(self) -> &'static Palette {
        match self {
            Theme::Light => &LIGHT_PALETTE,
            Theme::Dark => &DARK_PALETTE,
        }
    }
}

pub struct CaptionState {
    hovered: AtomicU8,
    pressed: AtomicU8,
    active: AtomicBool,
    dark: AtomicBool,
}

impl CaptionState {
    pub fn new() -> Self {
        Self {
            hovered: AtomicU8::new(0),
            pressed: AtomicU8::new(0),
            active: AtomicBool::new(true),
            dark: AtomicBool::new(Theme::from_system() == Theme::Dark),
        }
    }

    pub fn theme(&self) -> Theme {
        if self.dark.load(Ordering::Relaxed) {
            Theme::Dark
        } else {
            Theme::Light
        }
    }

    pub fn set_theme(&self, theme: Theme) -> bool {
        let dark = theme == Theme::Dark;
        self.dark.swap(dark, Ordering::Relaxed) != dark
    }

    pub fn hovered(&self) -> CaptionButton {
        CaptionButton::from_u8(self.hovered.load(Ordering::Relaxed))
    }

    pub fn pressed(&self) -> CaptionButton {
        CaptionButton::from_u8(self.pressed.load(Ordering::Relaxed))
    }

    pub fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn set_hovered(&self, button: CaptionButton) -> bool {
        self.hovered.swap(button as u8, Ordering::Relaxed) != button as u8
    }

    pub fn set_pressed(&self, button: CaptionButton) -> bool {
        self.pressed.swap(button as u8, Ordering::Relaxed) != button as u8
    }

    pub fn set_active(&self, active: bool) -> bool {
        self.active.swap(active, Ordering::Relaxed) != active
    }
}

#[derive(Clone, Copy)]
pub struct FrameInsets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub fn has_caption(style: u32) -> bool {
    style & WS_CAPTION == WS_CAPTION
}

pub fn frame_insets(style: u32) -> FrameInsets {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe {
        AdjustWindowRect(&mut r, style, 0);
    }
    FrameInsets {
        left: -r.left,
        top: -r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

// A maximized window extends past the work area by its border on every side.
pub fn caption_height(insets: &FrameInsets, maximized: bool) -> i32 {
    if maximized {
        insets.top - insets.bottom
    } else {
        insets.top
    }
}

pub fn window_to_client(window: RECT, insets: &FrameInsets, maximized: bool) -> RECT {
    RECT {
        left: window.left + insets.left,
        top: window.top + if maximized { insets.bottom } else { 0 },
        right: window.right - insets.right,
        bottom: window.bottom - insets.bottom,
    }
}

pub fn client_to_window(client: RECT, insets: &FrameInsets) -> RECT {
    RECT {
        left: client.left - insets.left,
        top: client.top,
        right: client.right + insets.right,
        bottom: client.bottom + insets.bottom,
    }
}

fn dpi_scale() -> f32 {
    unsafe { GetDpiForSystem() as f32 / 96.0 }
}

fn button_width() -> i32 {
    (BUTTON_WIDTH * dpi_scale()).round() as i32
}

fn button_at(x: i32, client_width: i32) -> CaptionButton {
    let width = button_width();
    if x >= client_width - width {
        CaptionButton::Close
    } else if x >= client_width - 2 * width {
        CaptionButton::Maximize
    } else if x >= client_width - 3 * width {
        CaptionButton::Minimize
    } else {
        CaptionButton::None
    }
}

// Called for WM_NCHITTEST after DefWindowProc returned HTCLIENT.
pub unsafe fn hit_test_client(hwnd: HWND, style: u32, lparam: LPARAM) -> LRESULT {
    unsafe {
        let mut pt = POINT {
            x: GET_X_LPARAM(lparam),
            y: GET_Y_LPARAM(lparam),
        };
        ScreenToClient(hwnd, &mut pt);
        let mut client = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        GetClientRect(hwnd, &mut client);

        let maximized = IsZoomed(hwnd) != 0;
        let resize_band = GetSystemMetrics(SM_CYFRAME);
        if !maximized && pt.y < resize_band {
            let corner = 2 * resize_band;
            return if pt.x < corner {
                HTTOPLEFT
            } else if pt.x >= client.right - corner {
                HTTOPRIGHT
            } else {
                HTTOP
            };
        }

        if pt.y < caption_height(&frame_insets(style), maximized) {
            return match button_at(pt.x, client.right) {
                CaptionButton::Minimize => HTMINBUTTON,
                CaptionButton::Maximize => HTMAXBUTTON,
                CaptionButton::Close => HTCLOSE,
                CaptionButton::None => HTCAPTION,
            };
        }

        HTCLIENT
    }
}

pub struct Caption<'a> {
    pub width: i32,
    pub height: i32,
    pub title: &'a str,
    pub active: bool,
    pub hovered: CaptionButton,
    pub pressed: CaptionButton,
    pub maximized: bool,
    pub theme: Theme,
}

pub struct CaptionRenderer {
    context: ID2D1DeviceContext,
    text_format: IDWriteTextFormat,
    glyph_format: Option<IDWriteTextFormat>,
    icon: ID2D1Bitmap1,
}

fn color(rgb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((rgb >> 8) & 0xff) as f32 / 255.0,
        b: (rgb & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn point(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}

fn icon_pixels() -> Vec<u8> {
    let quantize = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut pixels = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let r = x as f32 / ICON_SIZE as f32;
            let g = y as f32 / ICON_SIZE as f32;
            let b = 1.0 - (r + g) / 2.0;
            pixels.extend_from_slice(&[quantize(b), quantize(g), quantize(r), 255]);
        }
    }
    pixels
}

const GLYPH_FONTS: [PCWSTR; 2] = [
    windows::core::w!("Segoe Fluent Icons"),
    windows::core::w!("Segoe MDL2 Assets"),
];

fn glyph_char(button: CaptionButton, maximized: bool) -> u16 {
    match button {
        CaptionButton::Minimize => 0xE921,
        CaptionButton::Maximize if maximized => 0xE923,
        CaptionButton::Maximize => 0xE922,
        CaptionButton::Close => 0xE8BB,
        CaptionButton::None => 0,
    }
}

unsafe fn glyph_text_format(dwrite: &IDWriteFactory) -> Option<IDWriteTextFormat> {
    unsafe {
        let mut fonts = None;
        dwrite.GetSystemFontCollection(&mut fonts, false).ok()?;
        let fonts = fonts?;
        for family in GLYPH_FONTS {
            let mut index = 0;
            let mut exists = windows::core::BOOL(0);
            if fonts.FindFamilyName(family, &mut index, &mut exists).is_err() || !exists.as_bool() {
                continue;
            }
            let format = dwrite
                .CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT(400),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    GLYPH_SIZE * dpi_scale(),
                    windows::core::w!(""),
                )
                .ok()?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER).ok()?;
            format
                .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
                .ok()?;
            return Some(format);
        }
        None
    }
}

impl CaptionRenderer {
    pub unsafe fn new(device: *mut std::ffi::c_void) -> Option<Self> {
        unsafe {
            let device = ID3D11Device::from_raw_borrowed(&device)?;
            let dxgi_device: IDXGIDevice = device.cast().ok()?;
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let d2d_device = factory.CreateDevice(&dxgi_device).ok()?;
            let context = d2d_device
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .ok()?;
            context.SetDpi(96.0, 96.0);
            context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            let mut metrics = NONCLIENTMETRICSW {
                cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
                ..Default::default()
            };
            SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                Some(&mut metrics as *mut _ as _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .ok()?;
            let font = &metrics.lfCaptionFont;
            let font_size = (font.lfHeight.unsigned_abs() as f32).max(1.0);

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let text_format = dwrite
                .CreateTextFormat(
                    PCWSTR(font.lfFaceName.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT(font.lfWeight),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    font_size,
                    windows::core::w!(""),
                )
                .ok()?;
            text_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
            text_format
                .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
                .ok()?;
            let ellipsis = dwrite.CreateEllipsisTrimmingSign(&text_format).ok()?;
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            text_format.SetTrimming(&trimming, &ellipsis).ok()?;

            let pixels = icon_pixels();
            let icon_props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let icon = context
                .CreateBitmap(
                    D2D_SIZE_U {
                        width: ICON_SIZE,
                        height: ICON_SIZE,
                    },
                    Some(pixels.as_ptr() as _),
                    ICON_SIZE * 4,
                    &icon_props,
                )
                .ok()?;

            let glyph_format = glyph_text_format(&dwrite);

            Some(Self {
                context,
                text_format,
                glyph_format,
                icon,
            })
        }
    }

    pub unsafe fn draw(&self, target: *mut std::ffi::c_void, caption: &Caption) -> bool {
        if caption.height <= 0 {
            return true;
        }

        unsafe {
            let Some(texture) = ID3D11Texture2D::from_raw_borrowed(&target) else {
                return false;
            };
            let Ok(surface) = texture.cast::<IDXGISurface>() else {
                return false;
            };
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let Ok(bitmap) = self.context.CreateBitmapFromDxgiSurface(&surface, Some(&props)) else {
                return false;
            };

            self.context.SetTarget(&bitmap);
            self.context.BeginDraw();
            self.draw_caption(caption);
            let result = self.context.EndDraw(None, None);
            self.context.SetTarget(None);
            result.is_ok()
        }
    }

    unsafe fn brush(&self, rgb: u32) -> Option<ID2D1SolidColorBrush> {
        unsafe { self.context.CreateSolidColorBrush(&color(rgb), None).ok() }
    }

    unsafe fn draw_caption(&self, caption: &Caption) {
        unsafe {
            let s = dpi_scale();
            let w = caption.width as f32;
            let h = caption.height as f32;
            let ctx = &self.context;
            let palette = caption.theme.palette();

            let Some(background) = self.brush(palette.background) else {
                return;
            };
            ctx.FillRectangle(&rect(0.0, 0.0, w, h), &background);

            let icon_size = (ICON_SIZE as f32 * s).round();
            let icon_left = (ICON_LEFT * s).round();
            let icon_top = ((h - icon_size) / 2.0).floor();
            ctx.DrawBitmap(
                &self.icon,
                Some(&rect(icon_left, icon_top, icon_left + icon_size, icon_top + icon_size)),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );

            let text_rgb = if caption.active {
                palette.text_active
            } else {
                palette.text_inactive
            };
            let button_w = button_width() as f32;
            let buttons_left = w - 3.0 * button_w;
            let title: Vec<u16> = caption.title.encode_utf16().collect();
            if let Some(text) = self.brush(text_rgb) {
                let title_left = (TITLE_LEFT * s).round();
                if buttons_left > title_left {
                    ctx.DrawText(
                        &title,
                        &self.text_format,
                        &rect(title_left, 0.0, buttons_left, h),
                        &text,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }

            let buttons = [
                CaptionButton::Minimize,
                CaptionButton::Maximize,
                CaptionButton::Close,
            ];
            for (i, button) in buttons.iter().copied().enumerate() {
                let left = buttons_left + i as f32 * button_w;
                let hot = caption.hovered == button || caption.pressed == button;
                let pressed = caption.pressed == button && caption.hovered == button;
                let fill = match (button, pressed, hot) {
                    (CaptionButton::Close, true, _) => Some(CLOSE_PRESSED),
                    (CaptionButton::Close, false, true) => Some(CLOSE_HOVER),
                    (_, true, _) => Some(palette.button_pressed),
                    (_, false, true) => Some(palette.button_hover),
                    _ => None,
                };
                if let Some(fill) = fill.and_then(|rgb| self.brush(rgb)) {
                    ctx.FillRectangle(&rect(left, 0.0, left + button_w, h), &fill);
                }

                let glyph_rgb = if button == CaptionButton::Close && hot {
                    CLOSE_GLYPH_HOT
                } else {
                    text_rgb
                };
                let Some(glyph) = self.brush(glyph_rgb) else {
                    continue;
                };
                if let Some(format) = &self.glyph_format {
                    let glyph_char = [glyph_char(button, caption.maximized)];
                    let nudge = if button == CaptionButton::Minimize { s.round() } else { 0.0 };
                    ctx.DrawText(
                        &glyph_char,
                        format,
                        &rect(left + nudge, nudge, left + button_w + nudge, h - s.round() + nudge),
                        &glyph,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                    continue;
                }

                let size = (GLYPH_SIZE * s).round();
                let stroke = s.round().max(1.0);
                let x0 = left + ((button_w - size) / 2.0).floor();
                let y0 = ((h - size) / 2.0).floor();
                let half = stroke / 2.0;
                match button {
                    CaptionButton::Minimize => {
                        ctx.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
                        let y = y0 + (size / 2.0).floor() + half;
                        ctx.DrawLine(point(x0, y), point(x0 + size, y), &glyph, stroke, None);
                    }
                    CaptionButton::Maximize if caption.maximized => {
                        ctx.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
                        let offset = (2.0 * s).round();
                        let front = size - offset;
                        ctx.DrawRectangle(
                            &rect(x0 + half, y0 + offset + half, x0 + front - half, y0 + size - half),
                            &glyph,
                            stroke,
                            None,
                        );
                        let back_left = x0 + offset + half;
                        let back_top = y0 + half;
                        let back_right = x0 + size - half;
                        let back_bottom = y0 + front - half;
                        ctx.DrawLine(point(back_left, y0 + offset), point(back_left, back_top), &glyph, stroke, None);
                        ctx.DrawLine(point(back_left - half, back_top), point(back_right + half, back_top), &glyph, stroke, None);
                        ctx.DrawLine(point(back_right, back_top), point(back_right, back_bottom + half), &glyph, stroke, None);
                        ctx.DrawLine(point(back_right + half, back_bottom), point(x0 + front, back_bottom), &glyph, stroke, None);
                    }
                    CaptionButton::Maximize => {
                        ctx.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
                        ctx.DrawRectangle(
                            &rect(x0 + half, y0 + half, x0 + size - half, y0 + size - half),
                            &glyph,
                            stroke,
                            None,
                        );
                    }
                    CaptionButton::Close => {
                        ctx.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                        ctx.DrawLine(point(x0, y0), point(x0 + size, y0 + size), &glyph, stroke, None);
                        ctx.DrawLine(point(x0 + size, y0), point(x0, y0 + size), &glyph, stroke, None);
                    }
                    CaptionButton::None => {}
                }
            }
            ctx.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
    }
}
