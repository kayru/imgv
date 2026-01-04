use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum StepDirection {
    Backward,
    Forward,
}

pub fn is_compatible_file(path: &Path) -> bool {
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

pub fn get_next_file(path: &Path, direction: StepDirection) -> Option<PathBuf> {
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
