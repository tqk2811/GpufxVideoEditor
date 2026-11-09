//! Links the import libraries of third_party/ffmpeg (LGPL) or, with feature `ffmpeg-gpl`,
//! third_party/ffmpeg-gpl, and copies its DLLs next to the executables.

use std::env;
use std::fs;
use std::path::PathBuf;

const LINKED_LIBRARIES: [&str; 4] = ["avcodec", "avformat", "avutil", "swresample"];
const RUNTIME_DLL_PREFIXES: [&str; 5] = ["avcodec-", "avformat-", "avutil-", "swresample-", "swscale-"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // The DLLs land in the target directory, so each variant needs its own (see scripts/build-variants.ps1).
    let variant = if env::var_os("CARGO_FEATURE_FFMPEG_GPL").is_some() { "ffmpeg-gpl" } else { "ffmpeg" };
    let ffmpeg_root = manifest_dir.join("..").join("..").join("third_party").join(variant);
    let lib_dir = ffmpeg_root.join("lib");
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    if !lib_dir.join("avcodec.lib").is_file() {
        panic!("{} has no avcodec.lib; run scripts/setup-poc.ps1 first", lib_dir.display());
    }
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for library in LINKED_LIBRARIES {
        println!("cargo:rustc-link-lib=dylib={library}");
    }

    // OUT_DIR is target/<profile>/build/<crate>-<hash>/out; executables are in target/<profile>.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out_dir.ancestors().nth(3).expect("OUT_DIR is nested under target/<profile>");
    for entry in fs::read_dir(ffmpeg_root.join("bin")).expect("third_party/ffmpeg/bin is readable") {
        let path = entry.expect("directory entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        if name.ends_with(".dll") && RUNTIME_DLL_PREFIXES.iter().any(|p| name.starts_with(p)) {
            fs::copy(&path, profile_dir.join(&name)).expect("copy FFmpeg DLL");
        }
    }
}
