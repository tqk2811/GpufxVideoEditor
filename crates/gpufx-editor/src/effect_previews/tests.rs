use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

const SPEC: PreviewSpec = PreviewSpec::STANDARD;

fn scratch_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("gpufx-previews-unit-{}-{name}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn package_dir(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = scratch_dir(name);
    for (relative, bytes) in files {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    dir
}

fn image(width: u32, height: u32, seed: u8) -> PreviewImage {
    let rgba = (0..width * height * 4).map(|index| (index as u8).wrapping_mul(7).wrapping_add(seed)).collect();
    PreviewImage { width, height, rgba }
}

fn store(name: &str) -> PreviewStore {
    PreviewStore::new(scratch_dir(name).join("previews"))
}

fn names(store: &PreviewStore) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(&store.root).map(|entries| entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    names
}

#[test]
fn key_changes_with_a_package_byte_a_path_an_image_or_the_spec() {
    let base = package_digest(&package_dir("key-base", &[("effect.json", b"{}"), ("shaders/a.wgsl", b"fn a() {}")])).unwrap();
    let byte = package_digest(&package_dir("key-byte", &[("effect.json", b"{}"), ("shaders/a.wgsl", b"fn b() {}")])).unwrap();
    let path = package_digest(&package_dir("key-path", &[("effect.json", b"{}"), ("shaders/b.wgsl", b"fn a() {}")])).unwrap();
    assert_ne!(base, byte);
    assert_ne!(base, path);
    assert_ne!(byte, path);

    let image_a: &[u8] = b"image a";
    let image_b: &[u8] = b"image b";
    let key = preview_key(&base, &[image_a], SPEC);
    assert_eq!(key.len(), 16);
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(key, preview_key(&base, &[image_a], SPEC));
    assert_ne!(key, preview_key(&byte, &[image_a], SPEC));
    assert_ne!(key, preview_key(&base, &[image_b], SPEC));
    assert_ne!(key, preview_key(&base, &[image_a, image_b], SPEC));
    for spec in [PreviewSpec { width: 145, ..SPEC }, PreviewSpec { height: 145, ..SPEC }, PreviewSpec { frames: 25, ..SPEC }, PreviewSpec { fps: 21, ..SPEC }] {
        assert_ne!(key, preview_key(&base, &[image_a], spec));
    }
}

#[test]
fn image_boundaries_are_unambiguous() {
    let package = [1u8; 32];
    let joined = preview_key(&package, &[b"ab".as_slice(), b"c".as_slice()], SPEC);
    let moved = preview_key(&package, &[b"a".as_slice(), b"bc".as_slice()], SPEC);
    assert_ne!(joined, moved);
}

#[test]
fn directory_digest_ignores_the_order_files_were_made_in() {
    let forward = package_dir("order-forward", &[("a.txt", b"1"), ("b/c.txt", b"2"), ("z.txt", b"3")]);
    let backward = package_dir("order-backward", &[("z.txt", b"3"), ("b/c.txt", b"2"), ("a.txt", b"1")]);
    assert_eq!(package_digest(&forward).unwrap(), package_digest(&backward).unwrap());
}

#[test]
fn directory_digest_separates_the_path_from_the_bytes() {
    let one = package_dir("split-one", &[("ab", b"c")]);
    let two = package_dir("split-two", &[("a", b"bc")]);
    assert_ne!(package_digest(&one).unwrap(), package_digest(&two).unwrap());
}

#[test]
fn zip_digest_follows_its_bytes() {
    let dir = scratch_dir("zip");
    let one = dir.join("one.zip");
    let same = dir.join("same.zip");
    let other = dir.join("other.zip");
    std::fs::write(&one, b"PK zip bytes").unwrap();
    std::fs::write(&same, b"PK zip bytes").unwrap();
    std::fs::write(&other, b"PK zip bytez").unwrap();
    assert_eq!(package_digest(&one).unwrap(), package_digest(&same).unwrap());
    assert_ne!(package_digest(&one).unwrap(), package_digest(&other).unwrap());
    assert!(package_digest(&dir.join("missing.zip")).is_err());
}

#[test]
fn package_prefix_is_eight_hex_characters_per_name() {
    let prefix = package_prefix("Blur");
    assert_eq!(prefix.len(), 8);
    assert!(prefix.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(prefix, package_prefix("Blur"));
    assert_ne!(prefix, package_prefix("Glow"));
}

#[test]
fn still_and_strip_round_trip() {
    let store = store("round-trip");
    let still = image(144, 144, 1);
    let strip = image(144, 144 * 24, 2);
    store.write_still("aaaaaaaa", "0123456789abcdef", SPEC, &still).unwrap();
    store.write_loop("aaaaaaaa", "0123456789abcdef", SPEC, &strip).unwrap();
    assert_eq!(store.read_still("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), Some(still));
    assert_eq!(store.read_loop("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), Some(strip));
    assert_eq!(names(&store), ["aaaaaaaa-0123456789abcdef.loop.png", "aaaaaaaa-0123456789abcdef.still.png"]);
}

#[test]
fn an_effect_that_does_not_change_stores_one_frame_as_its_loop() {
    let store = store("single-frame");
    let frame = image(144, 144, 3);
    store.write_loop("aaaaaaaa", "0123456789abcdef", SPEC, &frame).unwrap();
    assert_eq!(store.read_loop("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), Some(frame));
}

#[test]
fn reading_with_no_directory_or_no_file_is_none() {
    let store = store("missing");
    assert_eq!(store.read_still("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), None);
    assert_eq!(store.read_loop("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), None);
    assert_eq!(store.sweep(DISK_LIMIT, SystemTime::now()).unwrap(), SweepReport::default());
    assert!(!store.root.exists());
}

#[test]
fn a_corrupt_png_is_removed_and_reads_as_none() {
    let store = store("corrupt");
    store.write_still("aaaaaaaa", "0123456789abcdef", SPEC, &image(144, 144, 4)).unwrap();
    let path = store.still_path("aaaaaaaa", "0123456789abcdef");
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    for byte in &mut bytes[middle..middle + 8] {
        *byte ^= 0xff;
    }
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(store.read_still("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), None);
    assert!(!path.exists());

    store.write_still("aaaaaaaa", "0123456789abcdef", SPEC, &image(144, 144, 4)).unwrap();
    std::fs::write(&path, b"not a png at all").unwrap();
    assert_eq!(store.read_still("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), None);
    assert!(!path.exists());
}

#[test]
fn a_png_with_a_wrong_size_is_removed_and_reads_as_none() {
    let store = store("wrong-size");
    // Put files of the wrong size where the store keeps its own, as an older version might have.
    let wrong = |width, height| {
        std::fs::create_dir_all(&store.root).unwrap();
        let bytes = {
            let mut bytes = Vec::new();
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&vec![9u8; (width * height * 4) as usize]).unwrap();
            writer.finish().unwrap();
            bytes
        };
        bytes
    };
    let still = store.still_path("aaaaaaaa", "0123456789abcdef");
    std::fs::write(&still, wrong(100, 100)).unwrap();
    assert_eq!(store.read_still("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), None);
    assert!(!still.exists());

    let strip = store.loop_path("aaaaaaaa", "0123456789abcdef");
    for (width, height) in [(144, 200), (100, 144), (144, 144 * 25)] {
        std::fs::write(&strip, wrong(width, height)).unwrap();
        assert_eq!(store.read_loop("aaaaaaaa", "0123456789abcdef", SPEC).unwrap(), None, "{width}x{height}");
        assert!(!strip.exists());
    }

    assert!(store.write_still("aaaaaaaa", "0123456789abcdef", SPEC, &image(100, 100, 1)).is_err());
    assert!(names(&store).is_empty());
}

#[test]
fn a_tmp_file_from_a_crash_is_swept_only_when_old() {
    let store = store("tmp");
    std::fs::create_dir_all(&store.root).unwrap();
    let now = SystemTime::now();
    let old = store.root.join("aaaaaaaa-0123456789abcdef.still.png.111.tmp");
    let recent = store.root.join("aaaaaaaa-0123456789abcdef.loop.png.222.tmp");
    std::fs::write(&old, b"half").unwrap();
    std::fs::write(&recent, b"half").unwrap();
    std::fs::File::options().write(true).open(&old).unwrap().set_modified(now - Duration::from_secs(2 * 60 * 60)).unwrap();
    std::fs::File::options().write(true).open(&recent).unwrap().set_modified(now - Duration::from_secs(60)).unwrap();

    let report = store.sweep(DISK_LIMIT, now).unwrap();
    assert_eq!(report, SweepReport { files: 1, bytes: 4 });
    assert!(!old.exists());
    assert!(recent.exists());
}

#[test]
fn a_new_key_removes_the_old_keys_files_but_not_another_packages() {
    let store = store("replace");
    let (mine, other) = ("aaaaaaaa", "bbbbbbbb");
    store.write_still(mine, "1111111111111111", SPEC, &image(144, 144, 1)).unwrap();
    store.write_loop(mine, "1111111111111111", SPEC, &image(144, 144 * 24, 1)).unwrap();
    store.mark_failed(mine, "1111111111111111", "old").unwrap();
    store.write_still(other, "2222222222222222", SPEC, &image(144, 144, 2)).unwrap();

    store.write_still(mine, "3333333333333333", SPEC, &image(144, 144, 3)).unwrap();
    assert_eq!(names(&store), ["aaaaaaaa-3333333333333333.still.png", "bbbbbbbb-2222222222222222.still.png"]);

    // The loop of the same key is kept when it is written after the still.
    store.write_loop(mine, "3333333333333333", SPEC, &image(144, 144 * 24, 3)).unwrap();
    assert_eq!(names(&store), ["aaaaaaaa-3333333333333333.loop.png", "aaaaaaaa-3333333333333333.still.png", "bbbbbbbb-2222222222222222.still.png"]);
}

#[test]
fn the_attempt_count_goes_from_zero_to_one() {
    let store = store("attempt");
    assert_eq!(store.mark_attempt("aaaaaaaa", "0123456789abcdef").unwrap(), 0);
    assert_eq!(store.mark_attempt("aaaaaaaa", "0123456789abcdef").unwrap(), 1);
    assert_eq!(store.mark_attempt("aaaaaaaa", "fedcba9876543210").unwrap(), 0);
    store.clear_attempt("aaaaaaaa", "0123456789abcdef");
    assert_eq!(store.mark_attempt("aaaaaaaa", "0123456789abcdef").unwrap(), 0);
}

#[test]
fn a_failed_marker_round_trips_its_reason() {
    let store = store("failed");
    assert_eq!(store.failure("aaaaaaaa", "0123456789abcdef"), None);
    store.mark_failed("aaaaaaaa", "0123456789abcdef", "the shader did not compile: caf\u{e9}").unwrap();
    assert_eq!(store.failure("aaaaaaaa", "0123456789abcdef").as_deref(), Some("the shader did not compile: caf\u{e9}"));
    assert_eq!(store.failure("aaaaaaaa", "fedcba9876543210"), None);
}

#[test]
fn sweep_evicts_the_oldest_until_under_the_limit() {
    let store = store("evict");
    std::fs::create_dir_all(&store.root).unwrap();
    let now = SystemTime::now();
    for (index, name) in ["oldest", "older", "newer", "newest"].iter().enumerate() {
        let path = store.root.join(format!("aaaaaaaa-{name}.still.png"));
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        let age = Duration::from_secs(1000 - index as u64 * 100);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(now - age).unwrap();
    }
    let report = store.sweep(250, now).unwrap();
    assert_eq!(report, SweepReport { files: 2, bytes: 200 });
    assert_eq!(names(&store), ["aaaaaaaa-newer.still.png", "aaaaaaaa-newest.still.png"]);

    // Already under the limit: nothing more goes.
    assert_eq!(store.sweep(250, now).unwrap(), SweepReport::default());
    assert_eq!(names(&store).len(), 2);
}
