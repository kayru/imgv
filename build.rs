use std::env;
use std::path::Path;
use std::process::Command;

struct ShaderTarget<'a> {
    entry: &'a str,
    target: &'a str,
}

fn main() {
    println!("cargo:rerun-if-changed=src/shaders.hlsl");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let fxc_path = Path::new("tools").join("fxc.exe");

    let shaders = [
        ShaderTarget {
            entry: "blit_vs",
            target: "vs_5_0",
        },
        ShaderTarget {
            entry: "blit_ps",
            target: "ps_5_0",
        },
    ];

    for shader in &shaders {
        println!("-- compiling {}", shader.entry);
        let dest_path = Path::new(&out_dir).join(shader.entry.to_owned() + ".dxbc");
        let fxc_status = Command::new(&fxc_path)
            .args(&[
                "/nologo",
                "/T",
                &shader.target,
                "/Fo",
                &dest_path.to_string_lossy(),
                "/E",
                shader.entry,
                "src\\shaders.hlsl",
            ])
            .status()
            .unwrap();
        if !fxc_status.success() {
            panic!();
        }
    }

    let icon_filename = make_icon();
    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon_with_id(&icon_filename.to_string_lossy(), "imgv");
        res.compile().unwrap();
    }
}

fn saturate(x: f32) -> f32 {
    if x < 0.0 {
        0.0
    } else if x > 1.0 {
        1.0
    } else {
        x
    }
}

fn quantize_u8(x: f32) -> u8 {
    (saturate(x) * 255.0).round() as u8
}

fn get_icon_color(uv: (f32, f32)) -> (u8, u8, u8, u8) {
    let r = uv.0;
    let g = uv.1;
    let b = 1.0 - (r + g) / 2.0;
    let a = 1.0;
    (
        quantize_u8(r),
        quantize_u8(g),
        quantize_u8(b),
        quantize_u8(a),
    )
}

fn make_icon() -> std::path::PathBuf {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let w = 16i32;
    let h = 16i32;
    let mut pixels = vec![std::u8::MAX; (4 * w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = (x + y * w) as usize;
            let uv = (x as f32 / w as f32, y as f32 / h as f32);
            let c = get_icon_color(uv);
            pixels[i * 4 + 0] = c.0;
            pixels[i * 4 + 1] = c.1;
            pixels[i * 4 + 2] = c.2;
            pixels[i * 4 + 3] = c.3;
        }
    }
    let image = ico::IconImage::from_rgba_data(w as u32, h as u32, pixels);
    let file_path = Path::new(&out_dir).join("imgv.ico");
    let file = std::fs::File::create(&file_path).unwrap();
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    icon_dir.add_entry(ico::IconDirEntry::encode(&image).unwrap());
    icon_dir.write(file).unwrap();

    file_path
}
