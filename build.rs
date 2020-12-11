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
}
