//! The render loop: scheduler results driven through decoder, compositor and sink traits.
//!
//! This module holds no GPU, FFmpeg or platform code. A backend implements [`Decoder`],
//! [`Compositor`] and [`FrameSink`]; the D3D12 one lives in the `d3d12-backend` crate. A Vulkan
//! backend would implement the same traits and leave the project model and scheduler untouched.
//!
//! The loop is single threaded. Within one output frame it decodes the frames of a layer, calls
//! [`Decoder::wait_idle`] on every decoder involved, composites, and reads the result. Decoders are
//! opened per clip the first time the clip is needed and dropped once the clip is no longer in use.

use crate::audio_mix::{mix_audio, AudioSource, MixReport};
use crate::audio_schedule::{sample_position, MIX_CHANNELS};
use crate::project::{Effect, Project};
use crate::schedule::{
    frame_count, frame_time, layers_at_frame, LayerContent, Progress, SourceSample,
};
use crate::time::Time;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt;

/// A failure anywhere in the render. The text names the frame or file when it is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError(pub String);

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

impl From<String> for RenderError {
    fn from(message: String) -> Self {
        RenderError(message)
    }
}

impl From<&str> for RenderError {
    fn from(message: &str) -> Self {
        RenderError(message.to_owned())
    }
}

/// Sequential access to the frames of one source.
pub trait Decoder {
    /// A decoded frame, valid at least until the compositor has finished reading it.
    type Frame;

    /// The frame shown at `time` inside the source. Times before the first frame give the first
    /// frame and times after the last frame give the last one.
    fn frame_at(&mut self, time: Time) -> Result<Self::Frame, RenderError>;

    /// Waits until no decode still running can touch a frame this decoder returned. Called after
    /// the frames of a layer are decoded and before they enter the compositor.
    fn wait_idle(&mut self) -> Result<(), RenderError>;
}

/// Identifies one clip of a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClipKey {
    /// Index of the track.
    pub track: usize,
    /// Index of the clip in its track.
    pub clip: usize,
}

/// The output frame being composited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameContext {
    /// Index of the output frame.
    pub index: i64,
    /// Start time of the output frame.
    pub time: Time,
}

/// GPU compositing, behind whatever graphics API the backend uses.
///
/// An image has the project's output size and belongs to the compositor. Every image the loop
/// obtains is given back through [`Compositor::release`], exactly once.
pub trait Compositor {
    /// The decoder this backend opens.
    type Decoder: Decoder;
    /// A handle to an image of the project's output size.
    type Image: Copy;

    /// Opens `source`, the path as written in the project.
    fn open_decoder(&mut self, source: &str) -> Result<Self::Decoder, RenderError>;

    /// Resizes `frame` to the project size and applies the clip's `effects` to it.
    fn prepare_clip(
        &mut self,
        key: ClipKey,
        frame: &mut <Self::Decoder as Decoder>::Frame,
        effects: &[Effect],
        at: FrameContext,
    ) -> Result<Self::Image, RenderError>;

    /// Runs the transition `package` from `from` to `to`.
    fn transition(
        &mut self,
        package: &str,
        params: &Map<String, Value>,
        from: Self::Image,
        to: Self::Image,
        progress: Progress,
        at: FrameContext,
    ) -> Result<Self::Image, RenderError>;

    /// Draws `top` over `base`.
    fn composite(
        &mut self,
        base: Self::Image,
        top: Self::Image,
        at: FrameContext,
    ) -> Result<Self::Image, RenderError>;

    /// An opaque black image, for frames no clip covers.
    fn blank(&mut self) -> Result<Self::Image, RenderError>;

    /// Waits for `image` and writes it to `rgba` as tightly packed RGBA8 rows.
    fn read(&mut self, image: Self::Image, rgba: &mut Vec<u8>) -> Result<(), RenderError>;

    /// Gives an image back.
    fn release(&mut self, image: Self::Image);
}

/// Where the finished frames go.
pub trait FrameSink {
    /// Receives output frame `index` as tightly packed RGBA8 rows. Frames arrive in order.
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError>;

    /// Receives the next mixed samples, interleaved stereo at 48 kHz, in order and without gaps.
    /// Called only for a project that has an audio output, after the frame they accompany.
    fn write_audio(&mut self, _samples: &[f32]) -> Result<(), RenderError> {
        Ok(())
    }
}

/// What a finished render produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderSummary {
    /// Number of frames given to the sink.
    pub frames: i64,
    /// Number of mixed stereo samples given to the sink; 0 for a project without audio output.
    pub audio_samples: i64,
    /// Channel samples that had to be clamped during the audio mix.
    pub clamped_samples: u64,
}

/// Decoded frames kept alive until the compositor has read the output of the current frame.
struct FrameStore<F>(Vec<F>);

fn decode_sample<C: Compositor>(
    compositor: &mut C,
    decoders: &mut HashMap<ClipKey, C::Decoder>,
    key: ClipKey,
    sample: &SourceSample<'_>,
) -> Result<<C::Decoder as Decoder>::Frame, RenderError> {
    if !decoders.contains_key(&key) {
        let decoder = compositor.open_decoder(sample.source)?;
        decoders.insert(key, decoder);
    }
    let decoder = decoders.get_mut(&key).expect("inserted above");
    decoder.frame_at(sample.source_time)
}

/// Decodes the one or two samples of a layer and makes the decoders idle, then prepares them.
fn render_layer<C: Compositor>(
    compositor: &mut C,
    decoders: &mut HashMap<ClipKey, C::Decoder>,
    kept: &mut FrameStore<<C::Decoder as Decoder>::Frame>,
    track: usize,
    content: &LayerContent<'_>,
    at: FrameContext,
) -> Result<C::Image, RenderError> {
    let key = |clip| ClipKey { track, clip };
    match content {
        LayerContent::Clip(sample) => {
            let mut frame = decode_sample(compositor, decoders, key(sample.clip), sample)?;
            decoders
                .get_mut(&key(sample.clip))
                .expect("just used")
                .wait_idle()?;
            let image =
                compositor.prepare_clip(key(sample.clip), &mut frame, sample.effects, at)?;
            kept.0.push(frame);
            Ok(image)
        }
        LayerContent::Transition {
            package,
            params,
            from,
            to,
            progress,
        } => {
            let mut from_frame = decode_sample(compositor, decoders, key(from.clip), from)?;
            let mut to_frame = decode_sample(compositor, decoders, key(to.clip), to)?;
            decoders
                .get_mut(&key(from.clip))
                .expect("just used")
                .wait_idle()?;
            decoders
                .get_mut(&key(to.clip))
                .expect("just used")
                .wait_idle()?;
            let from_image =
                compositor.prepare_clip(key(from.clip), &mut from_frame, from.effects, at)?;
            let to_image =
                match compositor.prepare_clip(key(to.clip), &mut to_frame, to.effects, at) {
                    Ok(image) => image,
                    Err(e) => {
                        compositor.release(from_image);
                        return Err(e);
                    }
                };
            let result =
                compositor.transition(package, params, from_image, to_image, *progress, at);
            compositor.release(from_image);
            compositor.release(to_image);
            kept.0.push(from_frame);
            kept.0.push(to_frame);
            result
        }
    }
}

fn layer_clips(content: &LayerContent<'_>, track: usize) -> Vec<ClipKey> {
    match content {
        LayerContent::Clip(sample) => vec![ClipKey {
            track,
            clip: sample.clip,
        }],
        LayerContent::Transition { from, to, .. } => vec![
            ClipKey {
                track,
                clip: from.clip,
            },
            ClipKey {
                track,
                clip: to.clip,
            },
        ],
    }
}

/// Renders every frame of `project` (which must have passed [`crate::validate`]) into `sink`,
/// without audio. A project with an audio output is refused, because the sink would get an audio
/// stream with nothing in it; use [`render_with_audio`] for those.
pub fn render<C: Compositor, S: FrameSink>(
    project: &Project,
    compositor: &mut C,
    sink: &mut S,
) -> Result<RenderSummary, RenderError> {
    if project.output.audio.is_some() {
        return Err(RenderError(
            "the project has an audio output: render it with render_with_audio".to_owned(),
        ));
    }
    render_with_audio(project, compositor, sink, None)
}

/// Like [`render`], and after each frame also mixes audio up to the end of that frame and sends it
/// to the sink, so a muxer can interleave the streams. Audio is produced only when `audio` is given
/// and the project has an audio output.
pub fn render_with_audio<C: Compositor, S: FrameSink>(
    project: &Project,
    compositor: &mut C,
    sink: &mut S,
    audio: Option<&mut dyn AudioSource>,
) -> Result<RenderSummary, RenderError> {
    let mut audio = audio.filter(|_| project.output.audio.is_some());
    let mut mixed = 0i64;
    let mut clamped_samples = 0u64;
    let mut samples: Vec<f32> = Vec::new();
    let mut decoders: HashMap<ClipKey, C::Decoder> = HashMap::new();
    let mut rgba = Vec::new();
    let frames = frame_count(project);
    for index in 0..frames {
        let time = frame_time(project, index)
            .ok_or_else(|| RenderError(format!("frame {index}: time overflows")))?;
        let at = FrameContext { index, time };
        let layers = layers_at_frame(project, index);
        let mut kept = FrameStore(Vec::new());
        let mut canvas = None;
        let mut used = Vec::new();
        for layer in &layers {
            used.extend(layer_clips(&layer.content, layer.track));
            let image = render_layer(
                compositor,
                &mut decoders,
                &mut kept,
                layer.track,
                &layer.content,
                at,
            )
            .map_err(|e| RenderError(format!("frame {index}, track {}: {e}", layer.track)))?;
            canvas = Some(match canvas {
                None => image,
                Some(base) => {
                    let joined = compositor.composite(base, image, at);
                    compositor.release(base);
                    compositor.release(image);
                    joined.map_err(|e| {
                        RenderError(format!("frame {index}, track {}: {e}", layer.track))
                    })?
                }
            });
        }
        let image = match canvas {
            Some(image) => image,
            None => compositor.blank()?,
        };
        compositor
            .read(image, &mut rgba)
            .map_err(|e| RenderError(format!("frame {index}: {e}")))?;
        compositor.release(image);
        drop(kept);
        sink.write_frame(index, &rgba)
            .map_err(|e| RenderError(format!("frame {index}: {e}")))?;
        if let Some(source) = &mut audio {
            let end = frame_time(project, index + 1)
                .map(sample_position)
                .ok_or_else(|| RenderError(format!("frame {index}: time overflows")))?;
            samples.resize((end - mixed) as usize * MIX_CHANNELS, 0.0);
            let MixReport { clamped_samples: clamped } =
                mix_audio(project, &mut **source, mixed, &mut samples)
                    .map_err(|e| RenderError(format!("frame {index}, audio: {e}")))?;
            clamped_samples += clamped;
            sink.write_audio(&samples)
                .map_err(|e| RenderError(format!("frame {index}, audio: {e}")))?;
            mixed = end;
        }
        // A clip is contiguous on its track, so a clip unused in this frame is finished.
        decoders.retain(|key, _| used.contains(key));
    }
    Ok(RenderSummary {
        frames,
        audio_samples: mixed,
        clamped_samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Journal {
        events: Vec<String>,
        released: usize,
        acquired: usize,
    }

    struct FakeDecoder {
        name: String,
        journal: Rc<RefCell<Journal>>,
    }

    impl Decoder for FakeDecoder {
        type Frame = String;

        fn frame_at(&mut self, time: Time) -> Result<String, RenderError> {
            Ok(format!("{}@{}", self.name, time.ticks()))
        }

        fn wait_idle(&mut self) -> Result<(), RenderError> {
            self.journal
                .borrow_mut()
                .events
                .push(format!("idle {}", self.name));
            Ok(())
        }
    }

    struct FakeCompositor {
        journal: Rc<RefCell<Journal>>,
    }

    impl Compositor for FakeCompositor {
        type Decoder = FakeDecoder;
        type Image = usize;

        fn open_decoder(&mut self, source: &str) -> Result<FakeDecoder, RenderError> {
            self.journal
                .borrow_mut()
                .events
                .push(format!("open {source}"));
            Ok(FakeDecoder {
                name: source.to_owned(),
                journal: self.journal.clone(),
            })
        }

        fn prepare_clip(
            &mut self,
            key: ClipKey,
            frame: &mut String,
            _effects: &[Effect],
            at: FrameContext,
        ) -> Result<usize, RenderError> {
            let mut journal = self.journal.borrow_mut();
            journal.acquired += 1;
            journal.events.push(format!(
                "f{} clip {}:{} {frame}",
                at.index, key.track, key.clip
            ));
            Ok(journal.acquired)
        }

        fn transition(
            &mut self,
            package: &str,
            _params: &Map<String, Value>,
            _from: usize,
            _to: usize,
            progress: Progress,
            at: FrameContext,
        ) -> Result<usize, RenderError> {
            let mut journal = self.journal.borrow_mut();
            journal.acquired += 1;
            journal.events.push(format!(
                "f{} transition {package} {}",
                at.index,
                progress.parts_per_million()
            ));
            Ok(journal.acquired)
        }

        fn composite(
            &mut self,
            _base: usize,
            _top: usize,
            at: FrameContext,
        ) -> Result<usize, RenderError> {
            let mut journal = self.journal.borrow_mut();
            journal.acquired += 1;
            journal.events.push(format!("f{} composite", at.index));
            Ok(journal.acquired)
        }

        fn blank(&mut self) -> Result<usize, RenderError> {
            let mut journal = self.journal.borrow_mut();
            journal.acquired += 1;
            journal.events.push("blank".to_owned());
            Ok(journal.acquired)
        }

        fn read(&mut self, _image: usize, rgba: &mut Vec<u8>) -> Result<(), RenderError> {
            rgba.clear();
            Ok(())
        }

        fn release(&mut self, _image: usize) {
            self.journal.borrow_mut().released += 1;
        }
    }

    struct CountSink(i64);

    impl FrameSink for CountSink {
        fn write_frame(&mut self, index: i64, _rgba: &[u8]) -> Result<(), RenderError> {
            assert_eq!(index, self.0, "frames arrive in order");
            self.0 += 1;
            Ok(())
        }
    }

    fn run(json: &str) -> (Journal, RenderSummary) {
        let project = Project::from_json(json).expect("valid json");
        let journal = Rc::new(RefCell::new(Journal::default()));
        let mut compositor = FakeCompositor {
            journal: journal.clone(),
        };
        let mut sink = CountSink(0);
        let summary =
            render(&project, &mut compositor, &mut sink).expect("fake render never fails");
        drop(compositor);
        assert_eq!(sink.0, summary.frames);
        (
            Rc::try_unwrap(journal)
                .ok()
                .expect("no other owner")
                .into_inner(),
            summary,
        )
    }

    const TWO_TRACKS: &str = r#"{"version":1,"output":{"width":16,"height":16,"frame_duration":100},"tracks":[
        {"clips":[{"source":"a","start":0,"source_in":0,"duration":200}]},
        {"clips":[{"source":"b","start":100,"source_in":500,"duration":100}]}]}"#;

    const TRANSITION: &str = r#"{"version":1,"output":{"width":16,"height":16,"frame_duration":100},"tracks":[
        {"clips":[{"source":"a","start":0,"source_in":0,"duration":300},{"source":"b","start":300,"source_in":0,"duration":300}],
         "transitions":[{"after_clip":0,"package":"mix","duration":200}]}]}"#;

    const GAP: &str = r#"{"version":1,"output":{"width":16,"height":16,"frame_duration":100},"tracks":[
        {"clips":[{"source":"a","start":0,"source_in":0,"duration":100},{"source":"b","start":200,"source_in":0,"duration":100}]}]}"#;

    #[test]
    fn composites_tracks_bottom_up_and_balances_images() {
        let (journal, summary) = run(TWO_TRACKS);
        assert_eq!(summary.frames, 2);
        let frame_one: Vec<_> = journal
            .events
            .iter()
            .filter(|e| e.starts_with("f1 "))
            .cloned()
            .collect();
        assert_eq!(
            frame_one,
            ["f1 clip 0:0 a@100", "f1 clip 1:0 b@500", "f1 composite"]
        );
        assert_eq!(
            journal.acquired, journal.released,
            "every image comes back exactly once"
        );
    }

    #[test]
    fn waits_for_idle_before_preparing_each_layer() {
        let (journal, _) = run(TWO_TRACKS);
        let idle = journal
            .events
            .iter()
            .position(|e| e == "idle b")
            .expect("b idle wait");
        let prepared = journal
            .events
            .iter()
            .position(|e| e == "f1 clip 1:0 b@500")
            .expect("b prepared");
        assert!(idle < prepared, "{:?}", journal.events);
    }

    #[test]
    fn runs_transitions_and_keeps_two_decoders_inside_the_window() {
        let (journal, _) = run(TRANSITION);
        // The window is [200, 400); frames 2 and 3 are inside it.
        assert!(
            journal.events.contains(&"f2 transition mix 0".to_owned()),
            "{:?}",
            journal.events
        );
        assert!(
            journal
                .events
                .contains(&"f3 transition mix 500000".to_owned()),
            "{:?}",
            journal.events
        );
        assert_eq!(
            journal.events.iter().filter(|e| *e == "open a").count(),
            1,
            "one decoder per clip"
        );
        assert_eq!(
            journal.events.iter().filter(|e| *e == "open b").count(),
            1,
            "one decoder per clip"
        );
        assert_eq!(journal.acquired, journal.released);
    }

    #[test]
    fn gaps_give_blank_frames_and_finished_decoders_are_dropped() {
        let (journal, summary) = run(GAP);
        assert_eq!(summary.frames, 3);
        assert_eq!(journal.events.iter().filter(|e| *e == "blank").count(), 1);
        assert_eq!(journal.acquired, journal.released);
        let open_a = journal.events.iter().filter(|e| *e == "open a").count();
        assert_eq!(open_a, 1);
    }

    /// Samples equal to their own source position.
    struct CountingSource;

    impl AudioSource for CountingSource {
        fn read(
            &mut self,
            _key: ClipKey,
            _source: &str,
            position: i64,
            out: &mut [f32],
        ) -> Result<(), RenderError> {
            for (i, frame) in out.chunks_exact_mut(2).enumerate() {
                let value = (position + i as i64) as f32 * 1e-4;
                frame[0] = value;
                frame[1] = value;
            }
            Ok(())
        }
    }

    struct FailingSource;

    impl AudioSource for FailingSource {
        fn read(
            &mut self,
            _key: ClipKey,
            _source: &str,
            _position: i64,
            _out: &mut [f32],
        ) -> Result<(), RenderError> {
            Err(RenderError("broken audio".to_owned()))
        }
    }

    #[derive(Default)]
    struct AudioSink {
        events: Vec<String>,
        samples: Vec<f32>,
    }

    impl FrameSink for AudioSink {
        fn write_frame(&mut self, index: i64, _rgba: &[u8]) -> Result<(), RenderError> {
            self.events.push(format!("frame {index}"));
            Ok(())
        }

        fn write_audio(&mut self, samples: &[f32]) -> Result<(), RenderError> {
            self.events.push(format!("audio {}", samples.len() / 2));
            self.samples.extend_from_slice(samples);
            Ok(())
        }
    }

    const WITH_AUDIO: &str = r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":100,"audio":{"sample_rate":48000,"channels":2,"bitrate":128000}},"tracks":[
        {"clips":[{"source":"a","start":0,"source_in":0,"duration":300}]}]}"#;

    fn run_audio(json: &str, source: &mut dyn AudioSource) -> Result<(AudioSink, RenderSummary), RenderError> {
        let project = Project::from_json(json).expect("valid json");
        let journal = Rc::new(RefCell::new(Journal::default()));
        let mut compositor = FakeCompositor { journal };
        let mut sink = AudioSink::default();
        let summary = render_with_audio(&project, &mut compositor, &mut sink, Some(source))?;
        Ok((sink, summary))
    }

    #[test]
    fn each_frame_is_followed_by_the_audio_up_to_its_end() {
        let (sink, summary) = run_audio(WITH_AUDIO, &mut CountingSource).expect("render");
        // 100 ticks is 53.33 samples: the blocks round so that they add up exactly.
        assert_eq!(
            sink.events,
            ["frame 0", "audio 53", "frame 1", "audio 53", "frame 2", "audio 54"]
        );
        assert_eq!(summary.audio_samples, 160);
        assert_eq!(sink.samples.len(), 320);
        for (i, frame) in sink.samples.chunks_exact(2).enumerate() {
            assert_eq!(frame[0], i as f32 * 1e-4, "no gap or overlap at sample {i}");
        }
    }

    #[test]
    fn a_project_without_audio_output_sends_no_audio() {
        let json = WITH_AUDIO.replace(
            r#","audio":{"sample_rate":48000,"channels":2,"bitrate":128000}"#,
            "",
        );
        let (sink, summary) = run_audio(&json, &mut FailingSource).expect("the source is not read");
        assert_eq!(sink.events, ["frame 0", "frame 1", "frame 2"]);
        assert_eq!(summary.audio_samples, 0);
    }

    #[test]
    fn render_without_a_source_refuses_a_project_with_audio_output() {
        let project = Project::from_json(WITH_AUDIO).expect("valid json");
        let journal = Rc::new(RefCell::new(Journal::default()));
        let mut compositor = FakeCompositor { journal: journal.clone() };
        let mut sink = AudioSink::default();
        let error = render(&project, &mut compositor, &mut sink)
            .err()
            .expect("must be refused");
        assert!(error.0.contains("render_with_audio"), "{}", error.0);
        assert!(sink.events.is_empty(), "nothing is rendered: {:?}", sink.events);
        assert!(journal.borrow().events.is_empty(), "no decoder is opened");
    }

    #[test]
    fn an_audio_failure_fails_the_render() {
        let error = run_audio(WITH_AUDIO, &mut FailingSource).err().expect("must fail");
        assert!(error.0.contains("audio"), "{}", error.0);
        assert!(error.0.contains("broken audio"), "{}", error.0);
    }

    #[test]
    fn empty_project_renders_nothing() {
        let (journal, summary) = run(
            r#"{"version":1,"output":{"width":16,"height":16,"frame_duration":100},"tracks":[]}"#,
        );
        assert_eq!(summary.frames, 0);
        assert!(journal.events.is_empty());
    }
}
