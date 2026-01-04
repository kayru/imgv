use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, ImageResult};
use std::path::Path;

pub struct LoadedImage {
    pub image: DynamicImage,
    pub exif: Option<Vec<u8>>,
    pub icc_profile: Option<Vec<u8>>,
    pub orientation: Option<Orientation>,
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
    let image = DynamicImage::from_decoder(decoder)?;
    Ok(LoadedImage {
        image,
        exif,
        icc_profile,
        orientation,
    })
}
