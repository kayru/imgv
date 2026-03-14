use cgmath::{assert_ulps_eq, prelude::*};
use com_ptr::{hresult, ComPtr};
use log::{debug, info};
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

use crate::window::get_window_client_rect_dimensions;
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
    pub mouse_pos: float2,
    pub mouse_buttons: u32,
    pub show_transparency: u32,
    pub xfm_viewport_to_image_uv: float4,
    pub image_type: u32,     // 0 = 2d/2darray, 1 = 3d
    pub current_slice: u32,
    pub current_mip: u32,
    pub slice_count: u32,
    pub premultiplied_alpha: u32,
    pub _pad: [u32; 3],
}

#[allow(clippy::manual_is_multiple_of)]
const _: () = assert!(
    std::mem::size_of::<Constants>() % 16 == 0,
    "Constants must be a multiple of 16 bytes for D3D11 constant buffers"
);

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
    pub dummy_srv_2d_array: ComPtr<ID3D11ShaderResourceView>,
    pub dummy_srv_3d: ComPtr<ID3D11ShaderResourceView>,
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
                    info!("IDXGISwapChain2 waitable object available");
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
                info!("D3D debug layer active");
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
                let smp_desc = smp_desc_base;
                let hr = device.CreateSamplerState(&smp_desc, &mut smp_linear);
                assert!(hr == S_OK);
            }

            {
                let mut smp_desc = smp_desc_base;
                smp_desc.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
                let hr = device.CreateSamplerState(&smp_desc, &mut smp_point);
                assert!(hr == S_OK);
            }
        }

        // Create 1x1 dummy textures for unused shader resource slots
        let dummy_srv_2d_array = {
            let mut tex: *mut ID3D11Texture2D = null_mut();
            let mut srv: *mut ID3D11ShaderResourceView = null_mut();
            let pixel: [u8; 4] = [0, 0, 0, 0];
            let desc = D3D11_TEXTURE2D_DESC {
                Width: 1, Height: 1, MipLevels: 1, ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_SHADER_RESOURCE,
                CPUAccessFlags: 0, MiscFlags: 0,
            };
            let data = D3D11_SUBRESOURCE_DATA {
                pSysMem: pixel.as_ptr() as _, SysMemPitch: 4, SysMemSlicePitch: 0,
            };
            let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
                u: {
                    let mut u: D3D11_SHADER_RESOURCE_VIEW_DESC_u = std::mem::zeroed();
                    *u.Texture2DArray_mut() = D3D11_TEX2D_ARRAY_SRV {
                        MostDetailedMip: 0, MipLevels: 1,
                        FirstArraySlice: 0, ArraySize: 1,
                    };
                    u
                },
            };
            device.CreateTexture2D(&desc, &data, &mut tex);
            device.CreateShaderResourceView(tex as *mut ID3D11Resource, &srv_desc, &mut srv);
            if !tex.is_null() { (*tex).Release(); }
            ComPtr::from_raw(srv)
        };

        let dummy_srv_3d = {
            let mut tex: *mut ID3D11Texture3D = null_mut();
            let mut srv: *mut ID3D11ShaderResourceView = null_mut();
            let pixel: [u8; 4] = [0, 0, 0, 0];
            let desc = D3D11_TEXTURE3D_DESC {
                Width: 1, Height: 1, Depth: 1, MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_SHADER_RESOURCE,
                CPUAccessFlags: 0, MiscFlags: 0,
            };
            let data = D3D11_SUBRESOURCE_DATA {
                pSysMem: pixel.as_ptr() as _, SysMemPitch: 4, SysMemSlicePitch: 4,
            };
            device.CreateTexture3D(&desc, &data, &mut tex);
            device.CreateShaderResourceView(tex as *mut ID3D11Resource, null_mut(), &mut srv);
            if !tex.is_null() { (*tex).Release(); }
            ComPtr::from_raw(srv)
        };

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
            dummy_srv_2d_array,
            dummy_srv_3d,
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

        debug!("update_backbuffer {:?}", new_dim);

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

pub struct DdsTextureData {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub mip_count: u32,
    pub array_size: u32,
    pub dxgi_format: u32,
    pub block_dim: u32,       // 4 for BCn, 1 for uncompressed
    pub block_byte_size: u32, // bytes per block (BCn) or per pixel (uncompressed)
    pub premultiplied_alpha: bool,
    pub data: Vec<u8>,
}

pub struct DdsSlice {
    pub width: u32,
    pub height: u32,
    pub dxgi_format: u32,
    pub data_offset: usize,
    pub row_pitch: u32,
}

impl DdsTextureData {
    /// Compute the byte size of one 2D slice (one depth/array element) at a given mip.
    fn mip_slice_size(&self, mip: u32) -> usize {
        let mw = 1.max(self.width >> mip);
        let mh = 1.max(self.height >> mip);
        let bw = mw.div_ceil(self.block_dim) as usize;
        let bh = mh.div_ceil(self.block_dim) as usize;
        bw * bh * self.block_byte_size as usize
    }

    /// Row pitch in bytes at a given mip level.
    fn mip_row_pitch(&self, mip: u32) -> u32 {
        let mw = 1.max(self.width >> mip);
        let bw = mw.div_ceil(self.block_dim);
        bw * self.block_byte_size
    }

    pub fn get_slice(&self, array_or_depth: u32, mip: u32) -> Option<DdsSlice> {
        if mip >= self.mip_count {
            return None;
        }

        let is_volume = self.depth > 1;

        if is_volume {
            // Volume texture: data is laid out as all depth slices for mip 0,
            // then all (halved) depth slices for mip 1, etc.
            // All in a single contiguous block (array_size == 1 for volumes).
            let mut offset: usize = 0;
            for m in 0..mip {
                let md = 1.max(self.depth >> m) as usize;
                offset += self.mip_slice_size(m) * md;
            }
            let md = 1.max(self.depth >> mip);
            if array_or_depth >= md {
                return None;
            }
            let slice_size = self.mip_slice_size(mip);
            offset += slice_size * array_or_depth as usize;
            if offset + slice_size > self.data.len() {
                return None;
            }
            Some(DdsSlice {
                width: 1.max(self.width >> mip),
                height: 1.max(self.height >> mip),
                dxgi_format: self.dxgi_format,
                data_offset: offset,
                row_pitch: self.mip_row_pitch(mip),
            })
        } else {
            // Array/single texture: data for each array layer contains all mips
            // contiguously. Compute array stride (sum of all mip sizes).
            if array_or_depth >= self.array_size {
                return None;
            }
            let array_stride = self.compute_array_stride();
            let mut offset = array_stride * array_or_depth as usize;
            for m in 0..mip {
                offset += self.mip_slice_size(m);
            }
            let slice_size = self.mip_slice_size(mip);
            if offset + slice_size > self.data.len() {
                return None;
            }
            Some(DdsSlice {
                width: 1.max(self.width >> mip),
                height: 1.max(self.height >> mip),
                dxgi_format: self.dxgi_format,
                data_offset: offset,
                row_pitch: self.mip_row_pitch(mip),
            })
        }
    }

    fn compute_array_stride(&self) -> usize {
        let mut stride: usize = 0;
        for m in 0..self.mip_count {
            stride += self.mip_slice_size(m);
        }
        stride
    }

    pub fn depth_at_mip(&self, mip: u32) -> u32 {
        if self.depth > 1 {
            1.max(self.depth >> mip)
        } else {
            1
        }
    }
}

/// Map TYPELESS formats to a default typed format for SRV creation.
/// D3D11 requires SRVs to use typed formats even if the resource is TYPELESS.
fn srv_format_for(format: u32) -> u32 {
    match format {
        DXGI_FORMAT_R32G32B32A32_TYPELESS => DXGI_FORMAT_R32G32B32A32_FLOAT,
        DXGI_FORMAT_R32G32B32_TYPELESS => DXGI_FORMAT_R32G32B32_FLOAT,
        DXGI_FORMAT_R16G16B16A16_TYPELESS => DXGI_FORMAT_R16G16B16A16_FLOAT,
        DXGI_FORMAT_R32G32_TYPELESS => DXGI_FORMAT_R32G32_FLOAT,
        DXGI_FORMAT_R10G10B10A2_TYPELESS => DXGI_FORMAT_R10G10B10A2_UNORM,
        DXGI_FORMAT_R8G8B8A8_TYPELESS => DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_R16G16_TYPELESS => DXGI_FORMAT_R16G16_FLOAT,
        DXGI_FORMAT_R32_TYPELESS => DXGI_FORMAT_R32_FLOAT,
        DXGI_FORMAT_R24G8_TYPELESS => DXGI_FORMAT_R24_UNORM_X8_TYPELESS,
        DXGI_FORMAT_R8G8_TYPELESS => DXGI_FORMAT_R8G8_UNORM,
        DXGI_FORMAT_R16_TYPELESS => DXGI_FORMAT_R16_FLOAT,
        DXGI_FORMAT_R8_TYPELESS => DXGI_FORMAT_R8_UNORM,
        DXGI_FORMAT_BC1_TYPELESS => DXGI_FORMAT_BC1_UNORM,
        DXGI_FORMAT_BC2_TYPELESS => DXGI_FORMAT_BC2_UNORM,
        DXGI_FORMAT_BC3_TYPELESS => DXGI_FORMAT_BC3_UNORM,
        DXGI_FORMAT_BC4_TYPELESS => DXGI_FORMAT_BC4_UNORM,
        DXGI_FORMAT_BC5_TYPELESS => DXGI_FORMAT_BC5_UNORM,
        DXGI_FORMAT_B8G8R8A8_TYPELESS => DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_FORMAT_B8G8R8X8_TYPELESS => DXGI_FORMAT_B8G8R8X8_UNORM,
        DXGI_FORMAT_BC6H_TYPELESS => DXGI_FORMAT_BC6H_UF16,
        DXGI_FORMAT_BC7_TYPELESS => DXGI_FORMAT_BC7_UNORM,
        other => other,
    }
}

pub enum ImageType {
    Image2DArray, // t0
    Volume,       // t1
}

pub struct Texture {
    pub srv: ComPtr<ID3D11ShaderResourceView>,
    pub dim: (u32, u32),
    pub image_type: ImageType,
}

impl Texture {
    pub fn new(device: &ComPtr<ID3D11Device>, image: image::DynamicImage) -> Option<Self> {
        let img_buf = image.into_rgba8();
        let dim = img_buf.dimensions();
        let img_container = img_buf.as_raw();
        let subresource = D3D11_SUBRESOURCE_DATA {
            pSysMem: img_container.as_ptr() as *mut c_void,
            SysMemPitch: 4 * dim.0,
            SysMemSlicePitch: 0,
        };
        Self::create_texture_2d_array(device, dim, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, &[subresource])
    }

    pub fn from_dds(device: &ComPtr<ID3D11Device>, dds: &DdsTextureData) -> Option<Self> {
        if dds.depth > 1 {
            Self::create_texture_3d(device, dds)
        } else {
            Self::create_texture_2d_array_from_dds(device, dds)
        }
    }

    fn create_texture_2d_array_from_dds(
        device: &ComPtr<ID3D11Device>,
        dds: &DdsTextureData,
    ) -> Option<Self> {
        // Subresources: for each array slice, then for each mip level
        // D3D11 expects subresource index = mip + (array_index * mip_count)
        let mut subresources = Vec::new();
        for array_idx in 0..dds.array_size {
            for mip in 0..dds.mip_count {
                let slice = dds.get_slice(array_idx, mip)?;
                subresources.push(D3D11_SUBRESOURCE_DATA {
                    pSysMem: dds.data[slice.data_offset..].as_ptr() as *mut c_void,
                    SysMemPitch: slice.row_pitch,
                    SysMemSlicePitch: 0,
                });
            }
        }
        Self::create_texture_2d_array(
            device,
            (dds.width, dds.height),
            dds.array_size,
            dds.mip_count,
            dds.dxgi_format,
            &subresources,
        )
    }

    fn create_texture_2d_array(
        device: &ComPtr<ID3D11Device>,
        dim: (u32, u32),
        array_size: u32,
        mip_levels: u32,
        format: u32,
        subresources: &[D3D11_SUBRESOURCE_DATA],
    ) -> Option<Self> {
        let mut tex: *mut ID3D11Texture2D = null_mut();
        let mut srv: *mut ID3D11ShaderResourceView = null_mut();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: dim.0,
            Height: dim.1,
            MipLevels: mip_levels,
            ArraySize: array_size,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: srv_format_for(format),
            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
            u: unsafe {
                let mut u: D3D11_SHADER_RESOURCE_VIEW_DESC_u = std::mem::zeroed();
                *u.Texture2DArray_mut() = D3D11_TEX2D_ARRAY_SRV {
                    MostDetailedMip: 0,
                    MipLevels: mip_levels,
                    FirstArraySlice: 0,
                    ArraySize: array_size,
                };
                u
            },
        };
        unsafe {
            device.CreateTexture2D(&desc, subresources.as_ptr(), &mut tex);
            if tex.is_null() {
                return None;
            }
            device.CreateShaderResourceView(
                tex as *mut ID3D11Resource,
                &srv_desc,
                &mut srv,
            );
            (*tex).Release();
            if srv.is_null() {
                return None;
            }
        };
        Some(Self {
            srv: unsafe { ComPtr::from_raw(srv) },
            dim,
            image_type: ImageType::Image2DArray,
        })
    }

    fn create_texture_3d(device: &ComPtr<ID3D11Device>, dds: &DdsTextureData) -> Option<Self> {
        // For 3D textures, subresources are indexed by mip level only.
        // Each mip's data contains all depth slices contiguously.
        let mut subresources = Vec::new();
        for mip in 0..dds.mip_count {
            let row_pitch = dds.mip_row_pitch(mip);
            let slice_pitch = dds.mip_slice_size(mip) as u32;
            let first_depth_slice = dds.get_slice(0, mip)?;
            subresources.push(D3D11_SUBRESOURCE_DATA {
                pSysMem: dds.data[first_depth_slice.data_offset..].as_ptr() as *mut c_void,
                SysMemPitch: row_pitch,
                SysMemSlicePitch: slice_pitch,
            });
        }

        let mut tex: *mut ID3D11Texture3D = null_mut();
        let mut srv: *mut ID3D11ShaderResourceView = null_mut();
        let desc = D3D11_TEXTURE3D_DESC {
            Width: dds.width,
            Height: dds.height,
            Depth: dds.depth,
            MipLevels: dds.mip_count,
            Format: dds.dxgi_format,
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: srv_format_for(dds.dxgi_format),
            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE3D,
            u: unsafe {
                let mut u: D3D11_SHADER_RESOURCE_VIEW_DESC_u = std::mem::zeroed();
                *u.Texture3D_mut() = D3D11_TEX3D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: dds.mip_count,
                };
                u
            },
        };
        unsafe {
            device.CreateTexture3D(&desc, subresources.as_ptr(), &mut tex);
            if tex.is_null() {
                return None;
            }
            device.CreateShaderResourceView(
                tex as *mut ID3D11Resource,
                &srv_desc,
                &mut srv,
            );
            (*tex).Release();
            if srv.is_null() {
                return None;
            }
        };
        Some(Self {
            srv: unsafe { ComPtr::from_raw(srv) },
            dim: (dds.width, dds.height),
            image_type: ImageType::Volume,
        })
    }
}
