//! Links the import libraries of third_party/ffmpeg and copies its DLLs next to the executable.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const LINKED_LIBRARIES: [&str; 3] = ["avcodec", "avformat", "avutil"];
const RUNTIME_DLL_PREFIXES: [&str; 5] = ["avcodec-", "avformat-", "avutil-", "swresample-", "swscale-"];

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ffmpeg_root = manifest_dir.join("..").join("..").join("third_party").join("ffmpeg");
    let lib_dir = ffmpeg_root.join("lib");
    let bin_dir = ffmpeg_root.join("bin");
    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!("cargo:rerun-if-changed=build.rs");

    if !lib_dir.join("avcodec.lib").is_file() {
        panic!("{} has no avcodec.lib; run scripts/setup-poc.ps1 first", lib_dir.display());
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for library in LINKED_LIBRARIES {
        println!("cargo:rustc-link-lib=dylib={library}");
    }

    copy_runtime_dlls(&bin_dir);
}

fn copy_runtime_dlls(bin_dir: &Path) {
    // OUT_DIR is target/<profile>/build/<crate>-<hash>/out; the executable is in target/<profile>.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out_dir.ancestors().nth(3).expect("OUT_DIR is nested under target/<profile>");

    for entry in fs::read_dir(bin_dir).expect("third_party/ffmpeg/bin is readable") {
        let path = entry.expect("directory entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let is_runtime_dll = name.ends_with(".dll") && RUNTIME_DLL_PREFIXES.iter().any(|p| name.starts_with(p));
        if is_runtime_dll {
            fs::copy(&path, profile_dir.join(&name)).expect("copy FFmpeg DLL");
        }
    }
}
