//! `transition_preview` and `asset_preview`: what a transition or a sticker looks like, for an agent
//! choosing one. The MCP server answers both in its own process in both modes, so an open window
//! never renders them and its UI thread is never held up.
//!
//! A transition preview is the strip of the effect picker's cache (`PreviewStore`): the default
//! size reads and fills the very entries the picker uses. Another size is a separate entry, under a
//! package name that includes the size, so writing it never removes the picker's files. A miss is
//! rendered at once by a `PreviewSession` of this process (its own helper, with the same timeout,
//! failure and device rules as the picker's), which only shares the files on disk with the window.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use d3d12_backend::{parse_asset_source, preview_image_files, read_animation, read_package_info, read_still, reserve_temporary, AnimatedPicture, EncoderChoice, Mp4Encoder, PreviewJobKind, ProjectPaths, StillPicture, VideoCodec, PREVIEW_FPS, PREVIEW_FRAMES};
use engine::render::FrameSink;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::image::{encode_png, shrink};
use super::protocol::{AgentError, AgentResult};
use crate::effect_previews::child::{PreviewRequest, PreviewSession, ProcessLauncher, RenderOutcome, SessionConfig};
use crate::effect_previews::{package_digest, package_prefix, preview_key, PreviewImage, PreviewSpec, PreviewStore, CONTROL_PACKAGE};

/// What a preview comes as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewFormat {
    /// PNG pictures, evenly spaced.
    #[default]
    Frames,
    /// One MP4 file in the cache.
    Video,
}

/// How many pictures a preview gives unless the call says otherwise.
pub const DEFAULT_FRAMES: u32 = 6;
/// The fewest and the most pictures a call may ask for.
pub const MIN_FRAMES: u32 = 2;
pub const MAX_FRAMES: u32 = 24;
/// The long edges a transition preview may have; the first is the picker's.
pub const TRANSITION_SIZES: [u32; 3] = [144, 360, 720];
/// The long edge of a sticker preview unless the call says otherwise, and the most it may ask for.
pub const ASSET_DEFAULT_SIZE: u32 = 360;
pub const ASSET_MAX_SIZE: u32 = 720;
/// The frame rate of a sticker's MP4.
pub const ASSET_VIDEO_FPS: u32 = 30;
/// Bump when a change makes the sticker MP4s look different.
const ASSET_PREVIEW_VERSION: u32 = 1;
/// How long a loop of a size other than the picker's may take: it has up to 44 times the pixels.
const SIZED_LOOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

fn invalid(message: impl Into<String>) -> AgentError {
    AgentError::new("invalid_params", message)
}

/// Checks the number of pictures asked for.
pub fn check_frames(frames: u32) -> Result<u32, AgentError> {
    if (MIN_FRAMES..=MAX_FRAMES).contains(&frames) {
        Ok(frames)
    } else {
        Err(invalid(format!("frames {frames}: from {MIN_FRAMES} to {MAX_FRAMES}")))
    }
}

/// The spec of a transition preview of long edge `size`: the picker's square for 144, 16:9 for the
/// others.
pub fn transition_spec(size: u32) -> Result<PreviewSpec, AgentError> {
    let (width, height) = match size {
        144 => return Ok(PreviewSpec::STANDARD),
        360 => (640, 360),
        720 => (1280, 720),
        other => return Err(invalid(format!("size {other}: one of 144, 360 or 720"))),
    };
    Ok(PreviewSpec { width, height, ..PreviewSpec::STANDARD })
}

/// The store's name for the previews of `package` at `spec`: the picker's own for its spec, and
/// one that includes the size for any other, so the two never remove each other's files.
pub fn sized_prefix(package: &str, spec: PreviewSpec) -> String {
    if spec == PreviewSpec::STANDARD {
        package_prefix(package)
    } else {
        package_prefix(&format!("{package}@{}x{}", spec.width, spec.height))
    }
}

/// `count` indexes into `total` items spread evenly from the first to the last, both included,
/// without repeats; fewer when there are fewer items.
pub fn spread_indexes(total: usize, count: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    let count = count.min(total).max(1);
    if count == 1 {
        return vec![0];
    }
    (0..count).map(|i| ((i * (total - 1)) as f64 / (count - 1) as f64).round() as usize).collect()
}

/// `count` times spread evenly over a loop of `length_ms`, from its start, the end left out since
/// it is the start again.
pub fn spread_times(length_ms: u64, count: usize) -> Vec<u64> {
    (0..count as u64).map(|i| i * length_ms / count.max(1) as u64).collect()
}

/// The frame of `picture` shown `ms` after its loop starts.
fn frame_at_ms(picture: &AnimatedPicture, ms: u64) -> usize {
    picture.ends_ms.partition_point(|end| *end <= ms).min(picture.frames.len() - 1)
}

/// The size of a picture of `width` x `height` with its long edge at most `long`, never larger than
/// it is; even sides (at least 2) when `even`, as 4:2:0 video needs.
pub fn fit_size(width: u32, height: u32, long: u32, even: bool) -> (u32, u32) {
    let native = width.max(height).max(1);
    let target = long.min(native);
    let scale = |side: u32| ((u64::from(side) * u64::from(target) + u64::from(native) / 2) / u64::from(native)).max(1) as u32;
    let (mut w, mut h) = (scale(width), scale(height));
    if even {
        w = (w & !1).max(2);
        h = (h & !1).max(2);
    }
    (w, h)
}

/// The frames of a strip of `spec`, top to bottom.
pub fn split_strip(strip: &PreviewImage, spec: PreviewSpec) -> Vec<Vec<u8>> {
    let bytes = spec.width as usize * spec.height as usize * 4;
    strip.rgba.chunks_exact(bytes).map(<[u8]>::to_vec).collect()
}

/// `file:///` URI of an absolute path, with what is not plainly safe escaped.
pub fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file:///");
    for byte in text.trim_start_matches('/').bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => uri.push(byte as char),
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// Makes `target` with `encode` unless it is already there, through a temporary name beside it
/// that is renamed into place, so a reader never sees half a file. Returns whether the file was
/// already there. When the rename fails but another writer finished the file first, that counts.
pub fn ensure_file(target: &Path, encode: impl FnOnce(&Path) -> Result<(), String>) -> Result<bool, String> {
    if std::fs::metadata(target).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0) {
        return Ok(true);
    }
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
    }
    let temporary = reserve_temporary(target).map_err(|e| format!("reserving a file next to {}: {e}", target.display()))?;
    let result = encode(&temporary).and_then(|()| std::fs::rename(&temporary, target).map_err(|e| format!("moving the video to {}: {e}", target.display())));
    if let Err(message) = result {
        let _ = std::fs::remove_file(&temporary);
        if std::fs::metadata(target).is_ok_and(|metadata| metadata.len() > 0) {
            return Ok(true);
        }
        return Err(message);
    }
    Ok(false)
}

/// Encodes `frames` (RGBA8 of `width` x `height`, both even) as an H.264 MP4 at `fps`.
pub fn encode_mp4(path: &Path, frames: &[Vec<u8>], width: u32, height: u32, fps: u32) -> Result<(), String> {
    let output = engine::Output { width, height, frame_rate: engine::FrameRate { num: fps, den: 1 }, audio: None };
    let mut encoder = Mp4Encoder::create(path, &output, &EncoderChoice::Auto(VideoCodec::H264))?;
    for (index, frame) in frames.iter().enumerate() {
        encoder.write_frame(index as i64, frame).map_err(|e| e.0)?;
    }
    encoder.finish()
}

/// The name of a sticker's MP4: its content, its size and the version of this code.
pub fn asset_video_key(content: &[u8], width: u32, height: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ASSET_PREVIEW_VERSION.to_le_bytes());
    hasher.update((content.len() as u64).to_le_bytes());
    hasher.update(content);
    for value in [width, height, ASSET_VIDEO_FPS] {
        hasher.update(value.to_le_bytes());
    }
    let digest = hasher.finalize();
    let hex: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{hex}-{width}x{height}")
}

/// `%LOCALAPPDATA%\gpufx-editor\asset-previews\v1`, beside the effect previews.
pub fn default_asset_root() -> Option<PathBuf> {
    PreviewStore::default_root().and_then(|root| Some(root.parent()?.parent()?.join("asset-previews").join("v1")))
}

fn png_base64(rgba: &[u8], width: u32, height: u32) -> Result<String, AgentError> {
    let (png, _, _) = encode_png(rgba, width, height, width).map_err(|e| AgentError::new("internal", e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
}

/// The JSON of an MP4 answer, with the link the MCP server turns into a resource link.
fn video_json(path: &Path, reused: bool, width: u32, height: u32, fps: u32, frames: usize) -> Value {
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let uri = file_uri(path);
    json!({
        "path": path.to_string_lossy(),
        "uri": uri,
        "mime_type": "video/mp4",
        "width": width,
        "height": height,
        "fps": fps,
        "frames": frames,
        "duration_ms": frames as u64 * 1000 / u64::from(fps.max(1)),
        "reused": reused,
        "resource_link": {"uri": uri, "name": name, "mime_type": "video/mp4"},
    })
}

/// The answer for a transition whose strip is at hand: `frames` pictures or an MP4 at `video`.
pub fn transition_answer(id: &str, strip: &PreviewImage, spec: PreviewSpec, format: PreviewFormat, frames: u32, video: &Path, cached: bool) -> AgentResult {
    let pictures = split_strip(strip, spec);
    let is_static = pictures.len() <= 1;
    let base = json!({"id": id, "width": spec.width, "height": spec.height, "static": is_static, "cached": cached});
    match format {
        PreviewFormat::Frames => {
            let mut answer = base;
            let mut items = Vec::new();
            let mut pngs = Vec::new();
            // A static strip has one frame, which every progress shows.
            for index in spread_indexes(PREVIEW_FRAMES as usize, frames as usize) {
                let picture = &pictures[if is_static { 0 } else { index.min(pictures.len() - 1) }];
                items.push(json!({"frame": index, "progress": index as f64 / f64::from(PREVIEW_FRAMES - 1)}));
                pngs.push(png_base64(picture, spec.width, spec.height)?);
            }
            answer["frames"] = Value::Array(items);
            answer["pngs"] = json!(pngs);
            Ok(answer)
        }
        PreviewFormat::Video => {
            let full: Vec<Vec<u8>> = if is_static { vec![pictures[0].clone(); PREVIEW_FRAMES as usize] } else { pictures };
            let reused = ensure_file(video, |path| encode_mp4(path, &full, spec.width, spec.height, PREVIEW_FPS)).map_err(|e| AgentError::new("encode_failed", e))?;
            let mut answer = video_json(video, reused, spec.width, spec.height, PREVIEW_FPS, full.len());
            for (key, value) in base.as_object().expect("an object") {
                answer[key] = value.clone();
            }
            Ok(answer)
        }
    }
}

fn scaled(picture: &StillPicture, width: u32, height: u32) -> Vec<u8> {
    if (picture.width, picture.height) == (width, height) {
        picture.rgba.clone()
    } else {
        shrink(&picture.rgba, picture.width, picture.height, width, height)
    }
}

/// The answer for a sticker already decoded: `animation` when it moves, `still` otherwise.
pub fn asset_answer(source: &str, content: &[u8], still: Option<&StillPicture>, animation: Option<&AnimatedPicture>, format: PreviewFormat, frames: u32, size: u32, video_root: Option<&Path>) -> AgentResult {
    let Some(animation) = animation.filter(|animation| !animation.frames.is_empty()) else {
        let still = still.ok_or_else(|| AgentError::new("internal", "no picture"))?;
        if format == PreviewFormat::Video {
            return Err(invalid(format!("{source} is a still sticker, which has no video; ask for frames")));
        }
        let (width, height) = fit_size(still.width, still.height, size, false);
        return Ok(json!({"source": source, "animated": false, "width": width, "height": height, "native_width": still.width, "native_height": still.height, "pngs": [png_base64(&scaled(still, width, height), width, height)?]}));
    };
    let first = &animation.frames[0];
    let length_ms = animation.ends_ms.last().copied().unwrap_or(0);
    let base = json!({"source": source, "animated": true, "native_width": first.width, "native_height": first.height, "duration_ms": length_ms});
    match format {
        PreviewFormat::Frames => {
            let (width, height) = fit_size(first.width, first.height, size, false);
            let mut answer = base;
            let mut items = Vec::new();
            let mut pngs = Vec::new();
            for time in spread_times(length_ms, frames as usize) {
                let index = frame_at_ms(animation, time);
                items.push(json!({"time_ms": time, "frame": index}));
                pngs.push(png_base64(&scaled(&animation.frames[index], width, height), width, height)?);
            }
            answer["width"] = json!(width);
            answer["height"] = json!(height);
            answer["frames"] = Value::Array(items);
            answer["pngs"] = json!(pngs);
            Ok(answer)
        }
        PreviewFormat::Video => {
            let root = video_root.ok_or_else(|| AgentError::new("preview_unavailable", "LOCALAPPDATA is not set, so the video has nowhere to be kept"))?;
            let (width, height) = fit_size(first.width, first.height, size, true);
            let target = root.join(format!("{}.mp4", asset_video_key(content, width, height)));
            let count = (length_ms * u64::from(ASSET_VIDEO_FPS)).div_ceil(1000).max(1) as usize;
            let reused = ensure_file(&target, |path| {
                let full: Vec<Vec<u8>> = (0..count as u64).map(|k| scaled(&animation.frames[frame_at_ms(animation, k * 1000 / u64::from(ASSET_VIDEO_FPS))], width, height)).collect();
                encode_mp4(path, &full, width, height, ASSET_VIDEO_FPS)
            })
            .map_err(|e| AgentError::new("encode_failed", e))?;
            let mut answer = video_json(&target, reused, width, height, ASSET_VIDEO_FPS, count);
            for (key, value) in base.as_object().expect("an object") {
                answer[key] = value.clone();
            }
            Ok(answer)
        }
    }
}

/// Answers the preview tools. It lives on the MCP server's session thread, in both modes, and
/// starts its helper only when a transition preview misses the cache.
pub struct PreviewService {
    roots: Vec<PathBuf>,
    store: Option<PreviewStore>,
    asset_root: Option<PathBuf>,
    session: Option<PreviewSession>,
}

impl PreviewService {
    /// The real one: the user's cache folders and the helper beside the running program.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots, store: PreviewStore::default_root().map(PreviewStore::new), asset_root: default_asset_root(), session: None }
    }

    fn paths(&self) -> ProjectPaths {
        ProjectPaths::roots_only(Path::new("agent.json"), self.roots.clone())
    }

    /// `transition_preview`.
    pub fn transition(&mut self, id: &str, format: PreviewFormat, frames: u32, size: u32) -> AgentResult {
        let frames = check_frames(frames)?;
        let spec = transition_spec(size)?;
        let paths = self.paths();
        let dir = paths.package_path(id).ok_or_else(|| invalid(format!("no transition {id} in the package roots; packages_list names them")))?;
        let info = read_package_info(&dir).map_err(|e| AgentError::new("package_unreadable", format!("{id}: {e}")))?;
        if (info.inputs, info.has_progress) != (2, true) {
            return Err(invalid(format!("{id} is an effect, not a transition")));
        }
        let store = self.store.as_ref().map(|store| PreviewStore::new(store.root.clone())).ok_or_else(|| AgentError::new("preview_unavailable", "LOCALAPPDATA is not set, so previews have nowhere to be kept"))?;
        let digest = package_digest(&dir).map_err(|e| AgentError::new("package_unreadable", format!("reading {id} failed: {e}")))?;
        let prefix = sized_prefix(id, spec);
        let key = preview_key(&digest, &preview_image_files(), spec);
        let video = store.root.join(format!("{prefix}-{key}.mp4"));
        let read = |store: &PreviewStore| store.read_loop(&prefix, &key, spec).map_err(|e| AgentError::new("preview_unavailable", format!("reading the preview failed: {e}")));
        if let Some(strip) = read(&store)? {
            return transition_answer(id, &strip, spec, format, frames, &video, true);
        }
        if let Some(record) = store.failure(&prefix, &key) {
            let reason = serde_json::from_str::<Value>(&record).ok().and_then(|value| value.get("reason").and_then(Value::as_str).map(str::to_owned)).unwrap_or(record);
            return Err(AgentError::new("preview_failed", format!("{id} could not be previewed: {reason}")));
        }
        let request = PreviewRequest { package_dir: dir, package_prefix: prefix.clone(), key: key.clone(), kind: PreviewJobKind::Loop, spec };
        let session = self.session.get_or_insert_with(|| {
            let mut config = SessionConfig::new();
            config.control_package = paths.package_path(CONTROL_PACKAGE);
            config.loop_timeout = SIZED_LOOP_TIMEOUT;
            PreviewSession::new(Box::new(ProcessLauncher::sibling()), PreviewStore::new(store.root.clone()), config)
        });
        match session.render(&request) {
            RenderOutcome::Published { .. } => match read(&store)? {
                Some(strip) => transition_answer(id, &strip, spec, format, frames, &video, false),
                None => Err(AgentError::new("preview_failed", "the rendered preview was not in the cache")),
            },
            RenderOutcome::NotPreviewable(reason) | RenderOutcome::AlreadyFailed(reason) | RenderOutcome::Failed { reason, .. } | RenderOutcome::GivenUp(reason) => Err(AgentError::new("preview_failed", format!("{id} could not be previewed: {reason}"))),
            RenderOutcome::CoolingDown(reason) => Err(AgentError::new("preview_unavailable", format!("the preview helper is resting after failures: {reason}"))),
            RenderOutcome::Stopped(reason) => {
                self.session = None;
                Err(AgentError::new("preview_unavailable", reason))
            }
            RenderOutcome::Cancelled => Err(AgentError::new("preview_unavailable", "the preview was cancelled")),
        }
    }

    /// `asset_preview`.
    pub fn asset(&mut self, source: &str, format: PreviewFormat, frames: u32, size: u32) -> AgentResult {
        let frames = check_frames(frames)?;
        if size == 0 || size > ASSET_MAX_SIZE {
            return Err(invalid(format!("size {size}: from 1 to {ASSET_MAX_SIZE}")));
        }
        let asset = parse_asset_source(source).map_err(|problem| invalid(problem.to_string()))?;
        if asset.kind != "sticker" {
            return Err(invalid(format!("{source} is a {} item, not a sticker", asset.kind)));
        }
        let file = self.paths().asset_path(source).map_err(|problem| AgentError::new("source_not_found", problem.to_string()))?;
        let content = std::fs::read(&file).map_err(|e| AgentError::new("source_not_found", format!("{}: {e}", file.display())))?;
        let unreadable = |e: String| AgentError::new("unreadable_source", e);
        let animation = read_animation(&file).map_err(unreadable)?;
        let still = match animation {
            Some(_) => None,
            None => Some(read_still(&file).map_err(unreadable)?),
        };
        asset_answer(source, &content, still.as_ref(), animation.as_ref(), format, frames, size, self.asset_root.as_deref())
    }
}

impl Drop for PreviewService {
    fn drop(&mut self) {
        if let Some(session) = &mut self.session {
            session.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-agent-previews-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn frames_and_sizes_are_checked() {
        assert_eq!(check_frames(6).unwrap(), 6);
        for bad in [0, 1, 25] {
            assert_eq!(check_frames(bad).unwrap_err().code, "invalid_params");
        }
        assert_eq!(transition_spec(144).unwrap(), PreviewSpec::STANDARD);
        assert_eq!((transition_spec(360).unwrap().width, transition_spec(360).unwrap().height), (640, 360));
        assert_eq!((transition_spec(720).unwrap().width, transition_spec(720).unwrap().height), (1280, 720));
        assert_eq!(transition_spec(500).unwrap_err().code, "invalid_params");
    }

    #[test]
    fn the_key_and_the_name_include_the_size_but_the_picker_keeps_its_own() {
        let images = preview_image_files();
        let digest = [7u8; 32];
        let keys: Vec<String> = TRANSITION_SIZES.iter().map(|size| preview_key(&digest, &images, transition_spec(*size).unwrap())).collect();
        assert_ne!(keys[0], keys[1]);
        assert_ne!(keys[1], keys[2]);
        assert_eq!(sized_prefix("t/wipe", PreviewSpec::STANDARD), package_prefix("t/wipe"), "the picker's entries are shared");
        assert_ne!(sized_prefix("t/wipe", transition_spec(360).unwrap()), package_prefix("t/wipe"));
        assert_ne!(sized_prefix("t/wipe", transition_spec(360).unwrap()), sized_prefix("t/wipe", transition_spec(720).unwrap()));
        assert_ne!(asset_video_key(b"abc", 100, 100), asset_video_key(b"abc", 200, 200));
        assert_ne!(asset_video_key(b"abc", 100, 100), asset_video_key(b"abd", 100, 100));
        assert!(asset_video_key(b"abc", 100, 50).ends_with("-100x50"));
    }

    #[test]
    fn a_sized_entry_leaves_the_picker_files_alone() {
        let root = scratch("store");
        let store = PreviewStore::new(root.clone());
        let picker = PreviewSpec::STANDARD;
        let strip = |spec: PreviewSpec| PreviewImage { width: spec.width, height: spec.height * 2, rgba: vec![9; (spec.width * spec.height * 8) as usize] };
        store.write_loop(&sized_prefix("t/wipe", picker), "aaaa", picker, &strip(picker)).unwrap();
        let big = transition_spec(360).unwrap();
        store.write_loop(&sized_prefix("t/wipe", big), "bbbb", big, &strip(big)).unwrap();
        assert!(store.read_loop(&sized_prefix("t/wipe", picker), "aaaa", picker).unwrap().is_some());
        assert!(store.read_loop(&sized_prefix("t/wipe", big), "bbbb", big).unwrap().is_some());
    }

    #[test]
    fn pictures_are_spread_evenly() {
        assert_eq!(spread_indexes(24, 6), [0, 5, 9, 14, 18, 23]);
        assert_eq!(spread_indexes(24, 2), [0, 23]);
        assert_eq!(spread_indexes(24, 24), (0..24).collect::<Vec<_>>());
        assert_eq!(spread_indexes(3, 6), [0, 1, 2]);
        assert_eq!(spread_indexes(1, 6), [0]);
        assert_eq!(spread_times(1000, 4), [0, 250, 500, 750]);
        let picture = AnimatedPicture { frames: vec![StillPicture { width: 1, height: 1, rgba: vec![0; 4], has_alpha: false }; 3], ends_ms: vec![100, 500, 1000], loops: 0 };
        assert_eq!(spread_times(1000, 4).into_iter().map(|t| frame_at_ms(&picture, t)).collect::<Vec<_>>(), [0, 1, 2, 2]);
    }

    #[test]
    fn a_sticker_is_never_scaled_up_and_video_sides_are_even() {
        assert_eq!(fit_size(1000, 500, 360, false), (360, 180));
        assert_eq!(fit_size(200, 100, 360, false), (200, 100));
        assert_eq!(fit_size(101, 51, 720, true), (100, 50));
        assert_eq!(fit_size(500, 1000, 361, true), (180, 360));
        assert_eq!(fit_size(1, 1, 360, true), (2, 2));
    }

    #[test]
    fn a_still_sticker_gives_one_picture_and_no_video() {
        let still = StillPicture { width: 800, height: 400, rgba: vec![200; 800 * 400 * 4], has_alpha: false };
        let answer = asset_answer("asset:sticker/p/a.png", b"x", Some(&still), None, PreviewFormat::Frames, 6, 360, None).unwrap();
        assert_eq!(answer["pngs"].as_array().unwrap().len(), 1);
        assert_eq!((answer["width"].as_u64(), answer["height"].as_u64()), (Some(360), Some(180)));
        let video = asset_answer("asset:sticker/p/a.png", b"x", Some(&still), None, PreviewFormat::Video, 6, 360, None).unwrap_err();
        assert_eq!(video.code, "invalid_params");
    }

    #[test]
    fn an_animated_sticker_gives_frames_over_its_loop() {
        let frame = |value: u8| StillPicture { width: 4, height: 2, rgba: vec![value; 32], has_alpha: false };
        let picture = AnimatedPicture { frames: vec![frame(1), frame(2)], ends_ms: vec![500, 1000], loops: 0 };
        let answer = asset_answer("asset:sticker/p/a.gif", b"x", None, Some(&picture), PreviewFormat::Frames, 4, 360, None).unwrap();
        assert_eq!(answer["pngs"].as_array().unwrap().len(), 4);
        assert_eq!(answer["frames"], json!([{"time_ms": 0, "frame": 0}, {"time_ms": 250, "frame": 0}, {"time_ms": 500, "frame": 1}, {"time_ms": 750, "frame": 1}]));
        assert_eq!((answer["width"].as_u64(), answer["height"].as_u64()), (Some(4), Some(2)), "never scaled up");
    }

    #[test]
    fn a_transition_strip_gives_its_progress_values() {
        let spec = PreviewSpec::STANDARD;
        let bytes = (spec.width * spec.height * 4) as usize;
        let strip = PreviewImage { width: spec.width, height: spec.height * spec.frames, rgba: (0..spec.frames).flat_map(|k| vec![k as u8; bytes]).collect() };
        let answer = transition_answer("t/wipe", &strip, spec, PreviewFormat::Frames, 3, Path::new("unused.mp4"), true).unwrap();
        assert_eq!(answer["pngs"].as_array().unwrap().len(), 3);
        let progress: Vec<f64> = answer["frames"].as_array().unwrap().iter().map(|item| item["progress"].as_f64().unwrap()).collect();
        assert_eq!(progress[0], 0.0);
        assert_eq!(progress[2], 1.0);
        assert!((progress[1] - 12.0 / 23.0).abs() < 1e-9);
    }

    #[test]
    fn the_video_file_is_written_once_through_a_temporary_and_then_reused() {
        let root = scratch("mp4");
        let target = root.join("k.mp4");
        let mut seen = None;
        assert!(!ensure_file(&target, |path| {
            assert_ne!(path, target, "written beside the target first");
            assert_eq!(path.parent(), target.parent());
            seen = Some(path.to_path_buf());
            std::fs::write(path, b"video").map_err(|e| e.to_string())
        })
        .unwrap());
        assert_eq!(std::fs::read(&target).unwrap(), b"video");
        assert!(!seen.unwrap().exists(), "the temporary file was renamed");
        assert!(ensure_file(&target, |_| panic!("an existing file is reused")).unwrap());
        let failed = root.join("f.mp4");
        assert!(ensure_file(&failed, |_| Err("no encoder".to_owned())).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "a failure leaves nothing behind");
    }

    #[test]
    fn a_file_uri_escapes_what_it_must() {
        assert_eq!(file_uri(Path::new(r"C:\Users\A B\x#1.mp4")), "file:///C:/Users/A%20B/x%231.mp4");
    }

    #[test]
    fn a_source_that_is_not_a_sticker_is_refused() {
        let mut service = PreviewService { roots: Vec::new(), store: None, asset_root: None, session: None };
        assert_eq!(service.asset("asset:music/m/a.mp3", PreviewFormat::Frames, 6, 360).unwrap_err().code, "invalid_params");
        assert_eq!(service.asset("C:/a.png", PreviewFormat::Frames, 6, 360).unwrap_err().code, "invalid_params");
        assert_eq!(service.asset("asset:sticker/p/a.png", PreviewFormat::Frames, 6, 0).unwrap_err().code, "invalid_params");
        assert_eq!(service.asset("asset:sticker/p/a.png", PreviewFormat::Frames, 6, 721).unwrap_err().code, "invalid_params");
        assert_eq!(service.transition("t/none", PreviewFormat::Frames, 6, 144).unwrap_err().code, "invalid_params");
        assert_eq!(service.transition("t/none", PreviewFormat::Frames, 6, 200).unwrap_err().code, "invalid_params");
    }
}
