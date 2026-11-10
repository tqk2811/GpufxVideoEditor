//! A real zip from the import through the library listing to a project that names one of its sounds.

#![cfg(windows)]

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use d3d12_backend::{probe_media, AudioFileDecoder, ProjectPaths};
use engine::{Resolver, Time, TICKS_PER_SECOND};
use gpufx_editor::audio_library::scan_audio_packs;
use gpufx_editor::package_import::{install_zip, ImportOutcome};
use gpufx_editor::project_file::{load_project_file, load_project_file_for_agent, save_project_file};
use zip::write::SimpleFileOptions;

const SECONDS: u32 = 2;
const SOURCE: &str = "asset:sfx/avs-sfx-impact/audio/boom.flac";

/// PCM in a WAV container: FFmpeg goes by the content, so this decodes under a `.flac` name.
fn wav_bytes() -> Vec<u8> {
    let samples: Vec<u8> = (0..48_000 * SECONDS).flat_map(|index| [(index % 200) as i16 * 40; 2]).flat_map(i16::to_le_bytes).collect();
    let mut wav = Vec::with_capacity(samples.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&48_000u32.to_le_bytes());
    wav.extend_from_slice(&192_000u32.to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    wav.extend_from_slice(&samples);
    wav
}

fn pack_zip() -> Vec<u8> {
    // The manifest length is deliberately not the real one.
    let manifest = r#"{"format":1,"type":"sfx","id":"avs-sfx-impact","name":"SFX: Impact","version":"1.0.0","license":{"spdx":"MIT","files":["LICENSE.txt","NOTICE.txt"]},"items":[{"id":"boom","file":"audio/boom.flac","name":"Boom","duration":1.2,"tags":["hit"],"category":"impact","loop":false}]}"#;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    let audio = wav_bytes();
    let files: [(&str, &[u8]); 4] = [("manifest.json", manifest.as_bytes()), ("LICENSE.txt", b"MIT"), ("NOTICE.txt", b"Notice"), ("audio/boom.flac", &audio)];
    for (name, content) in files {
        writer.start_file(name, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gpufx-audio-pack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn project_text(source: &str) -> String {
    format!(r#"{{"version":2,"output":{{"width":16,"height":16,"frame_duration":3000,"audio":{{"sample_rate":48000,"channels":2,"bitrate":128000}}}},"tracks":[{{"kind":"audio","clips":[{{"source":"{source}","start":0,"source_in":0,"duration":90000}}]}}]}}"#)
}

fn install(scratch: &Scratch, root: &Path) -> ImportOutcome {
    let zip = scratch.file("pack.zip");
    std::fs::write(&zip, pack_zip()).unwrap();
    install_zip(&zip, root, false).expect("the pack installs")
}

#[test]
fn an_installed_pack_is_listed_and_a_project_naming_its_sound_validates_saves_and_decodes() {
    let scratch = Scratch::new("end-to-end");
    let root = scratch.file("user");
    let outcome = install(&scratch, &root);
    assert!(matches!(&outcome, ImportOutcome::AudioInstalled(pack) if pack.id == "avs-sfx-impact" && !pack.replaced), "{outcome:?}");

    let roots = vec![root.clone()];
    let library = scan_audio_packs(&roots);
    assert_eq!(library.packs.len(), 1, "{:?}", library.problems);
    assert_eq!(library.packs[0].name, "SFX: Impact");
    let sound = &library.packs[0].sounds[0];
    assert_eq!(sound.source, SOURCE);
    assert_eq!(sound.duration, Some(Time::from_ticks(TICKS_PER_SECOND * 6 / 5)), "the listing shows the manifest's length");

    let file = scratch.file("project.json");
    std::fs::write(&file, project_text(SOURCE)).unwrap();
    let project = load_project_file(&file, &roots).expect("the project validates");
    assert_eq!(project.tracks[0].clips[0].source, SOURCE, "kept as it is in memory");
    load_project_file_for_agent(&file, &roots).expect("an agent may use it too");

    save_project_file(&project, &file).unwrap();
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains(SOURCE), "{saved}");
    assert!(!saved.contains(root.to_str().unwrap()) && !saved.contains("Temp"), "no absolute path in the file: {saved}");

    let paths = ProjectPaths::roots_only(&file, roots);
    assert!(paths.source_exists(SOURCE));
    assert!(paths.source_has_audio(SOURCE), "the audio stream is found through the pack");
    let real = paths.source_path(SOURCE);
    let probe = probe_media(&real).expect("the probe reads it");
    assert_eq!(probe.audio_duration, Some(Time::from_ticks(TICKS_PER_SECOND * i64::from(SECONDS))), "the real length, not the manifest's");
    assert!(AudioFileDecoder::open(&real).is_ok(), "it decodes");
}

#[test]
fn a_project_missing_its_pack_says_which_one_and_an_escape_is_refused_for_agents_too() {
    let scratch = Scratch::new("missing");
    let root = scratch.file("user");
    let roots = vec![root];
    let file = scratch.file("project.json");

    std::fs::write(&file, project_text(SOURCE)).unwrap();
    let error = load_project_file(&file, &roots).unwrap_err();
    assert!(error.contains("avs-sfx-impact") && error.contains("not installed"), "{error}");

    std::fs::write(&file, project_text("asset:sfx/../../secret.flac")).unwrap();
    assert!(load_project_file(&file, &roots).unwrap_err().contains("not a valid asset source"));
    assert!(load_project_file_for_agent(&file, &roots).unwrap_err().contains("not a valid asset source"));
}

#[test]
fn an_older_project_with_plain_sources_still_loads_unchanged() {
    let scratch = Scratch::new("plain");
    let media = scratch.file("a.wav");
    std::fs::write(&media, wav_bytes()).unwrap();
    let file = scratch.file("project.json");
    std::fs::write(&file, project_text("a.wav")).unwrap();
    let project = load_project_file(&file, &[]).expect("a relative source still opens");
    assert_eq!(project.tracks[0].clips[0].source, media.to_string_lossy());
}
