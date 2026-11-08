//! Phase 0 proof of concept. See docs/plans/spec-poc-phase0-vi.md.

#![cfg(windows)]

mod bench;
mod compositor;
mod decoder;
mod ffi;
mod gpu;
mod mix_bench;
mod player;
mod presenter;

use std::path::PathBuf;
use std::process::ExitCode;

const DEFAULT_SEEK_COUNT: usize = 20;
const USAGE: &str = "usage: poc decode-bench <clip> [seek-count]\n       poc player <clip1> <clip2> <clip3>\n       poc bench <clip1> <clip2> <clip3> --out <json>";

fn three_clips(arguments: &mut impl Iterator<Item = String>) -> Result<Vec<PathBuf>, String> {
    (0..3).map(|_| arguments.next().map(PathBuf::from).ok_or_else(|| USAGE.to_owned())).collect()
}

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
        Some("player") => {
            let clips = three_clips(&mut arguments)?;
            // SAFETY: called before any other libav* use.
            unsafe { ffi::av_log_set_level(ffi::AV_LOG_WARNING as i32) };
            player::run(clips)
        }
        Some("bench") => {
            let clips = three_clips(&mut arguments)?;
            if arguments.next().as_deref() != Some("--out") {
                return Err(USAGE.to_owned());
            }
            let out = PathBuf::from(arguments.next().ok_or(USAGE)?);
            // SAFETY: called before any other libav* use.
            unsafe { ffi::av_log_set_level(ffi::AV_LOG_WARNING as i32) };
            mix_bench::run(clips, out)
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
