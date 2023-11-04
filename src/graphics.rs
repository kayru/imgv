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
use winapi::shared::windef::{HBRUSH, HICON, HMENU, HWND, POINT, RECT};
use winapi::shared::windowsx::{GET_X_LPARAM, GET_Y_LPARAM};
use winapi::shared::winerror::S_OK;
use winapi::um::d3d11::*;
use winapi::um::d3d11sdklayers::*;
use winapi::um::d3dcommon::*;
use winapi::um::shellscalingapi::SetProcessDpiAwareness;
use winapi::um::winuser::*;
use winapi::Interface;

use crate::get_window_client_rect_dimensions;
use crate::math::*;

const NUM_BACK_BUFFERS: u32 = 3;
const BACK_BUFFER_FORMAT: u32 = DXGI_FORMAT_B8G8R8A8_UNORM;
const SWAP_CHAIN_FLAGS: u32 =
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT | DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING;

const DXGI_MWA_NO_WINDOW_CHANGES: UINT = 1;
const DXGI_MWA_NO_ALT_ENTER: UINT = 2;
const DXGI_MWA_NO_PRINT_SCREEN: UINT = 4;

// TODO: can we generate this based on shader reflection or inject into shader code from rust?
#[repr(C)]
#[derive(Clone)]
pub struct Constants {
    pub image_dim: float2,
    pub window_dim: float2,
    pub mouse: float4, // float2 xy pos, uint buttons, uint unused
    pub xfm_viewport_to_image_uv: float4,
}

pub struct BackBuffer {
    pub rtv: ComPtr<ID3D11RenderTargetView>,
    pub tex: ComPtr<ID3D11Texture2D>,
    pub dim: (u32, u32),
}

pub struct GraphicsD3D11 {
    pub device: ComPtr<ID3D11Device>,
    info_queue: Option<ComPtr<ID3D11InfoQueue>>,
    pub context: ComPtr<ID3D11DeviceContext>,
    swapchain: ComPtr<IDXGISwapChain1>,
    pub backbuffer: Option<BackBuffer>,
    pub blit_vs: ComPtr<ID3D11VertexShader>,
    pub blit_ps: ComPtr<ID3D11PixelShader>,
    pub constants: ComPtr<ID3D11Buffer>,
    pub smp_linear: ComPtr<ID3D11SamplerState>,
    pub smp_point: ComPtr<ID3D11SamplerState>,
    swap_chain_waitable: Option<winapi::shared::ntdef::HANDLE>,
    frame_statistics: DXGI_FRAME_STATISTICS,
}

impl Drop for GraphicsD3D11 {
    fn drop(&mut self) {
        if let Some(h) = self.swap_chain_waitable {
            unsafe {
                winapi::um::handleapi::CloseHandle(h);
            }
        }
    }
}

impl GraphicsD3D11 {
    pub unsafe fn new(hwnd: HWND) -> Result<Self, ()> {
        let device_flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | {
            D3D11_CREATE_DEVICE_DEBUG * cfg!(debug_assertions) as u32
        };

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
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            //SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
            Flags: SWAP_CHAIN_FLAGS,
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
        let swap_chain_waitable =
            if let Ok(swapchain2) = swapchain.query_interface::<IDXGISwapChain2>() {
                let h = swapchain2.GetFrameLatencyWaitableObject();
                if h == winapi::um::handleapi::INVALID_HANDLE_VALUE {
                    None
                } else {
                    println!("IDXGISwapChain2 waitable object available");
                    Some(h)
                }
            } else {
                None
            };

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
            swap_chain_waitable,
            frame_statistics: std::mem::zeroed(),
        };

        result.update_backbuffer(hwnd);

        Ok(result)
    }

    pub fn update_backbuffer(&mut self, hwnd: HWND) {
        let mut new_dim = get_window_client_rect_dimensions(hwnd);

        new_dim.0 = align_up(new_dim.0, 512);
        new_dim.1 = align_up(new_dim.1, 512);

        if let Some(backbuffer) = &self.backbuffer {
            if backbuffer.dim.0 as i32 >= new_dim.0 && backbuffer.dim.1 as i32 >= new_dim.1 {
                return;
            }
        }

        // Release old render target view before resizing back buffer
        self.backbuffer = None;

        assert!(new_dim.0 < 16384);
        assert!(new_dim.1 < 16384);

        println!("update_backbuffer {:?}", new_dim);

        let hr: HRESULT = unsafe {
            self.swapchain.ResizeBuffers(
                NUM_BACK_BUFFERS,
                new_dim.0 as u32,
                new_dim.1 as u32,
                BACK_BUFFER_FORMAT,
                SWAP_CHAIN_FLAGS,
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

    fn wait_for_swap_chain(&self) {
        if let Some(h) = self.swap_chain_waitable {
            unsafe {
                winapi::um::synchapi::WaitForSingleObject(h, 0xFFFFFFFF);
            }
        }
    }

    pub fn present(&mut self, sync_interval: u32) {
        profiling::scope!("Present");
        unsafe {
            let params = DXGI_PRESENT_PARAMETERS {
                DirtyRectsCount: 0,
                pDirtyRects: null_mut(),
                pScrollRect: null_mut(),
                pScrollOffset: null_mut(),
            };
            let flags = if sync_interval == 0 {
                DXGI_PRESENT_ALLOW_TEARING
            } else {
                0u32
            };
            self.swapchain.Present1(sync_interval, flags, &params);
            self.swapchain
                .GetFrameStatistics(&mut self.frame_statistics);
        }
    }
}

pub struct Texture {
    pub tex: ComPtr<ID3D11Texture2D>,
    pub srv: ComPtr<ID3D11ShaderResourceView>,
    pub dim: (u32, u32),
}

impl Texture {
    pub fn new(device: &ComPtr<ID3D11Device>, image: image::DynamicImage) -> Self {
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
