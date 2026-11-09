//! Stands in for `gpufx-render` in the tests of `export::child`. The output file's stem picks the
//! behaviour, so a test needs no shared state with it. `cargo test` also runs this target on its
//! own, without `--out`, and then it does nothing.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPORT: &str = r#"{"event":"finished","report":{"encoder":"fake","frames":3,"audio_samples":0,"clamped_samples":0,"pcm_sha256":null}}"#;

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn reserve(out: &Path) -> PathBuf {
    let partial = out.with_extension("1-0.tmp.mp4");
    std::fs::write(&partial, b"partial").expect("write the partial file");
    say(&format!(r#"{{"event":"output_reserved","path":{}}}"#, serde_json::to_string(&partial.to_string_lossy()).expect("a string")));
    partial
}

fn start(out: &Path) {
    say(r#"{"event":"started","protocol":1,"adapter":"fake","total":3,"out":"o"}"#);
    let _ = out;
}

fn finish(partial: &Path, out: &Path) {
    std::fs::rename(partial, out).expect("publish the file");
    say(REPORT);
}

fn progress(frame: i64) {
    say(&format!(r#"{{"event":"progress","frame":{frame},"total":3,"elapsed_ms":1}}"#));
}

fn flood_stderr() {
    let chunk = vec![b'e'; 1024 * 1024];
    let mut stderr = std::io::stderr().lock();
    for _ in 0..32 {
        stderr.write_all(&chunk).expect("write to stderr");
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(at) = arguments.iter().position(|a| a == "--out").map(|at| at + 1) else { return };
    let out = PathBuf::from(&arguments[at]);
    let mode = out.file_stem().expect("a file name").to_string_lossy().into_owned();
    let _ = std::fs::write(out.with_extension("args.txt"), arguments.join("\n"));
    let _ = std::fs::copy(&arguments[0], out.with_extension("project.json"));

    match mode.as_str() {
        "ok" => {
            start(&out);
            let partial = reserve(&out);
            for frame in 1..=3 {
                progress(frame);
            }
            finish(&partial, &out);
        }
        "malformed" => {
            say("gpu: not json");
            say("{\"event\":");
            say(&"y".repeat(200_000));
            let _ = std::io::stdout().write_all(&[0xff, 0xfe, b'\n']);
            say(r#"{"event":"from_the_future","x":1}"#);
            let partial = reserve(&out);
            finish(&partial, &out);
        }
        "crash" => {
            let _partial = reserve(&out);
            progress(1);
            eprintln!("the device went away");
            std::process::exit(0xC000_0005_u32 as i32);
        }
        "endcrash" => {
            start(&out);
            let partial = reserve(&out);
            finish(&partial, &out);
            std::process::exit(0xC000_0005_u32 as i32);
        }
        "errorcrash" => {
            let _partial = reserve(&out);
            say(r#"{"event":"error","message":"kept as errorcrash.1-0.tmp.mp4"}"#);
            std::process::exit(0xC000_0005_u32 as i32);
        }
        "silent" => {}
        "error" => {
            let _partial = reserve(&out);
            say(r#"{"event":"error","message":"kept as error.1-0.tmp.mp4"}"#);
        }
        "flood" => {
            flood_stderr();
            let partial = reserve(&out);
            finish(&partial, &out);
        }
        "floodcrash" => {
            flood_stderr();
            eprint!("last words");
            std::process::exit(3);
        }
        "cancel" => {
            let partial = reserve(&out);
            for line in std::io::stdin().lock().lines() {
                if line.is_ok_and(|line| line == "cancel") {
                    std::fs::remove_file(&partial).expect("remove the partial file");
                    std::fs::write(out.with_extension("stopped"), b"").expect("write");
                    say(r#"{"event":"cancelled"}"#);
                    return;
                }
            }
        }
        "hang" => {
            let _partial = reserve(&out);
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        other => panic!("no such mode: {other}"),
    }
}
