//! Phase 0 proof of concept. See docs/plans/spec-poc-phase0-vi.md.

#![cfg(windows)]

mod bench;
mod decoder;
mod ffi;
mod gpu;

use std::path::PathBuf;
use std::process::ExitCode;

const DEFAULT_SEEK_COUNT: usize = 20;
const USAGE: &str = "usage: poc decode-bench <clip> [seek-count]";

fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("decode-bench") => {
            let clip = PathBuf::from(arguments.next().ok_or(USAGE)?);
            let seek_count = match arguments.next() {
                Some(text) => text.parse().map_err(|_| USAGE)?,
                None => DEFAULT_SEEK_COUNT,
            };
            if seek_count == 0 {
                return Err(USAGE.to_owned());
            }
            // SAFETY: called before any other libav* use.
            unsafe { ffi::av_log_set_level(ffi::AV_LOG_WARNING as i32) };
            bench::run(&clip, seek_count)
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
