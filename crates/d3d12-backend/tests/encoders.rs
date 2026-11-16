//! Encodes a few frames with every encoder that opens on this machine. An encoder that does not
//! open is skipped, not failed: a machine without an NVIDIA GPU has no NVENC.

use std::path::PathBuf;
use std::time::Instant;

use d3d12_backend::{encoder_available, probe_encoder, probe_media, EncoderChoice, Mp4Encoder, VideoCodec};
use engine::render::FrameSink;
use engine::{Output, Time};

const FRAMES: i64 = 30;

/// A multiple of 8 on both sides, which libkvazaar needs.
fn output() -> Output {
    Output { width: 320, height: 176, frame_duration: Time::from_ticks(3000), audio: None }
}

fn out_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gpufx-encoders-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the test folder");
    dir.join(format!("{name}.mp4"))
}

/// A moving gradient, so the encoder has something to do.
fn frame(index: i64, output: &Output) -> Vec<u8> {
    let (width, height) = (output.width as usize, output.height as usize);
    let mut rgba = vec![255u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let p = (y * width + x) * 4;
            rgba[p] = ((x + index as usize * 4) % 256) as u8;
            rgba[p + 1] = (y % 256) as u8;
            rgba[p + 2] = ((x + y) % 256) as u8;
        }
    }
    rgba
}

/// Writes [`FRAMES`] frames with `choice` and returns the encoder that opened.
fn encode(choice: &EncoderChoice, path: &std::path::Path) -> String {
    let output = output();
    let mut encoder = Mp4Encoder::create(path, &output, choice).unwrap_or_else(|e| panic!("{choice:?}: {e}"));
    for index in 0..FRAMES {
        encoder.write_frame(index, &frame(index, &output)).unwrap_or_else(|e| panic!("{choice:?} frame {index}: {}", e.0));
    }
    encoder.finish().unwrap_or_else(|e| panic!("{choice:?}: {e}"));
    let name = encoder.encoder_name().to_owned();
    drop(encoder);
    let probe = probe_media(path).unwrap_or_else(|e| panic!("{name}: the file does not read back: {e}"));
    assert!(probe.has_video, "{name}: no video stream ({:?})", probe.video_problem);
    name
}

#[test]
fn every_available_encoder_writes_a_readable_file() {
    let mut tried = 0;
    for codec in VideoCodec::ALL {
        for &name in codec.auto_order() {
            let started = Instant::now();
            if let Err(reason) = probe_encoder(name) {
                eprintln!("encoders: {name} skipped: {reason}");
                continue;
            }
            let probed = started.elapsed();
            assert!(encoder_available(name));
            let path = out_path(name);
            let started = Instant::now();
            assert_eq!(encode(&EncoderChoice::Named(name.to_owned()), &path), name);
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            eprintln!("encoders: {name} available: probe {probed:?}, {FRAMES} frames 320x176 in {:?}, {size} bytes", started.elapsed());
            let _ = std::fs::remove_file(&path);
            tried += 1;
        }
    }
    if tried == 0 {
        eprintln!("encoders: skipped: no encoder opens on this machine");
    }
}

#[test]
fn auto_opens_the_first_available_encoder_of_each_codec() {
    for codec in VideoCodec::ALL {
        let Some(expected) = d3d12_backend::auto_select(codec, encoder_available) else {
            eprintln!("encoders: auto {} skipped: no encoder opens", codec.id());
            continue;
        };
        let path = out_path(&format!("auto-{}", codec.id()));
        assert_eq!(encode(&EncoderChoice::Auto(codec), &path), expected);
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn an_encoder_this_ffmpeg_lacks_is_not_available() {
    assert!(!encoder_available("no_such_encoder"));
    assert!(Mp4Encoder::create(&out_path("missing"), &output(), &EncoderChoice::Named("no_such_encoder".into())).is_err());
}
