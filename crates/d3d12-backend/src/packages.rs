//! What the editor needs to know about an effect package, read with gpufx's own parser.

use std::path::Path;

use gpufx_core::expr::Expr;
use gpufx_core::manifest::{ParamDef, ParamType};
use gpufx_core::Package;

/// The type of a parameter, as a manifest states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Float,
    Int,
    Bool,
    Vec2,
    Vec3,
    Vec4,
    Color,
    Expr,
    Enum,
}

/// One parameter of a package.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamInfo {
    pub name: String,
    pub kind: ParamKind,
    pub default: Option<serde_json::Value>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub description: String,
    /// The variables an `expr` parameter may use.
    pub vars: Vec<String>,
    /// The (name, number) choices of an `enum` parameter.
    pub choices: Vec<(String, i32)>,
}

/// The parts of a manifest that decide how a package can be used.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageInfo {
    pub name: String,
    pub description: String,
    pub inputs: usize,
    pub has_progress: bool,
    pub params: Vec<ParamInfo>,
}

fn param_info(def: &ParamDef) -> ParamInfo {
    let kind = match def.kind {
        ParamType::Float => ParamKind::Float,
        ParamType::Int => ParamKind::Int,
        ParamType::Bool => ParamKind::Bool,
        ParamType::Vec2 => ParamKind::Vec2,
        ParamType::Vec3 => ParamKind::Vec3,
        ParamType::Vec4 => ParamKind::Vec4,
        ParamType::Color => ParamKind::Color,
        ParamType::Expr => ParamKind::Expr,
        ParamType::Enum => ParamKind::Enum,
    };
    ParamInfo {
        name: def.name.clone(),
        kind,
        default: def.default.clone(),
        min: def.min,
        max: def.max,
        description: def.description.clone(),
        vars: def.vars.clone(),
        choices: def.enum_values().into_iter().map(|(name, number)| (name.to_owned(), number)).collect(),
    }
}

/// Why `text` cannot be the value of an `expr` parameter with `vars`, or `None` when it can.
pub fn expression_error(text: &str, vars: &[String]) -> Option<String> {
    Expr::parse(text, vars).err().map(|e| e.to_string())
}

/// Opens the package at `path` as the renderer would, which also checks its files.
pub fn read_package_info(path: &Path) -> Result<PackageInfo, String> {
    Package::open(path).map(|package| package_info(&package)).map_err(|e| e.to_string())
}

/// [`read_package_info`] for the bytes of a zip that is not on disk under its final name.
pub fn read_zip_package_info(bytes: &[u8]) -> Result<PackageInfo, String> {
    Package::from_zip_bytes(bytes).map(|package| package_info(&package)).map_err(|e| e.to_string())
}

fn package_info(package: &Package) -> PackageInfo {
    let manifest = &package.manifest;
    PackageInfo {
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        inputs: manifest.inputs.len(),
        has_progress: manifest.params.iter().any(|param| param.name == "progress"),
        params: manifest.params.iter().map(param_info).collect(),
    }
}
