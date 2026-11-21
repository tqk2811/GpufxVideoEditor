//! `transition_preview` and `asset_preview`: what a transition or a sticker looks like, for an agent
//! choosing one. The MCP server answers both in its own process in both modes, so an open window
//! never renders them and its UI thread is never held up.
//!
//! A transition preview is a strip of frames. The default size reads and fills the very entries of
//! the effect picker's cache (`PreviewStore`). Every other size, and every MP4 (transitions and
//! stickers alike), lives in a cache of the MCP server's own (`mcp-previews\v1`), so they never take
//! the picker's room or get its files removed; that cache has its own size cap and sweep. A miss is
//! rendered at once by a `PreviewSession` of this process (its own helper, with the same failure
//! and device rules as the picker's), which only shares the files on disk with the window.
//!
//! The work runs on the session thread: the MCP server answers one call at a time, so a render
//! holds the next call back, for at most the loop timeout of its size.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use d3d12_backend::{animation_thumbnail, animation_timing, parse_asset_source, preview_image_files, read_package_info, read_still, still_dimensions, AnimatedPicture, AnimationTiming, EncoderChoice, Mp4Encoder, PreviewJobKind, ProjectPaths, StillPicture, ThumbnailLimits, VideoCodec, PREVIEW_FPS};
use engine::render::FrameSink;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::image::{encode_png, shrink};
use super::protocol::{AgentError, AgentResult};
use crate::effect_previews::child::{PreviewRequest, PreviewSession, ProcessLauncher, RenderOutcome, SessionConfig};
use crate::effect_previews::{package_digest, package_prefix, preview_key, PreviewImage, PreviewSpec, PreviewStore, CONTROL_PACKAGE, TMP_EXTENSION};

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
/// The shortest side an MP4 may have.
pub const MIN_VIDEO_SIDE: u32 = 32;
/// The longest loop a sticker's MP4 may hold.
pub const MAX_ASSET_VIDEO_MS: u64 = 60_000;
/// The most bytes the decoded frames of an animated sticker may take, once shrunk to the size asked
/// for; a sticker past it is refused with `animation_too_large`.
pub const MAX_ANIMATION_BYTES: u64 = 256 * 1024 * 1024;
/// The most frames an animated sticker may have.
pub const MAX_ANIMATION_FRAMES: usize = 2000;
/// Past this the oldest files of the MCP cache go.
pub const CACHE_LIMIT: u64 = 512 * 1024 * 1024;
/// How long a preview that failed without a verdict is not tried again.
const GIVEN_UP_FOR: Duration = Duration::from_secs(10 * 60);
/// Bump when a change makes the sticker MP4s look different.
const ASSET_PREVIEW_VERSION: u32 = 1;

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

/// How long the loop of a transition preview of `spec` may take: the picker's own timeout for its
/// size, twice it for 360 and three times for 720, which have 11 and 44 times its pixels but render
/// on the GPU, where the time grows far slower than the pixels.
pub fn loop_timeout(spec: PreviewSpec) -> Duration {
    let picker = SessionConfig::new().loop_timeout;
    match spec.height {
        ..=144 => picker,
        ..=360 => picker * 2,
        _ => picker * 3,
    }
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

/// The folder the strips of `spec` live in: the picker's cache for its own spec, the MCP cache for
/// any other.
pub fn strip_root<'a>(spec: PreviewSpec, picker: &'a Path, cache: &'a Path) -> &'a Path {
    if spec == PreviewSpec::STANDARD {
        picker
    } else {
        cache
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

/// The frame of `picture` shown `ms` after its loop starts. The decoders already make a frame of
/// 0 to 10 ms last 100 ms, as browsers do, so every frame has a length and time 0 is the first.
fn frame_at_ms(picture: &AnimatedPicture, ms: u64) -> usize {
    picture.ends_ms.partition_point(|end| *end <= ms).min(picture.frames.len().saturating_sub(1))
}

/// The size of a picture of `width` x `height` with its long edge at most `long`, never larger than
/// it is, and at least 1 a side.
pub fn fit_size(width: u32, height: u32, long: u32) -> (u32, u32) {
    let native = width.max(height).max(1);
    let target = long.min(native);
    let scale = |side: u32| ((u64::from(side) * u64::from(target) + u64::from(native) / 2) / u64::from(native)).max(1) as u32;
    (scale(width), scale(height))
}

/// The size of an MP4 of a picture of `width` x `height` with its long edge at most `long`: even
/// sides, as 4:2:0 video needs, never larger than the picture, and at least [`MIN_VIDEO_SIDE`]. A
/// short side below that is raised to it, which a picture smaller than that cannot give.
pub fn fit_video_size(width: u32, height: u32, long: u32) -> Result<(u32, u32), AgentError> {
    let short_native = width.min(height);
    if short_native < MIN_VIDEO_SIDE {
        return Err(invalid(format!("{width}x{height} is too small for a video, whose sides must be at least {MIN_VIDEO_SIDE} pixels; ask for frames")));
    }
    let (mut w, mut h) = fit_size(width, height, long);
    if w.min(h) < MIN_VIDEO_SIDE {
        // The long edge that makes the short side MIN_VIDEO_SIDE, at most the native one.
        let raised = (u64::from(MIN_VIDEO_SIDE) * u64::from(width.max(height))).div_ceil(u64::from(short_native)) as u32;
        (w, h) = fit_size(width, height, raised);
    }
    Ok(((w & !1).max(MIN_VIDEO_SIDE), (h & !1).max(MIN_VIDEO_SIDE)))
}

/// The frames of a strip of `spec`, top to bottom, borrowed from it.
pub fn strip_frames(strip: &PreviewImage, spec: PreviewSpec) -> Vec<&[u8]> {
    let bytes = spec.width as usize * spec.height as usize * 4;
    strip.rgba.chunks_exact(bytes).collect()
}

fn escape_into(uri: &mut String, text: &str) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => uri.push(byte as char),
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
}

/// `file:` URI of an absolute path, with what is not plainly safe escaped: `file:///C:/...` for a
/// drive, `file://server/share/...` for a UNC path, `\\?\` prefixes included.
pub fn file_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let (host, rest): (&str, &str) = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        unc.split_once(['\\', '/']).unwrap_or((unc, ""))
    } else if let Some(local) = raw.strip_prefix(r"\\?\") {
        ("", local)
    } else if let Some(unc) = raw.strip_prefix(r"\\").or_else(|| raw.strip_prefix("//")) {
        unc.split_once(['\\', '/']).unwrap_or((unc, ""))
    } else {
        ("", &raw)
    };
    let mut uri = String::from("file://");
    escape_into(&mut uri, host);
    uri.push('/');
    escape_into(&mut uri, rest.replace('\\', "/").trim_start_matches('/'));
    uri
}

/// Creates an empty sibling of `target` under a name no file had, `<name>.<pid>-<n>.tmp`, so the
/// store's sweep removes it should the process die while writing it.
fn reserve_temporary(target: &Path) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let name = target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    for _ in 0..10_000 {
        let candidate = target.with_file_name(format!("{name}.{}-{}.{TMP_EXTENSION}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("no free temporary name next to {}", target.display())))
}

fn file_is_there(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// Makes `target` with `encode` unless it is already there, through a temporary name beside it
/// that is renamed into place, so a reader never sees half a file. Returns whether the file was
/// already there. When the rename fails but another writer finished the file first, that counts.
pub fn ensure_file(target: &Path, encode: impl FnOnce(&Path) -> Result<(), String>) -> Result<bool, String> {
    if file_is_there(target) {
        return Ok(true);
    }
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
    }
    let temporary = reserve_temporary(target).map_err(|e| format!("reserving a file next to {}: {e}", target.display()))?;
    let result = encode(&temporary).and_then(|()| std::fs::rename(&temporary, target).map_err(|e| format!("moving the video to {}: {e}", target.display())));
    if let Err(message) = result {
        let _ = std::fs::remove_file(&temporary);
        if file_is_there(target) {
            return Ok(true);
        }
        return Err(message);
    }
    Ok(false)
}

/// Encodes `count` frames (RGBA8 of `width` x `height`, both even), each asked of `frame` when it is
/// written, as an H.264 MP4 at `fps`. The MP4 muxer is named, so `path` may end in `.tmp`.
pub fn encode_mp4<'a>(path: &Path, count: usize, width: u32, height: u32, fps: u32, mut frame: impl FnMut(usize) -> Cow<'a, [u8]>) -> Result<(), String> {
    let output = engine::Output { width, height, frame_rate: engine::FrameRate { num: fps, den: 1 }, audio: None };
    let mut encoder = Mp4Encoder::create_mp4(path, &output, &EncoderChoice::Auto(VideoCodec::H264))?;
    for index in 0..count {
        encoder.write_frame(index as i64, &frame(index)).map_err(|e| e.0)?;
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

/// `%LOCALAPPDATA%\gpufx-editor\mcp-previews\v1`, beside the effect previews: the sized strips and
/// every MP4 of the MCP server.
pub fn default_cache_root() -> Option<PathBuf> {
    PreviewStore::default_root().and_then(|root| Some(root.parent()?.parent()?.join("mcp-previews").join("v1")))
}

/// Removes stale temporary files of `root`, then its oldest files while it is over
/// [`CACHE_LIMIT`].
pub fn sweep_cache(root: &Path, limit: u64) {
    let _ = PreviewStore::new(root.to_path_buf()).sweep(limit, SystemTime::now());
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

fn merged(mut answer: Value, base: Value) -> Value {
    if let Value::Object(base) = base {
        for (key, value) in base {
            answer[key] = value;
        }
    }
    answer
}

/// The note beside a transition's MP4: how many frames it holds and whether the strip was static,
/// so the MP4 can be given again without reading the strip.
fn video_note_path(video: &Path) -> PathBuf {
    let mut name = video.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    video.with_file_name(name)
}

fn transition_base(id: &str, spec: PreviewSpec, is_static: bool, cached: bool) -> Value {
    json!({"id": id, "width": spec.width, "height": spec.height, "static": is_static, "cached": cached})
}

/// The answer for a transition's MP4 that is already there, with its note, or `None` when either
/// is missing.
pub fn reused_transition_video(id: &str, spec: PreviewSpec, video: &Path) -> Option<Value> {
    if !file_is_there(video) {
        return None;
    }
    let note: Value = serde_json::from_slice(&std::fs::read(video_note_path(video)).ok()?).ok()?;
    let frames = usize::try_from(note.get("frames")?.as_u64()?).ok()?;
    let is_static = note.get("static")?.as_bool()?;
    Some(merged(video_json(video, true, spec.width, spec.height, PREVIEW_FPS, frames), transition_base(id, spec, is_static, true)))
}

/// The answer for a transition whose strip is at hand: `frames` pictures, or an MP4 at `video`.
pub fn transition_answer(id: &str, strip: &PreviewImage, spec: PreviewSpec, format: PreviewFormat, frames: u32, video: &Path, cached: bool) -> AgentResult {
    let pictures = strip_frames(strip, spec);
    if pictures.is_empty() {
        return Err(AgentError::new("preview_failed", format!("the preview of {id} has no frame")));
    }
    let is_static = pictures.len() == 1;
    let base = transition_base(id, spec, is_static, cached);
    match format {
        PreviewFormat::Frames => {
            let mut answer = base;
            let mut items = Vec::new();
            let mut pngs = Vec::new();
            // Spread over the frames the strip has; a static strip has one, which every progress shows.
            let last = pictures.len() - 1;
            for index in spread_indexes(pictures.len(), frames as usize) {
                let progress = if last == 0 { 0.0 } else { index as f64 / last as f64 };
                items.push(json!({"frame": index, "progress": progress}));
                pngs.push(png_base64(pictures[index], spec.width, spec.height)?);
            }
            answer["frames"] = Value::Array(items);
            answer["pngs"] = json!(pngs);
            Ok(answer)
        }
        PreviewFormat::Video => {
            let count = if is_static { spec.frames as usize } else { pictures.len() };
            let reused = ensure_file(video, |path| encode_mp4(path, count, spec.width, spec.height, PREVIEW_FPS, |index| Cow::Borrowed(pictures[if is_static { 0 } else { index }]))).map_err(|e| AgentError::new("encode_failed", e))?;
            // Without the note the next call reads the strip again, which is only slower.
            let _ = std::fs::write(video_note_path(video), json!({"frames": count, "static": is_static}).to_string());
            Ok(merged(video_json(video, reused, spec.width, spec.height, PREVIEW_FPS, count), base))
        }
    }
}

fn scaled(picture: &StillPicture, width: u32, height: u32) -> Cow<'_, [u8]> {
    if (picture.width, picture.height) == (width, height) {
        Cow::Borrowed(&picture.rgba)
    } else {
        Cow::Owned(shrink(&picture.rgba, picture.width, picture.height, width, height))
    }
}

/// The answer for a still sticker.
pub fn asset_still_answer(source: &str, still: &StillPicture, format: PreviewFormat, size: u32) -> AgentResult {
    if format == PreviewFormat::Video {
        return Err(invalid(format!("{source} is a still sticker, which has no video; ask for frames")));
    }
    let (width, height) = fit_size(still.width, still.height, size);
    Ok(json!({"source": source, "animated": false, "width": width, "height": height, "native_width": still.width, "native_height": still.height, "pngs": [png_base64(&scaled(still, width, height), width, height)?]}))
}

fn animated_base(source: &str, native: (u32, u32), length_ms: u64) -> Value {
    json!({"source": source, "animated": true, "native_width": native.0, "native_height": native.1, "duration_ms": length_ms})
}

/// The answer of an animated sticker of `native` size as `frames` pictures, from its frames already
/// shrunk to the size asked for.
pub fn asset_frames_answer(source: &str, native: (u32, u32), animation: &AnimatedPicture, frames: u32) -> AgentResult {
    let first = animation.frames.first().ok_or_else(|| AgentError::new("unreadable_source", format!("{source} has no frame")))?;
    let (width, height) = (first.width, first.height);
    let length_ms = animation.ends_ms.last().copied().unwrap_or(0);
    let mut answer = animated_base(source, native, length_ms);
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

/// How many frames an MP4 of one loop of `length_ms` at [`ASSET_VIDEO_FPS`] has.
fn asset_video_frames(length_ms: u64) -> usize {
    (length_ms * u64::from(ASSET_VIDEO_FPS)).div_ceil(1000).max(1) as usize
}

/// Encodes one loop of `animation` as an MP4 of `width` x `height`.
pub fn encode_asset_video(path: &Path, animation: &AnimatedPicture, width: u32, height: u32) -> Result<(), String> {
    let length_ms = animation.ends_ms.last().copied().unwrap_or(0);
    let count = asset_video_frames(length_ms);
    encode_mp4(path, count, width, height, ASSET_VIDEO_FPS, |k| scaled(&animation.frames[frame_at_ms(animation, k as u64 * 1000 / u64::from(ASSET_VIDEO_FPS))], width, height))
}

/// Refuses a loop longer than an MP4 may hold.
pub fn check_video_length(source: &str, length_ms: u64) -> Result<(), AgentError> {
    if length_ms > MAX_ASSET_VIDEO_MS {
        return Err(AgentError::new("animation_too_long", format!("{source} loops for {length_ms} ms, more than the {MAX_ASSET_VIDEO_MS} ms a video may hold; ask for frames")));
    }
    Ok(())
}

/// Refuses frames that stop before the end of the loop: the decoder stopped at
/// [`MAX_ANIMATION_BYTES`] or [`MAX_ANIMATION_FRAMES`].
pub fn check_whole_loop(source: &str, animation: &AnimatedPicture, timing: AnimationTiming) -> Result<(), AgentError> {
    let decoded = animation.ends_ms.last().copied().unwrap_or(0);
    if decoded < timing.loop_ms {
        return Err(AgentError::new("animation_too_large", format!("{source}: its frames take more than {} MiB or {MAX_ANIMATION_FRAMES} frames at this size (only {decoded} of its {} ms fit); ask for a smaller size", MAX_ANIMATION_BYTES / (1024 * 1024), timing.loop_ms)));
    }
    Ok(())
}

/// Decodes the frames of an animated sticker, each shrunk to `height` as soon as it is decoded, and
/// refuses one whose whole loop does not fit the limits.
fn decode_animation(source: &str, file: &Path, height: u32, timing: AnimationTiming) -> Result<AnimatedPicture, AgentError> {
    let unreadable = |e: String| AgentError::new("unreadable_source", e);
    let limits = ThumbnailLimits { height, max_frames: MAX_ANIMATION_FRAMES, max_bytes: MAX_ANIMATION_BYTES };
    let animation = animation_thumbnail(file, limits, &|| false).map_err(unreadable)?.ok_or_else(|| unreadable(format!("{source} is not animated")))?;
    check_whole_loop(source, &animation, timing)?;
    Ok(animation)
}

/// Answers the preview tools. It lives on the MCP server's session thread, in both modes, and
/// starts a helper only when a transition preview misses the cache.
pub struct PreviewService {
    roots: Vec<PathBuf>,
    /// The picker's cache, `effect-previews\v1`.
    picker: Option<PathBuf>,
    /// The MCP server's cache, `mcp-previews\v1`.
    cache: Option<PathBuf>,
    /// Renders into the picker's cache.
    picker_session: Option<PreviewSession>,
    /// Renders the other sizes into the MCP cache.
    sized_session: Option<PreviewSession>,
}

impl PreviewService {
    /// The real one: the user's cache folders and the helper beside the running program. The MCP
    /// cache is swept at once.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let service = Self { roots, picker: PreviewStore::default_root(), cache: default_cache_root(), picker_session: None, sized_session: None };
        service.sweep();
        service
    }

    fn sweep(&self) {
        if let Some(cache) = &self.cache {
            sweep_cache(cache, CACHE_LIMIT);
        }
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
        let (Some(picker), Some(cache)) = (self.picker.clone(), self.cache.clone()) else {
            return Err(AgentError::new("preview_unavailable", "LOCALAPPDATA is not set, so previews have nowhere to be kept"));
        };
        let store = PreviewStore::new(strip_root(spec, &picker, &cache).to_path_buf());
        let digest = package_digest(&dir).map_err(|e| AgentError::new("package_unreadable", format!("reading {id} failed: {e}")))?;
        let prefix = sized_prefix(id, spec);
        let key = preview_key(&digest, &preview_image_files(), spec);
        let video = cache.join(format!("{prefix}-{key}.mp4"));
        if format == PreviewFormat::Video {
            if let Some(answer) = reused_transition_video(id, spec, &video) {
                return Ok(answer);
            }
        }
        let read = |store: &PreviewStore| store.read_loop(&prefix, &key, spec).map_err(|e| AgentError::new("preview_unavailable", format!("reading the preview failed: {e}")));
        if let Some(strip) = read(&store)? {
            let answer = transition_answer(id, &strip, spec, format, frames, &video, true);
            if format == PreviewFormat::Video {
                self.sweep();
            }
            return answer;
        }
        if let Some(record) = store.failure(&prefix, &key) {
            let reason = serde_json::from_str::<Value>(&record).ok().and_then(|value| value.get("reason").and_then(Value::as_str).map(str::to_owned)).unwrap_or(record);
            return Err(AgentError::new("preview_failed", format!("{id} could not be previewed: {reason}")));
        }
        let request = PreviewRequest { package_dir: dir, package_prefix: prefix.clone(), key: key.clone(), kind: PreviewJobKind::Loop, spec };
        let slot = if spec == PreviewSpec::STANDARD { &mut self.picker_session } else { &mut self.sized_session };
        let session = slot.get_or_insert_with(|| {
            let mut config = SessionConfig::new();
            config.control_package = paths.package_path(CONTROL_PACKAGE);
            config.given_up_for = Some(GIVEN_UP_FOR);
            PreviewSession::new(Box::new(ProcessLauncher::sibling()), PreviewStore::new(store.root.clone()), config)
        });
        session.set_loop_timeout(loop_timeout(spec));
        let outcome = session.render(&request);
        if matches!(outcome, RenderOutcome::Stopped(_)) {
            *slot = None;
        }
        let answer = match outcome {
            RenderOutcome::Published { .. } => match read(&store)? {
                Some(strip) => transition_answer(id, &strip, spec, format, frames, &video, false),
                None => Err(AgentError::new("preview_failed", "the rendered preview was not in the cache")),
            },
            RenderOutcome::NotPreviewable(reason) | RenderOutcome::AlreadyFailed(reason) | RenderOutcome::Failed { reason, .. } | RenderOutcome::GivenUp(reason) => Err(AgentError::new("preview_failed", format!("{id} could not be previewed: {reason}"))),
            RenderOutcome::CoolingDown(reason) => Err(AgentError::new("preview_unavailable", format!("the preview helper is resting after failures: {reason}"))),
            RenderOutcome::Stopped(reason) => Err(AgentError::new("preview_unavailable", reason)),
            RenderOutcome::Cancelled => Err(AgentError::new("preview_unavailable", "the preview was cancelled")),
        };
        if spec != PreviewSpec::STANDARD || format == PreviewFormat::Video {
            self.sweep();
        }
        answer
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
        if !file.is_file() {
            return Err(AgentError::new("source_not_found", format!("{} is not there", file.display())));
        }
        let unreadable = |e: String| AgentError::new("unreadable_source", e);
        // Reads the blocks only; no frame is decoded yet.
        let Some(timing) = animation_timing(&file).map_err(unreadable)? else {
            let still = read_still(&file).map_err(unreadable)?;
            return asset_still_answer(source, &still, format, size);
        };
        let native = still_dimensions(&file).map_err(unreadable)?;
        match format {
            PreviewFormat::Frames => {
                let (_, height) = fit_size(native.0, native.1, size);
                let animation = decode_animation(source, &file, height, timing)?;
                asset_frames_answer(source, native, &animation, frames)
            }
            PreviewFormat::Video => {
                let root = self.cache.clone().ok_or_else(|| AgentError::new("preview_unavailable", "LOCALAPPDATA is not set, so the video has nowhere to be kept"))?;
                check_video_length(source, timing.loop_ms)?;
                let (width, height) = fit_video_size(native.0, native.1, size)?;
                let content = std::fs::read(&file).map_err(|e| AgentError::new("source_not_found", format!("{}: {e}", file.display())))?;
                let target = root.join(format!("asset-{}.mp4", asset_video_key(&content, width, height)));
                let count = asset_video_frames(timing.loop_ms);
                // An MP4 already made needs no frame decoded.
                let reused = if file_is_there(&target) {
                    true
                } else {
                    let animation = decode_animation(source, &file, height, timing)?;
                    let reused = ensure_file(&target, |path| encode_asset_video(path, &animation, width, height)).map_err(|e| AgentError::new("encode_failed", e))?;
                    self.sweep();
                    reused
                };
                Ok(merged(video_json(&target, reused, width, height, ASSET_VIDEO_FPS, count), animated_base(source, native, timing.loop_ms)))
            }
        }
    }
}

impl Drop for PreviewService {
    fn drop(&mut self) {
        for session in [&mut self.picker_session, &mut self.sized_session].into_iter().flatten() {
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

    fn strip_of(spec: PreviewSpec, count: u32) -> PreviewImage {
        let bytes = (spec.width * spec.height * 4) as usize;
        PreviewImage { width: spec.width, height: spec.height * count, rgba: (0..count).flat_map(|k| vec![k as u8; bytes]).collect() }
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
    fn the_picker_size_has_the_picker_timeout_and_larger_sizes_longer_ones() {
        let picker = SessionConfig::new().loop_timeout;
        assert_eq!(loop_timeout(transition_spec(144).unwrap()), picker);
        assert_eq!(loop_timeout(transition_spec(360).unwrap()), picker * 2);
        assert_eq!(loop_timeout(transition_spec(720).unwrap()), picker * 3);
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
    fn sized_strips_go_to_the_mcp_cache_and_the_picker_size_to_the_picker_cache() {
        let (picker, cache) = (Path::new("picker"), Path::new("cache"));
        assert_eq!(strip_root(PreviewSpec::STANDARD, picker, cache), picker);
        assert_eq!(strip_root(transition_spec(360).unwrap(), picker, cache), cache);
        assert_eq!(strip_root(transition_spec(720).unwrap(), picker, cache), cache);
        if let (Some(picker), Some(cache)) = (PreviewStore::default_root(), default_cache_root()) {
            assert_ne!(picker, cache);
            assert!(cache.ends_with(Path::new("mcp-previews").join("v1")), "{}", cache.display());
            assert_eq!(picker.parent().unwrap().parent(), cache.parent().unwrap().parent(), "beside the effect previews");
        }
    }

    #[test]
    fn the_mcp_cache_sweep_removes_the_oldest_files_and_stale_temporaries() {
        let root = scratch("sweep");
        let old = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        let write = |name: &str, bytes: usize, at: SystemTime| {
            let path = root.join(name);
            std::fs::write(&path, vec![1u8; bytes]).unwrap();
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(at).unwrap();
            path
        };
        let oldest = write("a.mp4", 100, old);
        let newer = write("b.mp4", 100, old + Duration::from_secs(60));
        let stale = write("c.mp4.1-1.tmp", 10, old);
        sweep_cache(&root, 150);
        assert!(!stale.exists(), "a stale temporary goes");
        assert!(!oldest.exists(), "the oldest goes while over the cap");
        assert!(newer.exists());
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
    fn a_sticker_is_never_scaled_up() {
        assert_eq!(fit_size(1000, 500, 360), (360, 180));
        assert_eq!(fit_size(200, 100, 360), (200, 100));
        assert_eq!(fit_size(1, 1, 360), (1, 1));
    }

    #[test]
    fn a_video_has_even_sides_of_at_least_32_and_never_more_than_the_sticker() {
        assert_eq!(fit_video_size(101, 51, 720).unwrap(), (100, 50));
        assert_eq!(fit_video_size(500, 1000, 361).unwrap(), (180, 360));
        assert_eq!(fit_video_size(1000, 100, 100).unwrap(), (320, 32), "the short side is raised to 32");
        assert_eq!(fit_video_size(33, 33, 720).unwrap(), (32, 32));
        for (width, height) in [(1, 1), (31, 500), (500, 31)] {
            assert_eq!(fit_video_size(width, height, 360).unwrap_err().code, "invalid_params", "{width}x{height}");
        }
    }

    #[test]
    fn a_still_sticker_gives_one_picture_and_no_video() {
        let still = StillPicture { width: 800, height: 400, rgba: vec![200; 800 * 400 * 4], has_alpha: false };
        let answer = asset_still_answer("asset:sticker/p/a.png", &still, PreviewFormat::Frames, 360).unwrap();
        assert_eq!(answer["pngs"].as_array().unwrap().len(), 1);
        assert_eq!((answer["width"].as_u64(), answer["height"].as_u64()), (Some(360), Some(180)));
        let video = asset_still_answer("asset:sticker/p/a.png", &still, PreviewFormat::Video, 360).unwrap_err();
        assert_eq!(video.code, "invalid_params");
    }

    #[test]
    fn an_animated_sticker_gives_frames_over_its_loop() {
        let frame = |value: u8| StillPicture { width: 4, height: 2, rgba: vec![value; 32], has_alpha: false };
        let picture = AnimatedPicture { frames: vec![frame(1), frame(2)], ends_ms: vec![500, 1000], loops: 0 };
        let answer = asset_frames_answer("asset:sticker/p/a.gif", (4, 2), &picture, 4).unwrap();
        assert_eq!(answer["pngs"].as_array().unwrap().len(), 4);
        assert_eq!(answer["frames"], json!([{"time_ms": 0, "frame": 0}, {"time_ms": 250, "frame": 0}, {"time_ms": 500, "frame": 1}, {"time_ms": 750, "frame": 1}]));
        assert_eq!((answer["width"].as_u64(), answer["height"].as_u64()), (Some(4), Some(2)), "never scaled up");
    }

    #[test]
    fn frames_cut_short_by_the_limits_are_refused_and_long_loops_get_no_video() {
        let frame = StillPicture { width: 2, height: 2, rgba: vec![0; 16], has_alpha: false };
        let picture = AnimatedPicture { frames: vec![frame.clone(), frame], ends_ms: vec![100, 200], loops: 0 };
        assert!(check_whole_loop("s", &picture, AnimationTiming { loop_ms: 200, loops: 0 }).is_ok());
        assert_eq!(check_whole_loop("s", &picture, AnimationTiming { loop_ms: 5000, loops: 0 }).unwrap_err().code, "animation_too_large");
        assert!(check_video_length("s", MAX_ASSET_VIDEO_MS).is_ok());
        assert_eq!(check_video_length("s", MAX_ASSET_VIDEO_MS + 1).unwrap_err().code, "animation_too_long");
    }

    #[test]
    fn a_transition_strip_gives_its_progress_values() {
        let spec = PreviewSpec::STANDARD;
        let answer = transition_answer("t/wipe", &strip_of(spec, spec.frames), spec, PreviewFormat::Frames, 3, Path::new("unused.mp4"), true).unwrap();
        assert_eq!(answer["pngs"].as_array().unwrap().len(), 3);
        let progress: Vec<f64> = answer["frames"].as_array().unwrap().iter().map(|item| item["progress"].as_f64().unwrap()).collect();
        assert_eq!(progress[0], 0.0);
        assert_eq!(progress[2], 1.0);
        assert!((progress[1] - 12.0 / 23.0).abs() < 1e-9);
    }

    #[test]
    fn progress_comes_from_the_frames_the_strip_has() {
        let spec = PreviewSpec::STANDARD;
        let answer = transition_answer("t/wipe", &strip_of(spec, 12), spec, PreviewFormat::Frames, 2, Path::new("unused.mp4"), true).unwrap();
        assert_eq!(answer["frames"], json!([{"frame": 0, "progress": 0.0}, {"frame": 11, "progress": 1.0}]));
        let still = transition_answer("t/fade", &strip_of(spec, 1), spec, PreviewFormat::Frames, 6, Path::new("unused.mp4"), true).unwrap();
        assert_eq!(still["static"], true);
        assert_eq!(still["frames"], json!([{"frame": 0, "progress": 0.0}]), "a static strip gives its one picture");
    }

    #[test]
    fn a_transition_video_with_its_note_is_given_again_without_the_strip() {
        let root = scratch("reuse");
        let spec = PreviewSpec::STANDARD;
        let video = root.join("abcd1234-key.mp4");
        assert!(reused_transition_video("t/wipe", spec, &video).is_none());
        std::fs::write(&video, b"video").unwrap();
        assert!(reused_transition_video("t/wipe", spec, &video).is_none(), "no note: the strip is read");
        std::fs::write(video_note_path(&video), r#"{"frames":24,"static":false}"#).unwrap();
        let answer = reused_transition_video("t/wipe", spec, &video).unwrap();
        assert_eq!((answer["reused"].as_bool(), answer["frames"].as_u64(), answer["static"].as_bool()), (Some(true), Some(24), Some(false)));
    }

    #[test]
    fn the_video_file_is_written_once_through_a_temporary_and_then_reused() {
        let root = scratch("mp4");
        let target = root.join("k.mp4");
        let mut seen = None;
        assert!(!ensure_file(&target, |path| {
            assert_ne!(path, target, "written beside the target first");
            assert_eq!(path.parent(), target.parent());
            assert_eq!(path.extension().unwrap(), TMP_EXTENSION, "the sweep knows it");
            assert!(path.file_name().unwrap().to_string_lossy().starts_with("k.mp4."));
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
        assert_eq!(file_uri(Path::new(r"\\server\share\a b.mp4")), "file://server/share/a%20b.mp4");
        assert_eq!(file_uri(Path::new(r"\\?\C:\x\y.mp4")), "file:///C:/x/y.mp4");
        assert_eq!(file_uri(Path::new(r"\\?\UNC\server\share\y.mp4")), "file://server/share/y.mp4");
    }

    #[test]
    fn a_source_that_is_not_a_sticker_is_refused() {
        let mut service = PreviewService { roots: Vec::new(), picker: None, cache: None, picker_session: None, sized_session: None };
        assert_eq!(service.asset("asset:music/m/a.mp3", PreviewFormat::Frames, 6, 360).unwrap_err().code, "invalid_params");
        assert_eq!(service.asset("C:/a.png", PreviewFormat::Frames, 6, 360).unwrap_err().code, "invalid_params");
        assert_eq!(service.asset("asset:sticker/p/a.png", PreviewFormat::Frames, 6, 0).unwrap_err().code, "invalid_params");
        assert_eq!(service.asset("asset:sticker/p/a.png", PreviewFormat::Frames, 6, 721).unwrap_err().code, "invalid_params");
        assert_eq!(service.transition("t/none", PreviewFormat::Frames, 6, 144).unwrap_err().code, "invalid_params");
        assert_eq!(service.transition("t/none", PreviewFormat::Frames, 6, 200).unwrap_err().code, "invalid_params");
    }
}
