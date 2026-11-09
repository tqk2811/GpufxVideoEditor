//! What the editor needs to know about an effect package, read with gpufx's own parser.

use std::path::Path;

use gpufx_core::Package;

/// The parts of a manifest that decide how a package can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub name: String,
    pub description: String,
    pub inputs: usize,
    pub has_progress: bool,
}

/// Opens the package at `path` as the renderer would, which also checks its files.
pub fn read_package_info(path: &Path) -> Result<PackageInfo, String> {
    let package = Package::open(path).map_err(|e| e.to_string())?;
    let manifest = &package.manifest;
    Ok(PackageInfo {
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        inputs: manifest.inputs.len(),
        has_progress: manifest.params.iter().any(|param| param.name == "progress"),
    })
}
