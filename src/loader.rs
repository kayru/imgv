use icc_profile::DecodedICCProfile;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, ImageResult};
use nom_exif::{EntryValue, Exif, ExifIter, ExifTag, MediaParser, MediaSource};
use std::path::Path;

pub struct LoadedImage {
    pub image: DynamicImage,
    pub exif: Option<Vec<u8>>,
    pub icc_profile: Option<Vec<u8>>,
    pub orientation: Option<Orientation>,
    pub exif_info: Option<ExifInfo>,
    pub icc_info: Option<IccInfo>,
}

pub fn load_image_with_metadata(path: &Path) -> ImageResult<LoadedImage> {
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
        image,
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
