//! `gpufx-render --effect-previews`: a long-lived helper that renders effect previews on request.
//!
//! The parent writes one JSON job per stdin line and reads one JSON event per stdout line. The lines are the
//! types of `d3d12_backend::{PreviewJob, PreviewEvent}`, and none is longer than `MAX_EVENT_LINE`:
//!
//! ```text
//! in : {"id":1,"kind":"still"|"loop","package":"<package dir>","out":"<file path>"}
//! out: {"event":"ready","protocol":1,"device":"<description>"}    once the renderer exists
//!      {"event":"done","id":1,"static":false}                     a job went well
//!      {"event":"failed","id":1,"blame":"device"|"package"|"output","message":"..."}
//!      {"event":"invalid","id":1,"message":"..."}                 a line that is not a job; `id` only if readable
//!      {"event":"fatal","message":"..."}                          no renderer; the exit code is non-zero
//! ```
//!
//! The parent picks `out` and publishes the file itself, so the helper writes that exact path and never touches
//! the preview cache. `out` must end in `.tmp` and sit directly in the preview store's folder: the store's sweep
//! removes a `.tmp` file there once it is an hour old, which cleans what a crash leaves. A job that fails while
//! writing removes its `out`. The parent must send paths with `Path::to_str` and treat `None` as not previewable,
//! never converting lossily, since a lossy path names another file.
//!
//! A line that is not valid UTF-8, or longer than `MAX_EVENT_LINE`, is answered with `invalid` without being
//! parsed, and the helper goes on with the next line. The end of stdin ends the helper with exit code 0. Stdin
//! is only read between jobs, so a parent that died stops the helper after the current job, not during it; a job
//! that hangs is ended by the parent's job object.
//!
//! A still is a `PREVIEW_SIZE` square PNG. A loop is one PNG holding its frames stacked top to bottom, each
//! `PREVIEW_SIZE` tall (the strip `PreviewStore` expects: as wide as a still, a whole number of stills tall);
//! a static loop is a strip of one frame, and `static` is true.

use std::io::{BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

use d3d12_backend::{PreviewBlame, PreviewError, PreviewEvent, PreviewJob, PreviewJobKind, PreviewLoop, PreviewRenderer, MAX_EVENT_LINE, PREVIEW_PROTOCOL, PREVIEW_SIZE};

/// What the protocol needs from a renderer, so it can be tested without a GPU.
pub trait Renderer {
    fn description(&self) -> String;
    fn still(&mut self, package: &Path) -> Result<Vec<u8>, PreviewError>;
    fn animation(&mut self, package: &Path) -> Result<PreviewLoop, PreviewError>;
}

impl Renderer for PreviewRenderer {
    fn description(&self) -> String {
        self.device_description().unwrap_or_default()
    }
    fn still(&mut self, package: &Path) -> Result<Vec<u8>, PreviewError> {
        self.render_still(package)
    }
    fn animation(&mut self, package: &Path) -> Result<PreviewLoop, PreviewError> {
        self.render_loop(package)
    }
}

/// Encodes RGBA8 pixels as a PNG.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Whether the finished job is a static loop, or why it failed as a (blame, message) pair.
fn render_job(renderer: &mut impl Renderer, job: &PreviewJob) -> Result<bool, (PreviewBlame, String)> {
    let from_preview = |error: PreviewError| match error {
        PreviewError::Device(message) => (PreviewBlame::Device, message),
        PreviewError::Package(message) => (PreviewBlame::Package, message),
    };
    // Encoding and writing are not the package's fault, nor the device's.
    let output = |message: String| (PreviewBlame::Output, message);
    let package = Path::new(&job.package);
    let (rgba, frames, is_static) = match job.kind {
        PreviewJobKind::Still => (renderer.still(package).map_err(from_preview)?, 1u32, false),
        PreviewJobKind::Loop => {
            let preview = renderer.animation(package).map_err(from_preview)?;
            (preview.frames.concat(), preview.frames.len() as u32, preview.is_static)
        }
    };
    let png = encode_png(PREVIEW_SIZE, PREVIEW_SIZE * frames, &rgba).map_err(|e| output(format!("encoding the preview failed: {e}")))?;
    if let Err(e) = std::fs::write(&job.out, png) {
        // A write that failed halfway leaves part of a file; do not let the parent mistake it for a preview.
        let _ = std::fs::remove_file(&job.out);
        return Err(output(format!("writing {}: {e}", job.out)));
    }
    Ok(is_static)
}

fn emit(writer: &mut impl Write, event: &PreviewEvent) -> std::io::Result<()> {
    let mut line = event.to_line();
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// One line read by [`read_line`].
#[derive(Debug, PartialEq, Eq)]
enum Line {
    End,
    /// In the buffer, without its line end.
    Read,
    /// Longer than `MAX_EVENT_LINE`; skipped to its end, and only a bounded part of it was kept in memory.
    TooLong,
}

/// Reads up to the next `\n` into `buffer`, never holding more than `MAX_EVENT_LINE + 1` bytes of it.
fn read_line(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> std::io::Result<Line> {
    buffer.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            // The end of input ends a last line that has no line end.
            return Ok(if !any {
                Line::End
            } else if too_long {
                Line::TooLong
            } else {
                Line::Read
            });
        }
        any = true;
        let newline = chunk.iter().position(|&byte| byte == b'\n');
        let part = &chunk[..newline.unwrap_or(chunk.len())];
        if !too_long {
            // One byte more than the limit is room for a `\r` that is not part of the line.
            if buffer.len() + part.len() > MAX_EVENT_LINE + 1 {
                too_long = true;
                buffer.clear();
            } else {
                buffer.extend_from_slice(part);
            }
        }
        let used = newline.map_or(part.len(), |at| at + 1);
        reader.consume(used);
        if newline.is_some() {
            if buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
            return Ok(if too_long || buffer.len() > MAX_EVENT_LINE { Line::TooLong } else { Line::Read });
        }
    }
}

/// Serves jobs until `reader` ends or fails. An error is only a writer that went away.
pub fn serve(mut reader: impl BufRead, writer: &mut impl Write, renderer: &mut impl Renderer) -> std::io::Result<()> {
    emit(writer, &PreviewEvent::Ready { protocol: PREVIEW_PROTOCOL, device: renderer.description() })?;
    let mut buffer = Vec::new();
    loop {
        // A read that fails (a broken pipe) cannot be answered, so it ends the helper like the end of input.
        let Ok(line) = read_line(&mut reader, &mut buffer) else { break };
        let event = match line {
            Line::End => break,
            Line::TooLong => PreviewEvent::Invalid { id: None, message: format!("the line is longer than {MAX_EVENT_LINE} bytes") },
            Line::Read => {
                let Ok(text) = std::str::from_utf8(&buffer) else {
                    emit(writer, &PreviewEvent::Invalid { id: None, message: "the line is not UTF-8".to_owned() })?;
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                match PreviewJob::parse(text) {
                    Err(invalid) => PreviewEvent::Invalid { id: invalid.id, message: invalid.message },
                    Ok(job) => match render_job(renderer, &job) {
                        Ok(is_static) => PreviewEvent::Done { id: job.id, is_static },
                        Err((blame, message)) => PreviewEvent::Failed { id: job.id, blame, message },
                    },
                }
            }
        };
        emit(writer, &event)?;
    }
    Ok(())
}

/// Lets the editor and the export have the CPU first. This is the CPU priority only; the GPU is shared by the
/// operating system's scheduler whatever this says.
#[cfg(windows)]
fn lower_priority() {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS};
    // Losing this only makes the helper less polite.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
}

#[cfg(not(windows))]
fn lower_priority() {}

/// Stdout that takes its lock for each write, so nothing holds it between events.
struct Stdout;

impl Write for Stdout {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        std::io::stdout().lock().write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().lock().flush()
    }
}

/// The mode, on the real stdin and stdout.
pub fn run_stdio() -> ExitCode {
    lower_priority();
    let mut renderer = match PreviewRenderer::new() {
        Ok(renderer) => renderer,
        Err(error) => {
            let _ = emit(&mut Stdout, &PreviewEvent::Fatal { message: error.to_string() });
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A stdout that went away means the parent did; there is nobody left to serve.
    let _ = serve(std::io::stdin().lock(), &mut Stdout, &mut renderer);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::Cursor;
    use std::path::PathBuf;

    struct Fake {
        calls: Vec<(PreviewJobKind, PathBuf)>,
    }

    fn frame(value: u8) -> Vec<u8> {
        vec![value; (PREVIEW_SIZE * PREVIEW_SIZE * 4) as usize]
    }

    impl Renderer for Fake {
        fn description(&self) -> String {
            "Fake GPU".to_owned()
        }
        fn still(&mut self, package: &Path) -> Result<Vec<u8>, PreviewError> {
            self.calls.push((PreviewJobKind::Still, package.to_owned()));
            fail_by_name(package)?;
            Ok(frame(7))
        }
        fn animation(&mut self, package: &Path) -> Result<PreviewLoop, PreviewError> {
            self.calls.push((PreviewJobKind::Loop, package.to_owned()));
            fail_by_name(package)?;
            if package.ends_with("static") {
                return Ok(PreviewLoop { frames: vec![frame(1)], is_static: true });
            }
            Ok(PreviewLoop { frames: vec![frame(1), frame(2), frame(3)], is_static: false })
        }
    }

    fn fail_by_name(package: &Path) -> Result<(), PreviewError> {
        if package.ends_with("broken") {
            return Err(PreviewError::Package("bad shader".to_owned()));
        }
        if package.ends_with("wordy") {
            return Err(PreviewError::Package("{1}".repeat(MAX_EVENT_LINE)));
        }
        if package.ends_with("lost") {
            return Err(PreviewError::Device("device lost".to_owned()));
        }
        Ok(())
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-effect-previews-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn job_line(id: u64, kind: &str, package: &str, out: &Path) -> String {
        json!({"id": id, "kind": kind, "package": package, "out": out.to_string_lossy()}).to_string()
    }

    fn run(input: &str) -> (Vec<Value>, Fake) {
        run_bytes(input.as_bytes())
    }

    fn run_bytes(input: &[u8]) -> (Vec<Value>, Fake) {
        let mut fake = Fake { calls: Vec::new() };
        let mut output = Vec::new();
        serve(Cursor::new(input.to_vec()), &mut output, &mut fake).expect("serves");
        let events = String::from_utf8(output).expect("utf-8").lines().map(|line| serde_json::from_str(line).expect("one JSON object per line")).collect();
        (events, fake)
    }

    fn png_size(path: &Path) -> (u32, u32) {
        let reader = png::Decoder::new(Cursor::new(std::fs::read(path).expect("file"))).read_info().expect("a PNG");
        let info = reader.info();
        assert_eq!((info.color_type, info.bit_depth), (png::ColorType::Rgba, png::BitDepth::Eight));
        (info.width, info.height)
    }

    fn pixels(path: &Path) -> Vec<u8> {
        let mut reader = png::Decoder::new(Cursor::new(std::fs::read(path).expect("file"))).read_info().expect("a PNG");
        let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
        reader.next_frame(&mut buffer).expect("frame");
        buffer
    }

    #[test]
    fn ready_comes_first_and_the_end_of_input_ends_quietly() {
        let (events, fake) = run("");
        assert_eq!(events, [json!({"event": "ready", "protocol": PREVIEW_PROTOCOL, "device": "Fake GPU"})]);
        assert!(fake.calls.is_empty());
    }

    #[test]
    fn a_still_is_one_square_png() {
        let dir = scratch("still");
        let out = dir.join("a.png");
        let (events, fake) = run(&format!("{}\n", job_line(5, "still", "pkg/invert", &out)));
        assert_eq!(events[1], json!({"event": "done", "id": 5, "static": false}));
        assert_eq!(fake.calls, [(PreviewJobKind::Still, PathBuf::from("pkg/invert"))]);
        assert_eq!(png_size(&out), (PREVIEW_SIZE, PREVIEW_SIZE));
    }

    #[test]
    fn a_loop_is_its_frames_stacked_top_to_bottom() {
        let dir = scratch("loop");
        let out = dir.join("a.png");
        let (events, _) = run(&format!("{}\n", job_line(1, "loop", "pkg/spin", &out)));
        assert_eq!(events[1], json!({"event": "done", "id": 1, "static": false}));
        assert_eq!(png_size(&out), (PREVIEW_SIZE, PREVIEW_SIZE * 3));
        let data = pixels(&out);
        let frame_bytes = (PREVIEW_SIZE * PREVIEW_SIZE * 4) as usize;
        assert_eq!([data[0], data[frame_bytes], data[2 * frame_bytes]], [1, 2, 3]);
    }

    #[test]
    fn a_static_loop_writes_its_one_frame_and_says_so() {
        let dir = scratch("static");
        let out = dir.join("a.png");
        let (events, _) = run(&format!("{}\n", job_line(2, "loop", "pkg/static", &out)));
        assert_eq!(events[1], json!({"event": "done", "id": 2, "static": true}));
        assert_eq!(png_size(&out), (PREVIEW_SIZE, PREVIEW_SIZE));
    }

    #[test]
    fn a_malformed_line_is_reported_and_the_next_job_still_runs() {
        let dir = scratch("malformed");
        let out = dir.join("a.png");
        let input = format!("not json\n{{\"id\":1,\"kind\":\"zoom\",\"package\":\"p\",\"out\":\"o\"}}\n{{\"id\":-1}}\n\n{}\n", job_line(9, "still", "pkg/invert", &out));
        let (events, fake) = run(&input);
        let names: Vec<&str> = events.iter().map(|event| event["event"].as_str().expect("event")).collect();
        assert_eq!(names, ["ready", "invalid", "invalid", "invalid", "done"]);
        assert!(events[1]["message"].as_str().expect("message").contains("JSON"));
        assert!(events[2]["message"].as_str().expect("message").contains("zoom"));
        assert_eq!(events[1].get("id"), None);
        assert_eq!(events[2]["id"], 1, "an unknown kind still has a readable id");
        assert_eq!(events[3].get("id"), None);
        assert_eq!(events[4]["id"], 9);
        assert_eq!(fake.calls.len(), 1);
        assert!(out.is_file());
    }

    #[test]
    fn each_blame_is_passed_on() {
        let dir = scratch("blame");
        let out = dir.join("a.png");
        let input = format!("{}\n{}\n{}\n", job_line(1, "still", "pkg/broken", &out), job_line(2, "loop", "pkg/lost", &out), job_line(3, "still", "pkg/invert", &out));
        let (events, _) = run(&input);
        assert_eq!(events[1], json!({"event": "failed", "id": 1, "blame": "package", "message": "bad shader"}));
        assert_eq!(events[2], json!({"event": "failed", "id": 2, "blame": "device", "message": "device lost"}));
        assert_eq!(events[3]["event"], "done", "a failure does not stop the helper");
    }

    #[test]
    fn a_file_that_cannot_be_written_is_blamed_on_the_output() {
        let dir = scratch("unwritable");
        let out = dir.join("missing-folder").join("a.png");
        let (events, _) = run(&format!("{}\n", job_line(4, "still", "pkg/invert", &out)));
        assert_eq!(events[1]["event"], "failed");
        assert_eq!(events[1]["id"], 4);
        assert_eq!(events[1]["blame"], "output");
        assert!(events[1]["message"].as_str().expect("message").contains("a.png"));
    }

    #[test]
    fn a_writer_that_went_away_ends_the_loop() {
        /// Takes the first write (the `ready` line), then fails like a closed pipe.
        struct GoesAway {
            writes: usize,
        }
        impl Write for GoesAway {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                if self.writes > 1 {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dir = scratch("went-away");
        let input = format!("{}\n{}\n", job_line(1, "still", "pkg/first", &dir.join("a.png")), job_line(2, "still", "pkg/second", &dir.join("b.png")));
        let mut fake = Fake { calls: Vec::new() };
        assert!(serve(Cursor::new(input), &mut GoesAway { writes: 0 }, &mut fake).is_err());
        assert_eq!(fake.calls, [(PreviewJobKind::Still, PathBuf::from("pkg/first"))], "the job whose answer failed ran; the next one did not");
    }

    #[test]
    fn a_line_that_is_not_utf8_is_invalid_and_the_next_job_still_runs() {
        let dir = scratch("not-utf8");
        let (first, second) = (dir.join("a.png"), dir.join("b.png"));
        let mut input = job_line(1, "still", "pkg/invert", &first).into_bytes();
        input.extend_from_slice(b"\n{\"id\":2,\"package\":\"\xff\xfe\"}\n");
        input.extend_from_slice(job_line(3, "still", "pkg/invert", &second).as_bytes());
        input.push(b'\n');
        let (events, fake) = run_bytes(&input);
        let names: Vec<&str> = events.iter().map(|event| event["event"].as_str().expect("event")).collect();
        assert_eq!(names, ["ready", "done", "invalid", "done"]);
        assert_eq!(events[2].get("id"), None, "a line that is not text has no readable id");
        assert!(events[2]["message"].as_str().expect("message").contains("UTF-8"));
        assert_eq!(fake.calls.len(), 2);
        assert!(first.is_file() && second.is_file());
    }

    #[test]
    fn a_line_that_is_too_long_is_invalid_without_being_parsed_and_the_next_job_still_runs() {
        let dir = scratch("too-long");
        let (first, second) = (dir.join("a.png"), dir.join("b.png"));
        // Starts like a valid job; only its length gives it away.
        let long = format!("{{\"id\":1,\"kind\":\"still\",\"package\":\"{}\",\"out\":\"x\"}}", "p".repeat(MAX_EVENT_LINE));
        let input = format!("{long}\n{}\n{long}", job_line(2, "still", "pkg/invert", &first));
        let (events, fake) = run(&input);
        let names: Vec<&str> = events.iter().map(|event| event["event"].as_str().expect("event")).collect();
        assert_eq!(names, ["ready", "invalid", "done", "invalid"], "the last line has no line end and is too long as well");
        assert_eq!(events[1].get("id"), None);
        assert!(events[1]["message"].as_str().expect("message").contains("longer"));
        assert_eq!(fake.calls.len(), 1, "a line that was too long is not run");
        assert!(first.is_file());

        // The limit itself is a line that fits, with a `\r` or without, and a job after it runs.
        let at_limit = format!("{}{}", job_line(3, "still", "pkg/invert", &second), " ".repeat(MAX_EVENT_LINE - job_line(3, "still", "pkg/invert", &second).len()));
        assert_eq!(at_limit.len(), MAX_EVENT_LINE);
        let (events, _) = run(&format!("{at_limit}\r\n{at_limit}\n"));
        let names: Vec<&str> = events.iter().map(|event| event["event"].as_str().expect("event")).collect();
        assert_eq!(names, ["ready", "done", "done"]);
    }

    #[test]
    fn a_long_line_is_skipped_in_bounded_memory_whatever_the_read_size() {
        let mut input = vec![b'a'; MAX_EVENT_LINE * 40];
        input.extend_from_slice(b"\nnext\n");
        let mut reader = std::io::BufReader::with_capacity(4096, Cursor::new(input));
        let mut buffer = Vec::new();
        assert_eq!(read_line(&mut reader, &mut buffer).expect("reads"), Line::TooLong);
        assert!(buffer.capacity() <= MAX_EVENT_LINE + 1 + 4096, "{} bytes kept", buffer.capacity());
        assert_eq!(read_line(&mut reader, &mut buffer).expect("reads"), Line::Read);
        assert_eq!(buffer, b"next");
        assert_eq!(read_line(&mut reader, &mut buffer).expect("reads"), Line::End);
    }

    #[test]
    fn a_message_too_long_for_a_line_is_cut_to_fit() {
        // Every character of it takes six bytes once it is escaped.
        let dir = scratch("wordy");
        let mut fake = Fake { calls: Vec::new() };
        let mut output = Vec::new();
        serve(Cursor::new(format!("{}\n", job_line(1, "still", "pkg/wordy", &dir.join("a.png")))), &mut output, &mut fake).expect("serves");
        let lines: Vec<&[u8]> = output.split(|&byte| byte == b'\n').filter(|line| !line.is_empty()).collect();
        assert!(lines.iter().all(|line| line.len() <= MAX_EVENT_LINE));
        let failed: Value = serde_json::from_slice(lines[1]).expect("JSON");
        assert_eq!((failed["event"].as_str(), failed["blame"].as_str()), (Some("failed"), Some("package")));
        assert!(failed["message"].as_str().expect("message").ends_with("..."));
    }
}
