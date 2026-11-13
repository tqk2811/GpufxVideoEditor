//! The preview frame producer on the D3D12 backend. This is the only place that runs compositing
//! work in the editor.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use d3d12_backend::{is_cancelled, D3d12Compositor, Gpu, ProjectPaths, SharedImage};
use engine::render::{Compositor, FrameRenderer, RenderError};
use engine::{layers_at_frame, Project};

use super::plan::RequestId;
use super::worker::{FrameProducer, ProduceError};

const RECYCLE_WAIT: Duration = Duration::from_secs(1);
/// How long a decode runs before a newer request can stop it. Short decodes finish, so a continuous
/// scrub keeps showing frames; a long catch-up is stopped soon after it is no longer wanted, unless
/// no frame was published lately (the decoder checks), so a slow source still shows pictures.
const ABORT_GRACE: Duration = Duration::from_millis(150);

pub struct PreviewRequest {
    pub revision: u64,
    pub project: Arc<Project>,
    pub paths: Arc<ProjectPaths>,
    pub frame: i64,
}

/// A published frame and the last UI submission that drew it.
pub struct PreviewImage {
    pub image: SharedImage,
    pub last_use: Option<wgpu::SubmissionIndex>,
}

struct Sized {
    width: u32,
    height: u32,
    compositor: D3d12Compositor,
}

pub struct D3d12Producer {
    gpu: Arc<Gpu>,
    paths: Option<Arc<ProjectPaths>>,
    compositor: Option<Sized>,
    renderer: FrameRenderer<D3d12Compositor>,
    abort: Option<Arc<AtomicBool>>,
    closing: Option<Arc<AtomicBool>>,
}

/// Sorts a failed frame's message into a real failure or a cancelled decode.
fn classify(message: String) -> ProduceError {
    if is_cancelled(&message) {
        ProduceError::Cancelled
    } else {
        ProduceError::Failed(message)
    }
}

impl D3d12Producer {
    pub fn new(gpu: Arc<Gpu>) -> Self {
        Self { gpu, paths: None, compositor: None, renderer: FrameRenderer::default(), abort: None, closing: None }
    }

    /// A compositor and decoders resolved files through the old paths, so they go with them.
    fn use_paths(&mut self, paths: &Arc<ProjectPaths>) {
        if !self.paths.as_ref().is_some_and(|current| Arc::ptr_eq(current, paths)) {
            self.compositor = None;
            self.renderer = FrameRenderer::default();
            self.paths = Some(paths.clone());
        }
    }

    /// Renders frame `frame` of `project` as RGBA for an agent. The decoders are its own, so the frames
    /// the preview is on stay where they are, and nothing is published to the screen.
    pub fn capture(&mut self, project: &Project, paths: &Arc<ProjectPaths>, frame: i64) -> Result<Vec<u8>, String> {
        if let Some(message) = self.gpu.device_lost() {
            return Err(format!("the GPU device was lost: {message}"));
        }
        self.use_paths(paths);
        let (width, height) = (project.output.width, project.output.height);
        let compositor = self.compositor_for(width, height)?;
        let mut rgba = Vec::new();
        let mut renderer = FrameRenderer::default();
        let rendered = compositor.without_abort(|compositor| renderer.render_frame(project, compositor, frame, |compositor, image| compositor.read(image, &mut rgba)));
        // A failed frame must not be blamed on the next one.
        self.gpu.take_error();
        rendered.map_err(|e| e.0)?;
        Ok(rgba)
    }

    /// Frees the images and pooled textures kept for the next frames, once playback stopped. The
    /// graphs stay, so the next frame needs no shader built again.
    pub fn trim(&mut self) {
        match &mut self.compositor {
            Some(sized) => sized.compositor.trim(),
            None => self.gpu.context.trim_pool(),
        }
    }

    fn compositor_for(&mut self, width: u32, height: u32) -> Result<&mut D3d12Compositor, String> {
        let stale = self.compositor.as_ref().is_some_and(|sized| sized.width != width || sized.height != height);
        if stale {
            self.compositor = None;
        }
        if self.compositor.is_none() {
            let mut compositor = D3d12Compositor::new(self.gpu.clone(), self.paths.clone().ok_or("no project paths yet")?, width, height)?;
            if let Some(abort) = &self.abort {
                compositor.set_abort(abort.clone(), ABORT_GRACE);
            }
            if let Some(closing) = &self.closing {
                compositor.set_closing(closing.clone());
            }
            self.compositor = Some(Sized { width, height, compositor });
        }
        Ok(&mut self.compositor.as_mut().expect("just created").compositor)
    }
}

impl FrameProducer for D3d12Producer {
    type Request = PreviewRequest;
    type Frame = PreviewImage;

    fn request_id(request: &PreviewRequest) -> RequestId {
        RequestId { revision: request.revision, frame: request.frame }
    }

    fn produce(&mut self, request: &PreviewRequest) -> Result<Option<PreviewImage>, ProduceError> {
        if let Some(message) = self.gpu.device_lost() {
            return Err(ProduceError::Failed(format!("the GPU device was lost: {message}")));
        }
        // No compositor needed, so a machine without effect packages still shows an empty timeline.
        if layers_at_frame(&request.project, request.frame).is_empty() {
            return Ok(None);
        }
        self.use_paths(&request.paths);
        let output = &request.project.output;
        let (width, height) = (output.width, output.height);
        let mut renderer = std::mem::take(&mut self.renderer);
        let compositor = self.compositor_for(width, height);
        let result = compositor.and_then(|compositor| {
            renderer
                .render_frame(&request.project, compositor, request.frame, |compositor, image| compositor.publish(image).map_err(RenderError))
                .map_err(|e| e.0)
        });
        self.renderer = renderer;
        match result {
            Ok(image) => Ok(image.map(|image| PreviewImage { image, last_use: None })),
            Err(message) => match classify(message) {
                ProduceError::Cancelled => {
                    // A layer that ran before the cancel may have left a wgpu error; it must not be blamed on the next frame.
                    self.gpu.take_error();
                    Err(ProduceError::Cancelled)
                }
                failed => {
                    self.gpu.take_error();
                    Err(failed)
                }
            },
        }
    }

    fn recycle(&mut self, frame: PreviewImage) {
        if let Some(index) = frame.last_use {
            let waited = self.gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(RECYCLE_WAIT) });
            if waited.is_err() {
                return;
            }
        }
        if let Some(sized) = &mut self.compositor {
            sized.compositor.recycle(frame.image);
        }
    }

    fn reset(&mut self) {
        self.compositor = None;
        self.renderer = FrameRenderer::default();
    }

    fn set_abort(&mut self, abort: Arc<AtomicBool>) {
        self.abort = Some(abort);
    }

    fn set_closing(&mut self, closing: Arc<AtomicBool>) {
        self.closing = Some(closing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_decode_is_not_a_failure() {
        assert_eq!(classify("frame 3, track 0: c:/a.mp4: decoding was cancelled".to_owned()), ProduceError::Cancelled);
    }

    #[test]
    fn other_messages_stay_failures() {
        let message = "frame 3: avcodec_receive_frame failed".to_owned();
        assert_eq!(classify(message.clone()), ProduceError::Failed(message));
    }
}
