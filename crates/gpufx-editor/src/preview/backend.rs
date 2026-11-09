//! The preview frame producer on the D3D12 backend. This is the only place that runs compositing
//! work in the editor.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use d3d12_backend::{D3d12Compositor, Gpu, ProjectPaths, SharedImage};
use engine::render::{FrameRenderer, RenderError};
use engine::{layers_at_frame, Project};

use super::worker::FrameProducer;

/// The checkout's effect packages, as `gpufx-render` uses by default.
const PACKAGE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter");
/// A project has no file yet, and clips added from a dialog hold absolute paths, which
/// [`ProjectPaths`] keeps as they are.
const UNSAVED_PROJECT_FILE: &str = "unsaved.json";
const RECYCLE_WAIT: Duration = Duration::from_secs(1);

pub struct PreviewRequest {
    pub project: Arc<Project>,
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
    paths: Arc<ProjectPaths>,
    compositor: Option<Sized>,
    renderer: FrameRenderer<D3d12Compositor>,
}

impl D3d12Producer {
    pub fn new(gpu: Arc<Gpu>) -> Self {
        let paths = Arc::new(ProjectPaths::new(Path::new(UNSAVED_PROJECT_FILE), vec![PathBuf::from(PACKAGE_ROOT)]));
        Self { gpu, paths, compositor: None, renderer: FrameRenderer::default() }
    }

    fn compositor_for(&mut self, width: u32, height: u32) -> Result<&mut D3d12Compositor, String> {
        let stale = self.compositor.as_ref().is_some_and(|sized| sized.width != width || sized.height != height);
        if stale {
            self.compositor = None;
        }
        if self.compositor.is_none() {
            let compositor = D3d12Compositor::new(self.gpu.clone(), self.paths.clone(), width, height)?;
            self.compositor = Some(Sized { width, height, compositor });
        }
        Ok(&mut self.compositor.as_mut().expect("just created").compositor)
    }
}

impl FrameProducer for D3d12Producer {
    type Request = PreviewRequest;
    type Frame = PreviewImage;

    fn produce(&mut self, request: &PreviewRequest) -> Result<Option<PreviewImage>, String> {
        if let Some(message) = self.gpu.device_lost() {
            return Err(format!("the GPU device was lost: {message}"));
        }
        // No compositor needed, so a machine without effect packages still shows an empty timeline.
        if layers_at_frame(&request.project, request.frame).is_empty() {
            return Ok(None);
        }
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
            Err(message) => {
                self.gpu.take_error();
                Err(message)
            }
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
}
