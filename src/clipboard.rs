use anyhow::{anyhow, Result};
use clipboard_win::formats::Format;
use image::DynamicImage;
use std::convert::TryInto;
use std::path::PathBuf;
use std::time::Duration;

fn dib_to_bmp(dib_data: &[u8]) -> Result<Vec<u8>> {
    const BMP_FILE_HEADER_SIZE: usize = 14;
    const BITMAPINFOHEADER_SIZE: usize = 40;
    if dib_data.len() < BITMAPINFOHEADER_SIZE {
        return Err(anyhow!("DIB data too small"));
    }

    let header_size = u32::from_le_bytes(dib_data[0..4].try_into().unwrap()) as usize;
    if header_size < BITMAPINFOHEADER_SIZE || dib_data.len() < header_size {
        return Err(anyhow!("Unsupported DIB header"));
    }

    let bit_count = u16::from_le_bytes(dib_data[14..16].try_into().unwrap());
    let clr_used = u32::from_le_bytes(dib_data[32..36].try_into().unwrap());
    let color_count = if clr_used != 0 {
        clr_used as usize
    } else if bit_count <= 8 {
        1usize << bit_count
    } else {
        0
    };

    let bf_off_bits = BMP_FILE_HEADER_SIZE + header_size + color_count * 4;
    let bf_size = BMP_FILE_HEADER_SIZE + dib_data.len();

    let mut bmp_data = Vec::with_capacity(bf_size);
    bmp_data.extend_from_slice(&[0x42, 0x4d]);
    bmp_data.extend_from_slice(&(bf_size as u32).to_le_bytes());
    bmp_data.extend_from_slice(&0u32.to_le_bytes());
    bmp_data.extend_from_slice(&(bf_off_bits as u32).to_le_bytes());
    bmp_data.extend_from_slice(dib_data);

    Ok(bmp_data)
}

pub fn write_bitmap_to_clipboard(hwnd: isize, bmp_data: &[u8]) -> Result<()> {
    const BMP_FILE_HEADER_SIZE: usize = 14;
    let mut attempts = 50;
    loop {
        match clipboard_win::Clipboard::new_for(hwnd as *mut _) {
            Ok(_clip) => {
                clipboard_win::raw::empty()
                    .map_err(|err| anyhow!("Failed to clear clipboard: {err:?}"))?;
                clipboard_win::raw::set_bitmap(bmp_data)
                    .map_err(|err| anyhow!("Failed to set CF_BITMAP: {err:?}"))?;
                let dib_data = &bmp_data[BMP_FILE_HEADER_SIZE..];
                clipboard_win::raw::set_without_clear(clipboard_win::formats::CF_DIB, dib_data)
                    .map_err(|err| anyhow!("Failed to set CF_DIB: {err:?}"))?;
                return Ok(());
            }
            Err(err) => {
                if attempts == 0 {
                    return Err(anyhow!("Failed to open clipboard: {err:?}"));
                }
                attempts -= 1;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

pub fn get_clipboard_file_path() -> Result<Option<PathBuf>> {
    let file_list = clipboard_win::formats::FileList;
    if !file_list.is_format_avail() {
        let unicode = clipboard_win::formats::Unicode;
        if !unicode.is_format_avail() {
            return Ok(None);
        }

        let text: String = match clipboard_win::get_clipboard(unicode) {
            Ok(text) => text,
            Err(_) => return Ok(None),
        };
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }

        let first_line = text.lines().next().unwrap_or("");
        let trimmed = first_line.trim().trim_matches('"');
        if trimmed.is_empty() {
            return Ok(None);
        }

        let path = PathBuf::from(trimmed);
        if path.exists() {
            return Ok(Some(path));
        }

        return Ok(None);
    }

    let paths: Vec<String> = match clipboard_win::get_clipboard(file_list) {
        Ok(paths) => paths,
        Err(_) => return Ok(None),
    };

    Ok(paths.into_iter().next().map(PathBuf::from))
}

pub fn get_clipboard_image() -> Result<Option<DynamicImage>> {
    let bitmap_format = clipboard_win::formats::Bitmap;
    if bitmap_format.is_format_avail() {
        if let Ok(data) = clipboard_win::get_clipboard(bitmap_format) {
            if let Ok(image) = image::load_from_memory(&data) {
                return Ok(Some(image));
            }
        }
    }

    let dib_format = clipboard_win::formats::RawData(clipboard_win::formats::CF_DIB);
    if dib_format.is_format_avail() {
        if let Ok(dib_data) = clipboard_win::get_clipboard(dib_format) {
            let bmp_data = dib_to_bmp(&dib_data)?;
            if let Ok(image) = image::load_from_memory(&bmp_data) {
                return Ok(Some(image));
            }
        }
    }

    Ok(None)
}
