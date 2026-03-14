use icc_profile::DecodedICCProfile;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, ImageResult};
use log::info;
use nom_exif::{EntryValue, Exif, ExifIter, ExifTag, MediaParser, MediaSource};
use std::path::Path;
use winapi::shared::dxgiformat::*;

use crate::graphics::DdsTextureData;

pub enum ImageData {
    Decoded(DynamicImage),
    Dds(DdsTextureData),
}

pub struct LoadedImage {
    pub image_data: ImageData,
    pub exif: Option<Vec<u8>>,
    pub icc_profile: Option<Vec<u8>>,
    pub orientation: Option<Orientation>,
    pub exif_info: Option<ExifInfo>,
    pub icc_info: Option<IccInfo>,
}

fn dds_pixel_format_to_dxgi(spf: &ddsfile::PixelFormat) -> Option<u32> {
    use ddsfile::PixelFormatFlags;
    let flags = spf.flags;
    let bpp = spf.rgb_bit_count.unwrap_or(0);
    let r = spf.r_bit_mask.unwrap_or(0);
    let g = spf.g_bit_mask.unwrap_or(0);
    let b = spf.b_bit_mask.unwrap_or(0);
    let a = spf.a_bit_mask.unwrap_or(0);

    if flags.contains(PixelFormatFlags::RGB) {
        match (bpp, r, g, b, a) {
            // 32-bit RGBA
            (32, 0x000000FF, 0x0000FF00, 0x00FF0000, 0xFF000000) => Some(DXGI_FORMAT_R8G8B8A8_UNORM),
            (32, 0x00FF0000, 0x0000FF00, 0x000000FF, 0xFF000000) => Some(DXGI_FORMAT_B8G8R8A8_UNORM),
            // 32-bit RGB (no alpha)
            (32, 0x000000FF, 0x0000FF00, 0x00FF0000, 0) => Some(DXGI_FORMAT_R8G8B8A8_UNORM),
            (32, 0x00FF0000, 0x0000FF00, 0x000000FF, 0) => Some(DXGI_FORMAT_B8G8R8X8_UNORM),
            // 32-bit packed
            (32, 0x3FF00000, 0x000FFC00, 0x000003FF, 0xC0000000) => Some(DXGI_FORMAT_R10G10B10A2_UNORM),
            // 16-bit 565
            (16, 0xF800, 0x07E0, 0x001F, 0) => Some(DXGI_FORMAT_B5G6R5_UNORM),
            // 16-bit 5551
            (16, 0x7C00, 0x03E0, 0x001F, 0x8000) => Some(DXGI_FORMAT_B5G5R5A1_UNORM),
            // 16-bit 4444
            (16, 0x0F00, 0x00F0, 0x000F, 0xF000) => Some(DXGI_FORMAT_B4G4R4A4_UNORM),
            // 16-bit RG
            (16, 0x00FF, 0xFF00, 0, 0) => Some(DXGI_FORMAT_R8G8_UNORM),
            _ => {
                info!("DDS: unsupported RGB format bpp={} r=0x{:x} g=0x{:x} b=0x{:x} a=0x{:x}", bpp, r, g, b, a);
                None
            }
        }
    } else if flags.contains(PixelFormatFlags::LUMINANCE) {
        match (bpp, r, a) {
            (8, 0xFF, 0) => Some(DXGI_FORMAT_R8_UNORM),
            (8, 0x0F, 0xF0) => Some(DXGI_FORMAT_R8G8_UNORM),
            (16, 0xFFFF, 0) => Some(DXGI_FORMAT_R16_UNORM),
            (16, 0x00FF, 0xFF00) => Some(DXGI_FORMAT_R8G8_UNORM),
            _ => {
                info!("DDS: unsupported luminance format bpp={} r=0x{:x} a=0x{:x}", bpp, r, a);
                None
            }
        }
    } else if flags.contains(PixelFormatFlags::ALPHA) {
        match bpp {
            8 => Some(DXGI_FORMAT_A8_UNORM),
            _ => None,
        }
    } else if flags.bits() & 0x80000 != 0 {
        // DDPF_BUMPDUDV
        match (bpp, r, g, b, a) {
            (16, 0x00FF, 0xFF00, 0, 0) => Some(DXGI_FORMAT_R8G8_SNORM),
            (32, 0x000000FF, 0x0000FF00, 0x00FF0000, 0xFF000000) => Some(DXGI_FORMAT_R8G8B8A8_SNORM),
            (32, 0x0000FFFF, 0xFFFF0000, 0, 0) => Some(DXGI_FORMAT_R16G16_SNORM),
            _ => {
                info!("DDS: unsupported BUMPDUDV format bpp={}", bpp);
                None
            }
        }
    } else {
        info!("DDS: unsupported pixel format flags=0x{:x}", flags.bits());
        None
    }
}

/// Returns (block_dim, block_byte_size) for a DXGI format.
/// BCn formats use 4x4 blocks, everything else uses 1x1 (per-pixel).
fn dxgi_format_info(format: u32) -> Option<(u32, u32)> {
    Some(match format {
        // BCn compressed: 4x4 blocks
        DXGI_FORMAT_BC1_UNORM | DXGI_FORMAT_BC1_UNORM_SRGB | DXGI_FORMAT_BC1_TYPELESS |
        DXGI_FORMAT_BC4_UNORM | DXGI_FORMAT_BC4_SNORM | DXGI_FORMAT_BC4_TYPELESS
            => (4, 8),
        DXGI_FORMAT_BC2_UNORM | DXGI_FORMAT_BC2_UNORM_SRGB | DXGI_FORMAT_BC2_TYPELESS |
        DXGI_FORMAT_BC3_UNORM | DXGI_FORMAT_BC3_UNORM_SRGB | DXGI_FORMAT_BC3_TYPELESS |
        DXGI_FORMAT_BC5_UNORM | DXGI_FORMAT_BC5_SNORM | DXGI_FORMAT_BC5_TYPELESS |
        DXGI_FORMAT_BC6H_UF16 | DXGI_FORMAT_BC6H_SF16 | DXGI_FORMAT_BC6H_TYPELESS |
        DXGI_FORMAT_BC7_UNORM | DXGI_FORMAT_BC7_UNORM_SRGB | DXGI_FORMAT_BC7_TYPELESS
            => (4, 16),

        // 128-bit (16 bytes/pixel)
        DXGI_FORMAT_R32G32B32A32_FLOAT | DXGI_FORMAT_R32G32B32A32_UINT |
        DXGI_FORMAT_R32G32B32A32_SINT | DXGI_FORMAT_R32G32B32A32_TYPELESS
            => (1, 16),

        // 96-bit (12 bytes/pixel)
        DXGI_FORMAT_R32G32B32_FLOAT | DXGI_FORMAT_R32G32B32_UINT |
        DXGI_FORMAT_R32G32B32_SINT | DXGI_FORMAT_R32G32B32_TYPELESS
            => (1, 12),

        // 64-bit (8 bytes/pixel)
        DXGI_FORMAT_R16G16B16A16_FLOAT | DXGI_FORMAT_R16G16B16A16_UNORM |
        DXGI_FORMAT_R16G16B16A16_UINT | DXGI_FORMAT_R16G16B16A16_SNORM |
        DXGI_FORMAT_R16G16B16A16_SINT | DXGI_FORMAT_R16G16B16A16_TYPELESS |
        DXGI_FORMAT_R32G32_FLOAT | DXGI_FORMAT_R32G32_UINT |
        DXGI_FORMAT_R32G32_SINT | DXGI_FORMAT_R32G32_TYPELESS
            => (1, 8),

        // 32-bit (4 bytes/pixel)
        DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB |
        DXGI_FORMAT_R8G8B8A8_UINT | DXGI_FORMAT_R8G8B8A8_SNORM |
        DXGI_FORMAT_R8G8B8A8_SINT | DXGI_FORMAT_R8G8B8A8_TYPELESS |
        DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB |
        DXGI_FORMAT_B8G8R8A8_TYPELESS |
        DXGI_FORMAT_B8G8R8X8_UNORM | DXGI_FORMAT_B8G8R8X8_UNORM_SRGB |
        DXGI_FORMAT_B8G8R8X8_TYPELESS |
        DXGI_FORMAT_R10G10B10A2_UNORM | DXGI_FORMAT_R10G10B10A2_UINT |
        DXGI_FORMAT_R10G10B10A2_TYPELESS |
        DXGI_FORMAT_R11G11B10_FLOAT |
        DXGI_FORMAT_R9G9B9E5_SHAREDEXP |
        DXGI_FORMAT_R16G16_FLOAT | DXGI_FORMAT_R16G16_UNORM |
        DXGI_FORMAT_R16G16_UINT | DXGI_FORMAT_R16G16_SNORM |
        DXGI_FORMAT_R16G16_SINT | DXGI_FORMAT_R16G16_TYPELESS |
        DXGI_FORMAT_R32_FLOAT | DXGI_FORMAT_R32_UINT |
        DXGI_FORMAT_R32_SINT | DXGI_FORMAT_R32_TYPELESS |
        DXGI_FORMAT_D32_FLOAT |
        DXGI_FORMAT_R24G8_TYPELESS | DXGI_FORMAT_D24_UNORM_S8_UINT |
        DXGI_FORMAT_R24_UNORM_X8_TYPELESS | DXGI_FORMAT_X24_TYPELESS_G8_UINT
            => (1, 4),

        // 16-bit (2 bytes/pixel)
        DXGI_FORMAT_R8G8_UNORM | DXGI_FORMAT_R8G8_UINT |
        DXGI_FORMAT_R8G8_SNORM | DXGI_FORMAT_R8G8_SINT |
        DXGI_FORMAT_R8G8_TYPELESS |
        DXGI_FORMAT_R16_FLOAT | DXGI_FORMAT_R16_UNORM |
        DXGI_FORMAT_R16_UINT | DXGI_FORMAT_R16_SNORM |
        DXGI_FORMAT_R16_SINT | DXGI_FORMAT_R16_TYPELESS |
        DXGI_FORMAT_D16_UNORM |
        DXGI_FORMAT_B5G6R5_UNORM | DXGI_FORMAT_B5G5R5A1_UNORM |
        DXGI_FORMAT_B4G4R4A4_UNORM
            => (1, 2),

        // 8-bit (1 byte/pixel)
        DXGI_FORMAT_R8_UNORM | DXGI_FORMAT_R8_UINT |
        DXGI_FORMAT_R8_SNORM | DXGI_FORMAT_R8_SINT |
        DXGI_FORMAT_R8_TYPELESS |
        DXGI_FORMAT_A8_UNORM
            => (1, 1),

        _ => {
            info!("DDS: unsupported dxgi_format={}", format);
            return None;
        }
    })
}

fn try_load_dds(path: &Path) -> Option<DdsTextureData> {
    let file_data = std::fs::read(path).ok()?;
    let dds = ddsfile::Dds::read(&mut std::io::Cursor::new(&file_data)).ok()?;

    let width = dds.get_width();
    let height = dds.get_height();
    let depth = dds.get_depth();
    let mip_count = dds.get_num_mipmap_levels();
    let array_size = dds.get_num_array_layers();

    let mut premultiplied_alpha = false;

    let dxgi_format = if let Some(ref header10) = dds.header10 {
        header10.dxgi_format as u32
    } else if let Some(ref fourcc) = dds.header.spf.fourcc {
        match fourcc.0 {
            ddsfile::FourCC::DXT1 => DXGI_FORMAT_BC1_UNORM,
            ddsfile::FourCC::DXT2 => { premultiplied_alpha = true; DXGI_FORMAT_BC2_UNORM },
            ddsfile::FourCC::DXT3 => DXGI_FORMAT_BC2_UNORM,
            ddsfile::FourCC::DXT4 => { premultiplied_alpha = true; DXGI_FORMAT_BC3_UNORM },
            ddsfile::FourCC::DXT5 => DXGI_FORMAT_BC3_UNORM,
            ddsfile::FourCC::R16F => DXGI_FORMAT_R16_FLOAT,
            ddsfile::FourCC::G16R16F => DXGI_FORMAT_R16G16_FLOAT,
            ddsfile::FourCC::A16B16G16R16F => DXGI_FORMAT_R16G16B16A16_FLOAT,
            ddsfile::FourCC::R32F => DXGI_FORMAT_R32_FLOAT,
            ddsfile::FourCC::G32R32F => DXGI_FORMAT_R32G32_FLOAT,
            ddsfile::FourCC::A32B32G32R32F => DXGI_FORMAT_R32G32B32A32_FLOAT,
            ddsfile::FourCC::A16B16G16R16 => DXGI_FORMAT_R16G16B16A16_UNORM,
            ddsfile::FourCC::Q16W16V16U16 => DXGI_FORMAT_R16G16B16A16_SNORM,
            ddsfile::FourCC::CXV8U8 => DXGI_FORMAT_R8G8_SNORM,
            _ => {
                info!("DDS: unsupported fourcc 0x{:x}", fourcc.0);
                return None;
            }
        }
    } else {
        // Uncompressed with pixel format masks
        dds_pixel_format_to_dxgi(&dds.header.spf)?
    };

    let (block_dim, block_byte_size) = dxgi_format_info(dxgi_format)?;

    let data = dds.data;

    info!(
        "DDS: {}x{}x{} format={} mips={} array={} block_dim={} block_size={}",
        width, height, depth, dxgi_format, mip_count, array_size, block_dim, block_byte_size
    );

    Some(DdsTextureData {
        width,
        height,
        depth,
        mip_count,
        array_size,
        dxgi_format,
        block_dim,
        block_byte_size,
        premultiplied_alpha,
        data,
    })
}

pub fn load_image_with_metadata(path: &Path) -> ImageResult<LoadedImage> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dds")) {
        if let Some(dds_data) = try_load_dds(path) {
            return Ok(LoadedImage {
                image_data: ImageData::Dds(dds_data),
                exif: None,
                icc_profile: None,
                orientation: None,
                exif_info: None,
                icc_info: None,
            });
        }
    }

    let reader = ImageReader::open(path)?;
    let reader = reader.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let exif = decoder.exif_metadata().unwrap_or(None);
    let icc_profile = decoder.icc_profile().unwrap_or(None);
    let orientation = exif
        .as_deref()
        .and_then(Orientation::from_exif_chunk);
    let exif_info = parse_exif_info(path);
    let icc_info = icc_profile.as_deref().and_then(parse_icc_info);
    let image = DynamicImage::from_decoder(decoder)?;
    Ok(LoadedImage {
        image_data: ImageData::Decoded(image),
        exif,
        icc_profile,
        orientation,
        exif_info,
        icc_info,
    })
}

#[derive(Debug, Clone)]
pub struct ExifInfo {
    pub make: Option<String>,
    pub model: Option<String>,
    pub software: Option<String>,
    pub datetime: Option<String>,
    pub datetime_original: Option<String>,
    pub lens_model: Option<String>,
    pub orientation: Option<u16>,
    pub gps_iso6709: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IccInfo {
    pub size: u32,
    pub cmm_type: String,
    pub version: String,
    pub profile_class: String,
    pub color_space: String,
    pub pcs: String,
    pub created: Option<String>,
    pub platform: String,
    pub flags: u32,
    pub manufacturer: String,
    pub model: String,
    pub rendering_intent: u32,
}

fn parse_exif_info(path: &Path) -> Option<ExifInfo> {
    let ms = MediaSource::file_path(path).ok()?;
    if !ms.has_exif() {
        return None;
    }
    let mut parser = MediaParser::new();
    let iter: ExifIter = parser.parse(ms).ok()?;
    let exif: Exif = iter.into();

    let make = exif_string(&exif, ExifTag::Make);
    let model = exif_string(&exif, ExifTag::Model);
    let software = exif_string(&exif, ExifTag::Software);
    let datetime = exif_value_string(&exif, ExifTag::ModifyDate);
    let datetime_original = exif_value_string(&exif, ExifTag::DateTimeOriginal);
    let lens_model = exif_string(&exif, ExifTag::LensModel);
    let orientation = exif_u16(&exif, ExifTag::Orientation);
    let gps_iso6709 = exif
        .get_gps_info()
        .ok()
        .flatten()
        .map(|gps| gps.format_iso6709());

    Some(ExifInfo {
        make,
        model,
        software,
        datetime,
        datetime_original,
        lens_model,
        orientation,
        gps_iso6709,
    })
}

fn exif_string(exif: &Exif, tag: ExifTag) -> Option<String> {
    exif.get(tag)
        .and_then(EntryValue::as_str)
        .map(str::to_owned)
}

fn exif_value_string(exif: &Exif, tag: ExifTag) -> Option<String> {
    exif.get(tag).map(|v| v.to_string())
}

fn exif_u16(exif: &Exif, tag: ExifTag) -> Option<u16> {
    exif.get(tag).and_then(EntryValue::as_u16)
}

fn parse_icc_info(icc: &[u8]) -> Option<IccInfo> {
    let data = icc.to_vec();
    let decoded = DecodedICCProfile::new(&data).ok()?;
    Some(IccInfo {
        size: decoded.length,
        cmm_type: u32_to_tag(decoded.cmmid),
        version: format_icc_version(decoded.version),
        profile_class: u32_to_tag(decoded.device_class),
        color_space: u32_to_tag(decoded.color_space),
        pcs: u32_to_tag(decoded.pcs),
        created: Some(decoded.create_date),
        platform: u32_to_tag(decoded.platform),
        flags: decoded.flags,
        manufacturer: u32_to_tag(decoded.manufacturer),
        model: u32_to_tag(decoded.model),
        rendering_intent: decoded.rendering_intent,
    })
}

fn u32_to_tag(tag: u32) -> String {
    let bytes = tag.to_be_bytes();
    let mut s = String::with_capacity(4);
    for &b in &bytes {
        let ch = if (0x20..=0x7E).contains(&b) {
            b as char
        } else {
            '?'
        };
        s.push(ch);
    }
    s
}

fn format_icc_version(version_raw: u32) -> String {
    let major = (version_raw >> 24) & 0xFF;
    let minor = (version_raw >> 20) & 0x0F;
    let bugfix = (version_raw >> 16) & 0x0F;
    format!("{major}.{minor}.{bugfix}")
}
