//! A preview proxy made by the real `gpufx-render` on a real GPU, and the preview decoding it.
//! Skips on a machine without a hardware adapter, test-clips or a built `gpufx-render.exe`.

#![cfg(windows)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use d3d12_backend::{probe_media, probe_video_format, Gpu, ProjectPaths, ProxyFile};
use engine::{Output, Project, Time, Track, TrackKind, SUPPORTED_VERSION};
use gpufx_editor::editor::new_clip;
use gpufx_editor::preview::backend::D3d12Producer;
use gpufx_editor::project_file::package_roots;
use gpufx_editor::proxy::{preview_file, proxy_file, ChildProxyMaker, MakeOutcome, ProxyMaker, KEYFRAME_INTERVAL};
use gpufx_editor::visuals::worker::FileStamp;

fn skip(reason: &str) {
    assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {reason}");
    eprintln!("proxy_gpu: skipped: {reason}");
}

/// `gpufx-render.exe` of the same profile, which `cargo build -p gpufx-render` puts above `deps`.
fn render_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let found = exe.parent()?.parent()?.join("gpufx-render.exe");
    found.is_file().then_some(found)
}

fn test_clip(name: &str) -> PathBuf {
    std::path::absolute(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-clips")).join(name)).expect("absolute path")
}

fn ffprobe() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffprobe.exe"))
}

/// The frame numbers of the keyframes of the video of `path`, in decode order.
fn keyframes(path: &Path) -> (Vec<usize>, usize) {
    let output = Command::new(ffprobe()).args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=key_frame", "-of", "csv=p=0"]).arg(path).output().expect("ffprobe runs");
    assert!(output.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&output.stderr));
    let flags: Vec<bool> = String::from_utf8_lossy(&output.stdout).lines().map(|line| line.trim().starts_with('1')).collect();
    (flags.iter().enumerate().filter(|(_, key)| **key).map(|(index, _)| index).collect(), flags.len())
}

fn one_clip_project(source: &Path, seconds: i64) -> Arc<Project> {
    let clip = new_clip(&source.to_string_lossy(), Time::ZERO, Time::from_seconds(seconds).unwrap());
    Arc::new(Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 320, height: 180, frame_rate: engine::FrameRate::whole(30), audio: None },
        tracks: vec![Track { kind: TrackKind::Video, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false, subtitle: None }],
        markers: Vec::new(),
        beats: Default::default(),
        motion_tracks: Vec::new(),
    })
}

fn mean_difference(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len() as f64
}

#[test]
fn a_proxy_is_540p_short_gop_silent_and_the_preview_decodes_it_in_place_of_the_original() {
    if Gpu::create_hardware().expect("device").is_none() {
        return skip("no hardware adapter");
    }
    let original = test_clip("clip-a.mp4");
    if !original.is_file() || !test_clip("clip-b.mp4").is_file() {
        return skip("test-clips is missing (scripts/setup-poc.ps1 makes it)");
    }
    let Some(tool) = render_exe() else { return skip("gpufx-render.exe is not built; run cargo build -p gpufx-render") };

    let folder = std::env::temp_dir().join(format!("gpufx-proxy-gpu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    let maker = ChildProxyMaker { tool, package_roots: package_roots() };
    let stamp: FileStamp = maker.stamp(&original).expect("stamp");
    let out = proxy_file(&folder, &original, stamp);
    let (format, duration) = maker.inspect(&original).expect("inspect").expect("a video");
    let progress = Mutex::new(Vec::new());
    let outcome = maker.make(&original, &format, duration, &out, &Arc::new(AtomicBool::new(false)), &|fraction| progress.lock().unwrap().push(fraction));
    assert_eq!(outcome, MakeOutcome::Done);
    assert!(out.is_file(), "the proxy has its final name");
    assert!(std::fs::read_dir(&folder).unwrap().count() == 1, "no temporary file is left");
    assert!(!progress.lock().unwrap().is_empty(), "progress was reported");

    let proxy_format = probe_video_format(&out).expect("probe the proxy");
    assert_eq!((proxy_format.width, proxy_format.height), (960, 540), "{} is {}x{}", original.display(), format.width, format.height);
    assert_eq!(proxy_format.frame_duration, format.frame_duration, "the frame rate of the source");
    let streams = probe_media(&out).expect("probe");
    assert!(streams.has_video && !streams.has_audio, "video only: the sound comes from the original");
    let (keys, frames) = keyframes(&out);
    assert!(frames >= 590, "the whole clip: {frames} frames");
    assert_eq!(keys.first(), Some(&0));
    let longest = keys.windows(2).map(|pair| pair[1] - pair[0]).chain([frames - keys.last().unwrap()]).max().unwrap();
    assert!(longest <= KEYFRAME_INTERVAL as usize, "a keyframe at least every {KEYFRAME_INTERVAL} frames, not {longest}: {keys:?}");

    // The preview: the proxy draws what the original draws, at the same place and size.
    let gpu = Arc::new(Gpu::create_hardware().expect("device").expect("hardware"));
    let mut producer = D3d12Producer::new(gpu);
    let project = one_clip_project(&original, 20);
    let plain = Arc::new(ProjectPaths::new(Path::new("p.json"), package_roots()));
    let with_proxy = Arc::new(ProjectPaths::new(Path::new("p.json"), package_roots()).with_proxies(Arc::new(HashMap::from([(original.clone(), preview_file(out.clone(), &format))]))));
    let frame = 45;
    let from_original = producer.capture(&project, &plain, frame).expect("capture the original");
    let from_proxy = producer.capture(&project, &with_proxy, frame).expect("capture the proxy");
    let close = mean_difference(&from_original, &from_proxy);
    assert!(close < 6.0, "the proxy looks like the original: mean difference {close}");
    // Mapped to another file, the preview shows that file: the map is what it decodes.
    let elsewhere = Arc::new(ProjectPaths::new(Path::new("p.json"), package_roots()).with_proxies(Arc::new(HashMap::from([(original.clone(), ProxyFile::plain(test_clip("clip-b.mp4")))]))));
    let from_other = producer.capture(&project, &elsewhere, frame).expect("capture the other file");
    let far = mean_difference(&from_original, &from_other);
    assert!(far > close * 2.0 && far > 6.0, "another file looks different: {far} against {close}");
    let _ = std::fs::remove_dir_all(&folder);
}

fn ffmpeg_exe() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe"))
}

/// Writes `name` in `folder` with ffmpeg and returns its absolute path.
fn synthesize(folder: &Path, name: &str, arguments: &[&str]) -> PathBuf {
    let path = folder.join(name);
    let status = Command::new(ffmpeg_exe()).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).arg(&path).status().expect("ffmpeg runs");
    assert!(status.success(), "making {name} failed");
    std::path::absolute(path).unwrap()
}

/// Makes the proxy of `original` in `folder` with the real tool, and the paths that read it.
fn make_proxy(tool: &Path, folder: &Path, original: &Path) -> (Arc<ProjectPaths>, d3d12_backend::VideoFormat, PathBuf) {
    let maker = ChildProxyMaker { tool: tool.to_path_buf(), package_roots: package_roots() };
    let out = proxy_file(folder, original, maker.stamp(original).expect("stamp"));
    let (format, duration) = maker.inspect(original).expect("inspect").expect("a video");
    let outcome = maker.make(original, &format, duration, &out, &Arc::new(AtomicBool::new(false)), &|_| {});
    assert_eq!(outcome, MakeOutcome::Done, "the proxy of {}", original.display());
    let paths = Arc::new(ProjectPaths::new(Path::new("p.json"), package_roots()).with_proxies(Arc::new(HashMap::from([(original.to_path_buf(), preview_file(out.clone(), &format))]))));
    (paths, format, out)
}

/// The GPU, the tool and a scratch folder, or why the test skips.
fn rig(name: &str) -> Option<(Arc<Gpu>, PathBuf, PathBuf)> {
    let Some(gpu) = Gpu::create_hardware().expect("device") else {
        skip("no hardware adapter");
        return None;
    };
    let Some(tool) = render_exe() else {
        skip("gpufx-render.exe is not built; run cargo build -p gpufx-render");
        return None;
    };
    let folder = std::env::temp_dir().join(format!("gpufx-proxy-gpu-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    Some((Arc::new(gpu), tool, folder))
}

#[test]
fn a_proxy_of_an_odd_rate_source_with_a_start_offset_shows_the_frame_the_original_shows() {
    let Some((gpu, tool, folder)) = rig("timing") else { return };
    // 59.94 fps (1501.5 ticks a frame, so a rounded step drifts half a tick a frame), a grey level
    // of its own on every frame, and a stream that starts at 1.5 s.
    let original = synthesize(&folder, "film.mp4", &["-f", "lavfi", "-i", "color=c=black:s=640x360:r=60000/1001:d=10,geq=lum=mod(N*37\\,200)+20:cb=128:cr=128", "-c:v", "libopenh264", "-pix_fmt", "yuv420p", "-b:v", "8M", "-g", "12", "-output_ts_offset", "1.5"]);
    let (with_proxy, format, out) = make_proxy(&tool, &folder, &original);
    assert_eq!(format.rate, Some((60000, 1001)));
    let (_, proxy_frames) = keyframes(&out);
    let (_, original_frames) = keyframes(&original);
    assert!(proxy_frames >= original_frames, "every frame of the source is in the proxy: {proxy_frames} against {original_frames}");

    let mut producer = D3d12Producer::new(gpu);
    let mut project = (*one_clip_project(&original, 10)).clone();
    project.output.frame_rate = engine::FrameRate::MAX;
    // Trimmed by 1000 ticks, so output frame i reads the source at i * 1500 + 1000: late in the
    // clip that lands just after the start of a source frame, where a drifting proxy is behind.
    project.tracks[0].clips[0].source_in = Time::from_ticks(1000);
    project.tracks[0].clips[0].duration = Time::from_ticks(9 * engine::TICKS_PER_SECOND);
    let plain = Arc::new(ProjectPaths::new(Path::new("p.json"), package_roots()));
    let mut worst = (0.0, 0);
    // 60 fps output frames over a 59.94 fps source, at the start and late, where drift adds up.
    for frame in (0..30).chain(450..535) {
        let from_original = producer.capture(&project, &plain, frame).expect("capture the original");
        let from_proxy = producer.capture(&project, &with_proxy, frame).expect("capture the proxy");
        let difference = mean_difference(&from_original, &from_proxy);
        if difference > worst.0 {
            worst = (difference, frame);
        }
    }
    // One frame off is a grey step of 37 (or more), about 30 in RGB.
    assert!(worst.0 < 4.0, "output frame {} shows another source frame through the proxy: mean difference {}", worst.1, worst.0);
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn a_turned_source_has_an_upright_proxy_that_is_not_turned_again_and_keeps_the_original_shape() {
    let Some((gpu, tool, folder)) = rig("turned") else { return };
    // Stored 1280x720, shown turned a quarter: an upright 720x1280 picture.
    let stored = synthesize(&folder, "stored.mp4", &["-f", "lavfi", "-i", "testsrc2=s=1280x720:r=30:d=1", "-c:v", "libopenh264", "-pix_fmt", "yuv420p", "-b:v", "8M"]);
    let original = synthesize(&folder, "turned.mp4", &["-display_rotation", "90", "-i", &stored.to_string_lossy(), "-c", "copy"]);
    let (with_proxy, format, out) = make_proxy(&tool, &folder, &original);
    assert_eq!((format.width, format.height), (720, 1280));
    let proxy_format = probe_video_format(&out).expect("probe the proxy");
    assert_eq!((proxy_format.width, proxy_format.height), (304, 540), "upright, with no turn of its own");
    assert!((proxy_format.aspect - format.aspect).abs() > 1e-4, "the even sides round the shape: {} against {}", proxy_format.aspect, format.aspect);
    assert_eq!(with_proxy.video_decode_source(&original.to_string_lossy()).and_then(|file| file.display_aspect), Some(format.aspect), "the layout uses the original's shape");

    let mut producer = D3d12Producer::new(gpu);
    let project = one_clip_project(&original, 1);
    let plain = Arc::new(ProjectPaths::new(Path::new("p.json"), package_roots()));
    for frame in [0, 15] {
        let from_original = producer.capture(&project, &plain, frame).expect("capture the original");
        let from_proxy = producer.capture(&project, &with_proxy, frame).expect("capture the proxy");
        let difference = mean_difference(&from_original, &from_proxy);
        assert!(difference < 8.0, "frame {frame}: the proxy is drawn where and as the original is: mean difference {difference}");
    }
    let _ = std::fs::remove_dir_all(&folder);
}
