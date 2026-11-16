//! The beat and marker tools on a [`FileSession`]: beats_detect on a click track written as a WAV
//! file, the beats following the clip, and the marker tools.

mod common;

use common::*;
use engine::Project;
use gpufx_mcp::session::FileSession;
use serde_json::{json, Value};

const SECOND: i64 = 90_000;
const RATE: u32 = 48_000;

fn project_of(session: &mut FileSession) -> Project {
    serde_json::from_value(call_ok(session, "project_get", json!({}))["project"].clone()).expect("a project")
}

/// Writes `seconds` of stereo 16-bit clicks at `bpm`, the first at `offset` seconds.
fn write_click_wav(path: &std::path::Path, bpm: f64, seconds: f64, offset: f64) {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut mono = vec![0.0_f64; frames];
    let mut index = 0;
    loop {
        let at = offset + index as f64 * 60.0 / bpm;
        if at + 0.05 >= seconds {
            break;
        }
        let first = (at * f64::from(RATE)).round() as usize;
        for n in 0..(0.015 * f64::from(RATE)) as usize {
            let time = n as f64 / f64::from(RATE);
            mono[first + n] += 0.6 * (-time / 0.004).exp() * (std::f64::consts::TAU * 1500.0 * time).sin();
        }
        index += 1;
    }
    let data_len = (frames * 4) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    for value in [16u32.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), 2u16.to_le_bytes().to_vec(), RATE.to_le_bytes().to_vec(), (RATE * 4).to_le_bytes().to_vec(), 4u16.to_le_bytes().to_vec(), 16u16.to_le_bytes().to_vec()] {
        bytes.extend_from_slice(&value);
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in mono {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write the wav");
}

/// An audio track with a 120 BPM click track of 12 s at 2 s on the timeline. Returns the revision.
fn music(session: &mut FileSession, name: &str) -> u64 {
    let path = scratch(name).join("clicks.wav");
    write_click_wav(&path, 120.0, 12.0, 0.25);
    call_ok(session, "track_add", json!({"expected_revision": 0, "kind": "audio"}));
    revision(&call_ok(session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": path.to_string_lossy(), "start": 2 * SECOND})))
}

fn times(beats: &Value) -> Vec<i64> {
    beats.as_array().unwrap().iter().map(|beat| beat["at"].as_i64().unwrap()).collect()
}

#[test]
fn beats_detect_stores_the_beats_on_the_clip_and_they_follow_it() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let now = music(&mut session, "beats-follow");
    let clip = json!({"track": 0, "clip": 0});
    let detected = call_ok(&mut session, "beats_detect", json!({"expected_revision": now, "clip": clip}));
    assert_eq!(detected["state"], "done");
    let tempo = detected["tempo"].as_f64().unwrap();
    assert!((tempo - 120.0).abs() < 1.0, "{detected}");
    let shown = times(&detected["beats"]);
    assert!(shown.len() >= 20 && detected["count"] == shown.len(), "{detected}");
    // The first click is at 0.25 s of the source, which plays from 2 s.
    assert!((shown[0] - (2 * SECOND + SECOND / 4)).abs() <= SECOND / 50, "{}", shown[0]);
    assert!(detected["beats"].as_array().unwrap().iter().any(|beat| beat["downbeat"] == true));

    // The project keeps them in source time.
    let project = project_of(&mut session);
    let beats = &project.tracks[0].clips[0].beats;
    assert_eq!(beats.beats.len(), shown.len());
    assert!((beats.beats[0].at.ticks() - SECOND / 4).abs() <= SECOND / 50);

    // A move carries them along.
    let moved = call_ok(&mut session, "clip_move", json!({"expected_revision": revision(&detected), "clip": clip, "track": 0, "start": 5 * SECOND}));
    let listed = call_ok(&mut session, "markers_list", json!({}));
    let after = times(&listed["beats"][0]["beats"]);
    assert_eq!(after, shown.iter().map(|at| at + 3 * SECOND).collect::<Vec<_>>());

    // Detecting again is one more undo step, from the cache; clearing is one too.
    let again = call_ok(&mut session, "beats_detect", json!({"expected_revision": revision(&moved), "clip": clip}));
    assert_eq!(again["state"], "done");
    let cleared = call_ok(&mut session, "clip_beats_clear", json!({"expected_revision": revision(&again), "clip": clip}));
    assert!(project_of(&mut session).tracks[0].clips[0].beats.is_empty());
    call_ok(&mut session, "undo", json!({"expected_revision": revision(&cleared)}));
    assert_eq!(project_of(&mut session).tracks[0].clips[0].beats.beats.len(), shown.len());
}

#[test]
fn beats_detect_refuses_a_still_a_stale_revision_and_a_missing_clip() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let now = music(&mut session, "beats-refused");
    assert_eq!(call_err(&mut session, "beats_detect", json!({"expected_revision": now + 1, "clip": {"track": 0, "clip": 0}})).code, "stale_revision");
    assert_eq!(call_err(&mut session, "beats_detect", json!({"expected_revision": now, "clip": {"track": 0, "clip": 3}})).code, "clip_not_found");
    let text = call_ok(&mut session, "text_add", json!({"expected_revision": now, "start": 0, "duration": SECOND, "content": "hi"}));
    let track = text["clip"]["track"].clone();
    assert_eq!(call_err(&mut session, "beats_detect", json!({"expected_revision": revision(&text), "clip": {"track": track, "clip": 0}})).code, "invalid_params");
}

#[test]
fn marker_tools_add_move_list_and_remove() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let added = call_ok(&mut session, "marker_add", json!({"expected_revision": 0, "time": 3 * SECOND, "label": "Chorus", "color": [255, 0, 0, 255]}));
    assert_eq!(added["marker"], 0);
    let earlier = call_ok(&mut session, "marker_add", json!({"expected_revision": revision(&added), "time": SECOND}));
    assert_eq!(earlier["marker"], 0);
    let moved = call_ok(&mut session, "marker_move", json!({"expected_revision": revision(&earlier), "marker": 0, "time": 5 * SECOND}));
    assert_eq!(moved["marker"], 1);
    let listed = call_ok(&mut session, "markers_list", json!({}));
    assert_eq!(listed["markers"], json!([{"marker": 0, "at": 3 * SECOND, "label": "Chorus", "color": [255, 0, 0, 255]}, {"marker": 1, "at": 5 * SECOND, "label": "", "color": null}]));
    // The project description has them too, as the file stores them.
    let project = call_ok(&mut session, "project_get", json!({}));
    assert_eq!(project["project"]["markers"][1], json!({"at": 5 * SECOND}));
    assert_eq!(call_err(&mut session, "marker_remove", json!({"expected_revision": revision(&moved), "marker": 2})).code, "marker_not_found");
    assert_eq!(call_err(&mut session, "marker_add", json!({"expected_revision": revision(&moved), "time": -1})).code, "out_of_range");
    assert_eq!(call_err(&mut session, "marker_add", json!({"expected_revision": revision(&moved), "time": 0, "color": [1, 2, 3]})).code, "invalid_params");
    call_ok(&mut session, "marker_remove", json!({"expected_revision": revision(&moved), "marker": 0}));
    assert_eq!(project_of(&mut session).markers.len(), 1);
}
