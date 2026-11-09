//! Runs the real `gpufx-mcp` binary over stdio as an MCP client would.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use base64::Engine as _;
use common::*;
use d3d12_backend::Gpu;
use serde_json::{json, Value};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gpufx-mcp")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().expect("start gpufx-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self { child, stdin, stdout, next_id: 1 };
        client.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
        client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        client
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the server closed stdout");
            let message: Value = serde_json::from_str(&line).expect("one JSON message per line");
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "{method}: {message}");
                return message["result"].clone();
            }
        }
    }

    /// Calls a tool and returns its content blocks and whether it reported an error.
    fn tool(&mut self, name: &str, arguments: Value) -> (Vec<Value>, bool) {
        let result = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        (result["content"].as_array().expect("content").clone(), result["isError"] == json!(true))
    }

    fn ok(&mut self, name: &str, arguments: Value) -> (Value, Vec<Value>) {
        let (content, error) = self.tool(name, arguments.clone());
        let body: Value = serde_json::from_str(content[0]["text"].as_str().expect("text first")).expect("JSON text");
        assert!(!error, "{name} {arguments}: {body}");
        (body, content)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn decode_png(block: &Value) -> (u32, u32, Vec<u8>) {
    assert_eq!(block["type"], "image");
    assert_eq!(block["mimeType"], "image/png");
    let bytes = base64::engine::general_purpose::STANDARD.decode(block["data"].as_str().unwrap()).expect("base64");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a PNG signature");
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().expect("a real PNG");
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).expect("PNG data");
    pixels.truncate(info.buffer_size());
    (info.width, info.height, pixels)
}

fn gpu_available() -> bool {
    if matches!(Gpu::create_hardware(), Ok(Some(_))) {
        return true;
    }
    assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "no GPU");
    eprintln!("skipping: no D3D12 hardware adapter is available");
    false
}

#[test]
fn opens_a_project_edits_it_and_captures_by_time_and_by_frame() {
    let _gpu = gpu_lock();
    if !gpu_available() {
        return;
    }
    let (a, b) = clip_paths();
    let dir = scratch("stdio");
    let file = dir.join("project.json");
    let project = json!({"version": 2, "output": {"width": 640, "height": 360, "frame_duration": FRAME}, "tracks": [{"clips": [{"source": a, "start": 0, "source_in": 0, "duration": 60 * FRAME}]}]});
    std::fs::write(&file, project.to_string()).unwrap();

    let mut client = Client::start();
    let tools = client.rpc("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"frame_capture") && names.contains(&"clip_add"), "{names:?}");

    let (opened, _) = client.ok("project_open", json!({"path": file}));
    // An edit: the second clip after the first, starting 20 frames into its source.
    let (added, _) = client.ok("clip_add", json!({"expected_revision": opened["revision"], "track": 0, "source": b, "start": 60 * FRAME, "source_in": 20 * FRAME, "duration": 30 * FRAME}));
    assert_eq!(added["clip"], json!({"track": 0, "clip": 1}));

    // Frame 70 is 10 frames into the added clip, so 30 frames into its source.
    let (by_frame, content) = client.ok("frame_capture", json!({"frame": 70}));
    assert_eq!((by_frame["frame"].clone(), by_frame["time"].clone()), (json!(70), json!(70 * FRAME)));
    assert_eq!(by_frame["revision"], added["revision"]);
    let (width, height, frame_pixels) = decode_png(&content[1]);
    assert_eq!((width, height), (640, 360));
    assert_eq!(read_frame_number(&frame_pixels, width), 30, "the picture of the edited timeline");

    // A time inside frame 70 is the same frame and the same picture.
    let (by_time, content) = client.ok("frame_capture", json!({"time": 70 * FRAME + 5}));
    assert_eq!(by_time["frame"], json!(70));
    assert_eq!(by_time["time"], json!(70 * FRAME));
    assert_eq!(decode_png(&content[1]).2, frame_pixels);

    let (_, content) = client.ok("frame_capture", json!({"frame": 5, "max_width": 320}));
    assert_eq!(decode_png(&content[1]).0, 320);

    for (arguments, code) in [(json!({"frame": 90}), "out_of_range"), (json!({"time": -1}), "out_of_range"), (json!({}), "invalid_params"), (json!({"time": 0, "frame": 0}), "invalid_params")] {
        let (content, error) = client.tool("frame_capture", arguments.clone());
        assert!(error, "{arguments}");
        let body: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["code"], code, "{arguments}");
    }
    let (content, error) = client.tool("clip_delete", json!({"expected_revision": 0, "clip": {"track": 0, "clip": 0}}));
    assert!(error && content[0]["text"].as_str().unwrap().contains("stale_revision"), "the server lives on after a refusal");
    client.ok("session_status", json!({}));
}

#[test]
fn clip_set_speed_changes_what_a_capture_shows() {
    let _gpu = gpu_lock();
    if !gpu_available() {
        return;
    }
    let (a, _) = clip_paths();
    let dir = scratch("speed");
    let file = dir.join("project.json");
    let project = json!({"version": 2, "output": {"width": 640, "height": 360, "frame_duration": FRAME}, "tracks": [{"clips": [{"source": a, "start": 0, "source_in": 0, "duration": 60 * FRAME}]}]});
    std::fs::write(&file, project.to_string()).unwrap();

    let mut client = Client::start();
    let tools = client.rpc("tools/list", json!({}));
    assert!(tools["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == "clip_set_speed"));
    let (opened, _) = client.ok("project_open", json!({"path": file}));
    let (set, _) = client.ok("clip_set_speed", json!({"expected_revision": opened["revision"], "clip": {"track": 0, "clip": 0}, "speed": 2.0}));

    // The 60 source frames now last 30 timeline frames: frame 10 shows source frame 20.
    let (by_frame, content) = client.ok("frame_capture", json!({"frame": 10}));
    assert_eq!(by_frame["revision"], set["revision"]);
    let (width, _, pixels) = decode_png(&content[1]);
    assert_eq!(read_frame_number(&pixels, width), 20, "source frame of 2x");
    let (read, _) = client.ok("project_get", json!({}));
    assert_eq!(read["project"]["tracks"][0]["clips"][0]["speed"], json!(2.0), "{read}");
    assert_eq!(read["frames"], json!(30));
    let (content, error) = client.tool("frame_capture", json!({"frame": 45}));
    assert!(error && content[0]["text"].as_str().unwrap().contains("out_of_range"), "the timeline got shorter");

    // A stale revision and an out of range speed are refused without changing anything.
    let (content, error) = client.tool("clip_set_speed", json!({"expected_revision": 0, "clip": {"track": 0, "clip": 0}, "speed": 0.5}));
    assert!(error && content[0]["text"].as_str().unwrap().contains("stale_revision"));
    let (content, error) = client.tool("clip_set_speed", json!({"expected_revision": set["revision"], "clip": {"track": 0, "clip": 0}, "speed": 20.0}));
    assert!(error && content[0]["text"].as_str().unwrap().contains("invalid_project"), "{content:?}");
    // clip_add takes a speed, and the default length follows it.
    let (added, _) = client.ok("clip_add", json!({"expected_revision": set["revision"], "track": 0, "source": a, "start": 90 * FRAME, "speed": 2.0}));
    let (read, _) = client.ok("project_get", json!({}));
    let clips = read["project"]["tracks"][0]["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[1]["speed"], json!(2.0));
    assert_eq!(added["clip"], json!({"track": 0, "clip": 1}));
}

fn render_tool() -> std::path::PathBuf {
    let tool = std::path::Path::new(env!("CARGO_BIN_EXE_gpufx-mcp")).with_file_name("gpufx-render.exe");
    assert!(tool.is_file(), "build gpufx-render too: {}", tool.display());
    tool
}

/// A long project, so an export is still running when the test acts on it.
fn long_project(file: &std::path::Path, clips: i64) {
    let (a, _) = clip_paths();
    let clips: Vec<Value> = (0..clips).map(|i| json!({"source": a, "start": i * 150 * FRAME, "source_in": 0, "duration": 150 * FRAME})).collect();
    let project = json!({"version": 2, "output": {"width": 640, "height": 360, "frame_duration": FRAME}, "tracks": [{"clips": clips}]});
    std::fs::write(file, project.to_string()).unwrap();
}

fn wait_for(limit: u64, mut done: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed().as_secs() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn exports_refuses_to_overwrite_and_its_frames_hash_like_the_cli() {
    let _gpu = gpu_lock();
    if !gpu_available() {
        return;
    }
    let dir = scratch("export");
    let file = dir.join("project.json");
    long_project(&file, 1);
    let out = dir.join("out.mp4");
    let mut client = Client::start();
    let (opened, _) = client.ok("project_open", json!({"path": file}));
    let (started, _) = client.ok("export_start", json!({"out": out}));
    let job = started["job"].clone();
    let (content, error) = client.tool("clip_delete", json!({"expected_revision": opened["revision"], "clip": {"track": 0, "clip": 0}}));
    assert!(error && content[0]["text"].as_str().unwrap().contains("busy"), "no edits during an export");
    let mut last = Value::Null;
    assert!(wait_for(120, || {
        last = client.ok("export_status", json!({"job": job})).0;
        last["state"] != "running"
    }), "{last}");
    assert_eq!(last["state"], "finished", "{last}");
    assert_eq!(last["report"]["frames"], json!(150));
    assert!(out.is_file());
    let (content, error) = client.tool("export_start", json!({"out": out}));
    assert!(error && content[0]["text"].as_str().unwrap().contains("file_exists"));
    std::fs::write(&out, "x").unwrap();
    let (again, _) = client.ok("export_start", json!({"out": out, "overwrite": true}));
    assert!(wait_for(120, || client.ok("export_status", json!({"job": again["job"]})).0["state"] != "running"), "the second export ended");
    assert!(std::fs::metadata(&out).unwrap().len() > 1, "overwrite replaced the file");

    // The picture an agent sees is the picture the renderer writes.
    let hashes = dir.join("hashes.txt");
    let cli = Command::new(render_tool()).arg(&file).arg("--out").arg(dir.join("cli.mp4")).arg("--hashes").arg(&hashes).arg("--packages").arg(packages_root()).output().expect("run gpufx-render");
    assert!(cli.status.success(), "{}", String::from_utf8_lossy(&cli.stderr));
    let expected = std::fs::read_to_string(&hashes).unwrap().lines().nth(42).unwrap().split_whitespace().nth(1).unwrap().to_owned();
    use sha2::Digest;
    let (_, content) = client.ok("frame_capture", json!({"frame": 42}));
    let digest = sha2::Sha256::digest(decode_png(&content[1]).2);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(actual, expected, "frame 42");
}

#[test]
fn cancelling_an_export_leaves_no_files() {
    let _gpu = gpu_lock();
    if !gpu_available() {
        return;
    }
    let dir = scratch("cancel");
    let file = dir.join("project.json");
    long_project(&file, 20);
    let mut client = Client::start();
    client.ok("project_open", json!({"path": file}));
    client.ok("export_start", json!({"out": dir.join("out.mp4")}));
    assert!(wait_for(60, || client.ok("export_status", json!({})).0["done"].as_i64().unwrap_or(0) > 0), "the export got going");
    client.ok("export_cancel", json!({}));
    assert!(wait_for(60, || client.ok("export_status", json!({})).0["state"] == "cancelled"));
    assert_eq!(names_in(&dir), ["project.json", "project.json.lock"], "only the project and the lock of the open session");
}

#[test]
fn a_server_that_dies_stops_its_export_cleanly() {
    let _gpu = gpu_lock();
    if !gpu_available() {
        return;
    }
    let dir = scratch("dies");
    let file = dir.join("project.json");
    long_project(&file, 20);
    let mut client = Client::start();
    client.ok("project_open", json!({"path": file}));
    client.ok("export_start", json!({"out": dir.join("out.mp4")}));
    assert!(wait_for(60, || client.ok("export_status", json!({})).0["done"].as_i64().unwrap_or(0) > 0));
    assert!(names_in(&dir).iter().any(|name| name.contains(".tmp")), "the render is writing");
    client.child.kill().unwrap();
    client.child.wait().unwrap();
    assert!(wait_for(30, || names_in(&dir) == ["project.json"]), "{:?}", names_in(&dir));
}
