#[cfg(windows)]
fn main() {
    use image::{ImageEncoder, codecs::png::PngEncoder, imageops::FilterType};
    use std::{env, fs, path::PathBuf};

    const PNG_PATH: &str = "resources/visor-mascot.png";
    const SIZES: [u32; 6] = [16, 32, 48, 64, 128, 256];
    println!("cargo:rerun-if-changed={PNG_PATH}");

    let png = fs::read(PNG_PATH).expect("Could not read application icon");
    let image = image::load_from_memory(&png).expect("Could not decode application icon");
    let mut images = Vec::with_capacity(SIZES.len());
    for size in SIZES {
        let rgba = image
            .resize_exact(size, size, FilterType::Lanczos3)
            .to_rgba8();
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&rgba, size, size, image::ExtendedColorType::Rgba8)
            .expect("Could not encode Windows icon size");
        images.push((size, png));
    }

    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in &images {
        let dimension = if *size == 256 { 0 } else { *size as u8 };
        ico.extend_from_slice(&[dimension, dimension, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        ico.extend_from_slice(&png);
    }

    let icon_path =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set")).join("visor-mascot.ico");
    fs::write(&icon_path, ico).expect("Could not write Windows icon resource");
    winres::WindowsResource::new()
        .set_icon(icon_path.to_str().expect("Icon path is valid UTF-8"))
        .compile()
        .expect("Could not embed Windows icon resource");
    let target = env::var("TARGET").expect("Cargo provides TARGET");
    let archive = if target.ends_with("windows-gnu") {
        icon_path.with_file_name("libresource.a")
    } else {
        icon_path.with_file_name("resource.lib")
    };
    let link_arg = if target.ends_with("windows-gnu") {
        format!(
            "-Wl,--whole-archive,{},--no-whole-archive",
            archive.display()
        )
    } else {
        format!("/WHOLEARCHIVE:{}", archive.display())
    };
    println!("cargo:rustc-link-arg={link_arg}");
}

#[cfg(not(windows))]
fn main() {
    println!("cargo:rerun-if-changed=resources/visor-mascot.png");
}
