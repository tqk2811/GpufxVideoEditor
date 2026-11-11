//! The tools of text clips on a [`FileSession`]: a text added is read back as it was asked for,
//! changing its size or colour is one undo step that carries its place along, and what no text can
//! be is refused with a clear code and changes nothing.

mod common;

use std::time::{Duration, Instant};

use common::*;
use engine::{AnimationGroup, ClipPlacement, FontRef, Project, TextContent};
use gpufx_mcp::session::FileSession;
use serde_json::{json, Value};

const SECOND: i64 = 90_000;

fn project_of(session: &mut FileSession) -> Project {
    let value = call_ok(session, "project_get", json!({}));
    serde_json::from_value(value["project"].clone()).expect("a project")
}

fn text_of(session: &mut FileSession, track: usize, clip: usize) -> TextContent {
    project_of(session).tracks[track].clips[clip].text.clone().expect("a text clip")
}

fn placement_of(session: &mut FileSession, track: usize, clip: usize) -> ClipPlacement {
    project_of(session).tracks[track].clips[clip].placement.expect("a placed clip")
}

/// A session with one text "Hello" of two seconds on a new track, and its revision.
fn with_text(session: &mut FileSession) -> u64 {
    revision(&call_ok(session, "text_add", json!({"expected_revision": 0, "start": 0, "duration": 2 * SECOND, "content": "Hello"})))
}

#[test]
fn a_text_added_is_read_back_with_the_defaults_of_the_text_tab_and_its_natural_place() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let added = call_ok(&mut session, "text_add", json!({"expected_revision": 0, "start": SECOND, "duration": 3 * SECOND, "content": "Hello\nworld"}));
    assert_eq!(added["clip"], json!({"track": 0, "clip": 0}), "a new video track took it");
    let project = project_of(&mut session);
    assert_eq!(project.tracks.len(), 1);
    assert_eq!(project.tracks[0].kind, engine::TrackKind::Video);
    let clip = &project.tracks[0].clips[0];
    assert!(clip.is_text());
    assert_eq!((clip.start.ticks(), clip.duration.ticks()), (SECOND, 3 * SECOND));
    let text = clip.text.clone().unwrap();
    assert_eq!(text, gpufx_editor::text_clip::default_text("Hello\nworld"));
    // The natural size, in the middle of the frame.
    let placement = clip.placement.expect("a place");
    assert!(placement.width > 0.0 && placement.width <= 1.0, "{placement:?}");
    assert!((placement.x + placement.width / 2.0 - 0.5).abs() < 1e-6, "{placement:?}");
    // The project JSON an agent reads shows the text.
    let value = call_ok(&mut session, "project_get", json!({}));
    assert_eq!(value["project"]["tracks"][0]["clips"][0]["text"]["content"], "Hello\nworld");
}

#[test]
fn a_text_added_with_a_style_keeps_every_field_and_a_second_one_shares_a_free_track() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let style = json!({
        "size": 0.12, "color": [255, 0, 0, 255], "align": "left", "line_spacing": 1.5,
        "outline": {"color": [0, 0, 0, 255], "width": 0.05},
        "shadow": {"color": [0, 0, 0, 128], "offset": [0.05, 0.05], "blur": 0.1},
        "animation": {"in": {"preset": "fade", "duration": 45_000}, "loop": {"preset": "pulse", "duration": SECOND}}
    });
    let first = call_ok(&mut session, "text_add", json!({"expected_revision": 0, "start": 0, "duration": SECOND, "content": "Title", "style": style}));
    let second = call_ok(&mut session, "text_add", json!({"expected_revision": revision(&first), "start": SECOND, "duration": SECOND, "content": "After"}));
    assert_eq!(second["clip"], json!({"track": 0, "clip": 1}), "track absent: the first video track with room");
    let text = text_of(&mut session, 0, 0);
    assert_eq!(text.size, 0.12);
    assert_eq!(text.color, [255, 0, 0, 255]);
    assert_eq!(text.align, engine::TextAlign::Left);
    assert_eq!(text.line_spacing, 1.5);
    assert_eq!(text.outline.map(|outline| outline.width), Some(0.05));
    assert_eq!(text.shadow.map(|shadow| shadow.blur), Some(0.1));
    assert_eq!(text.background, None);
    assert_eq!(text.animation.enter.map(|animation| (animation.preset, animation.duration.ticks())), Some(("fade".to_owned(), 45_000)));
    assert_eq!(text.animation.repeat.map(|animation| animation.preset), Some("pulse".to_owned()));
    assert!(text.animation.exit.is_none());
    // An overlap on the same track goes to a new one.
    let third = call_ok(&mut session, "text_add", json!({"expected_revision": revision(&second), "start": 0, "duration": SECOND, "content": "Over"}));
    assert_eq!(third["clip"], json!({"track": 1, "clip": 0}));
}

#[test]
fn text_set_recolours_and_a_bigger_size_scales_the_place_around_its_centre_and_undo_restores_both() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let added = with_text(&mut session);
    let before_project = project_of(&mut session);
    let before = placement_of(&mut session, 0, 0);
    let output = &before_project.output;
    let aspect = f64::from(output.width) / f64::from(output.height);

    let recoloured = call_ok(&mut session, "text_set", json!({"expected_revision": added, "clip": {"track": 0, "clip": 0}, "color": [0, 255, 0, 255]}));
    assert_eq!(recoloured["clip"], json!({"track": 0, "clip": 0}));
    assert_eq!(text_of(&mut session, 0, 0).color, [0, 255, 0, 255]);
    assert_eq!(text_of(&mut session, 0, 0).content, "Hello", "what is absent stays");
    let same = placement_of(&mut session, 0, 0);
    assert!((same.width - before.width).abs() < 1e-9 && (same.x - before.x).abs() < 1e-9 && (same.y - before.y).abs() < 1e-9, "a colour does not move the text");

    let text_before = text_of(&mut session, 0, 0);
    let doubled = call_ok(&mut session, "text_set", json!({"expected_revision": revision(&recoloured), "clip": {"track": 0, "clip": 0}, "size": 0.16}));
    let after = placement_of(&mut session, 0, 0);
    let text_after = text_of(&mut session, 0, 0);
    assert!((after.width / before.width - 2.0).abs() < 0.05, "twice the size, twice the width: {before:?} {after:?}");
    // The height follows the shape of the picture; the centre stays.
    let rect = |placement: ClipPlacement, text: &TextContent| {
        let pixels = gpufx_editor::text_shapes::TextShapes::default().shape(text, output.height).pixels;
        engine::PlacedRect::of(Some(placement), aspect, f64::from(pixels.0) / f64::from(pixels.1))
    };
    let (old, new) = (rect(before, &text_before), rect(after, &text_after));
    assert!((old.x + old.width / 2.0 - new.x - new.width / 2.0).abs() < 1e-6 && (old.y + old.height / 2.0 - new.y - new.height / 2.0).abs() < 1e-6, "{old:?} {new:?}");

    let undone = call_ok(&mut session, "undo", json!({"expected_revision": revision(&doubled)}));
    assert_eq!(placement_of(&mut session, 0, 0), same);
    assert_eq!(text_of(&mut session, 0, 0).size, 0.08, "the size and the place went back in one step");
    call_ok(&mut session, "undo", json!({"expected_revision": revision(&undone)}));
    assert_eq!(text_of(&mut session, 0, 0).color, [255, 255, 255, 255]);
}

#[test]
fn text_set_changes_the_content_and_takes_an_outline_and_an_animation_off_with_null() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let style = json!({"outline": {"color": [0, 0, 0, 255], "width": 0.1}, "animation": {"out": {"preset": "fade", "duration": 9000}}});
    let added = call_ok(&mut session, "text_add", json!({"expected_revision": 0, "start": 0, "duration": SECOND, "content": "Hi", "style": style}));
    let longer = call_ok(&mut session, "text_set", json!({"expected_revision": revision(&added), "clip": {"track": 0, "clip": 0}, "content": "A much longer line"}));
    let width = |session: &mut FileSession| placement_of(session, 0, 0).width;
    assert_eq!(text_of(&mut session, 0, 0).content, "A much longer line");
    assert!(text_of(&mut session, 0, 0).outline.is_some(), "the style stays");
    let wide = width(&mut session);
    assert!(wide > 0.2, "longer content draws wider: {wide}");
    call_ok(&mut session, "text_set", json!({"expected_revision": revision(&longer), "clip": {"track": 0, "clip": 0}, "outline": null, "animation": {"out": null}}));
    let text = text_of(&mut session, 0, 0);
    assert!(text.outline.is_none() && text.animation.is_empty(), "{text:?}");
}

#[test]
fn a_text_no_text_can_be_is_refused_with_a_clear_code_and_changes_nothing() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let current = with_text(&mut session);
    let known = project_of(&mut session);
    let clip = json!({"track": 0, "clip": 0});
    let add = |arguments: Value| {
        let mut call = json!({"expected_revision": current, "start": 5 * SECOND, "duration": SECOND, "content": "x"});
        for (name, value) in arguments.as_object().unwrap() {
            call[name] = value.clone();
        }
        call
    };
    let set = |arguments: Value| {
        let mut call = json!({"expected_revision": current, "clip": clip.clone()});
        for (name, value) in arguments.as_object().unwrap() {
            call[name] = value.clone();
        }
        call
    };

    for (tool, arguments, why) in [
        ("text_add", add(json!({"content": ""})), "empty content"),
        ("text_add", add(json!({"duration": 0})), "no duration"),
        ("text_add", add(json!({"style": {"size": 0}})), "size 0"),
        ("text_add", add(json!({"style": {"size": -0.1}})), "negative size"),
        ("text_add", add(json!({"style": {"line_spacing": 0}})), "line spacing 0"),
        ("text_add", add(json!({"style": {"color": [1, 2, 3]}})), "a colour of three numbers"),
        ("text_add", add(json!({"style": {"colour": [1, 2, 3, 4]}})), "a misnamed field"),
        ("text_add", add(json!({"style": {"animation": {"in": {"preset": "bounce", "duration": 9000}}}})), "a loop preset in the in group"),
        ("text_add", add(json!({"style": {"animation": {"loop": {"preset": "nope", "duration": 9000}}}})), "an unknown preset"),
        ("text_add", add(json!({"style": {"animation": {"out": {"preset": "fade", "duration": 0}}}})), "an animation of no length"),
        ("text_set", set(json!({"content": ""})), "empty content"),
        ("text_set", set(json!({"size": 0})), "size 0"),
        ("text_set", set(json!({"animation": {"loop": {"preset": "fade", "duration": 9000}}})), "an in preset in the loop group"),
        ("text_set", set(json!({})), "nothing to change"),
        ("text_set", set(json!({"font": {"kind": "file", "path": r"C:\no\such\font-123.ttf"}})), "a font file that is not there"),
        ("text_set", set(json!({"font": {"kind": "file", "path": "fonts/a.ttf"}})), "a relative font file"),
        ("text_set", set(json!({"font": {"kind": "file", "path": "//server/share/a.ttf"}})), "a font file on a network share"),
        ("text_add", add(json!({"style": {"font": {"kind": "file", "path": concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")}}})), "a file that is not a font"),
    ] {
        assert_eq!(call_err(&mut session, tool, arguments).code, "invalid_params", "{tool}: {why}");
    }
    let error = call_err(&mut session, "text_add", add(json!({"style": {"animation": {"in": {"preset": "bounce", "duration": 9000}}}})));
    assert!(error.message.contains("bounce") && error.message.contains("'in'") && error.message.contains("fade"), "names the preset, the group and the choices: {}", error.message);

    assert_eq!(call_err(&mut session, "text_set", set(json!({"clip": {"track": 0, "clip": 7}, "size": 0.1}))).code, "clip_not_found");
    assert_eq!(call_err(&mut session, "text_set", set(json!({"clip": {"track": 9, "clip": 0}, "size": 0.1}))).code, "track_not_found");
    assert_eq!(call_err(&mut session, "text_set", set(json!({"expected_revision": current - 1, "size": 0.1}))).code, "stale_revision");
    assert_eq!(call_err(&mut session, "text_add", add(json!({"track": 0, "start": 0}))).code, "overlap");
    assert_eq!(project_of(&mut session), known, "a refusal changes nothing");
}

#[test]
fn text_set_takes_a_font_file_that_reads() {
    let Some(arial) = std::env::var_os("WINDIR").map(|dir| std::path::Path::new(&dir).join("Fonts").join("arial.ttf")).filter(|path| path.is_file()) else {
        eprintln!("skipped: no arial.ttf");
        return;
    };
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let current = with_text(&mut session);
    let path = arial.to_string_lossy().into_owned();
    call_ok(&mut session, "text_set", json!({"expected_revision": current, "clip": {"track": 0, "clip": 0}, "font": {"kind": "file", "path": path}}));
    assert_eq!(text_of(&mut session, 0, 0).font, FontRef::File { path });
}

#[test]
fn text_set_refuses_a_clip_that_is_not_text_and_text_add_an_audio_track() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let (video, _) = clip_paths();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    call_ok(&mut session, "track_add", json!({"expected_revision": 1, "kind": "audio"}));
    let added = call_ok(&mut session, "clip_add", json!({"expected_revision": 2, "track": 0, "source": video, "start": 0, "duration": SECOND}));
    let error = call_err(&mut session, "text_set", json!({"expected_revision": revision(&added), "clip": {"track": 0, "clip": 0}, "size": 0.1}));
    assert_eq!(error.code, "not_text");
    assert_ne!(call_err(&mut session, "text_add", json!({"expected_revision": revision(&added), "track": 1, "start": 0, "duration": SECOND, "content": "x"})).code, "invalid_params", "an audio track is the engine's refusal");
}

#[test]
fn font_list_puts_the_default_font_first_and_says_while_the_fonts_are_still_being_read() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let deadline = Instant::now() + Duration::from_secs(60);
    let list = loop {
        let list = call_ok(&mut session, "font_list", json!({}));
        assert_eq!(list["fonts"][0], json!({"kind": "default"}), "{list}");
        match list["state"].as_str() {
            Some("ready") => break list,
            Some("loading") => assert!(Instant::now() < deadline, "the font scan never ended"),
            other => panic!("unknown state {other:?}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let fonts = list["fonts"].as_array().unwrap();
    assert!(fonts[1..].iter().all(|font| font["kind"] == "system" && font["family"].is_string()), "{list}");
    if let Some(font) = fonts.get(1) {
        // What the list names is what a text can use.
        let family = font["family"].as_str().unwrap();
        let used = call_ok(&mut session, "text_add", json!({"expected_revision": 0, "start": 0, "duration": SECOND, "content": "x", "style": {"font": font}}));
        assert_eq!(text_of(&mut session, 0, 0).font, FontRef::System { family: family.to_owned() });
        assert_eq!(revision(&used), 1);
    }
    let missing = call_err(&mut session, "text_add", json!({"expected_revision": if fonts.len() > 1 { 1 } else { 0 }, "start": 5 * SECOND, "duration": SECOND, "content": "x", "style": {"font": {"kind": "system", "family": "No Such Family 0xBEEF"}}}));
    assert_eq!(missing.code, "invalid_params", "the scan is over, so a family it did not find is refused");
}

#[test]
fn text_animation_list_names_the_presets_of_each_group() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let list = call_ok(&mut session, "text_animation_list", json!({}));
    for group in [AnimationGroup::Enter, AnimationGroup::Exit, AnimationGroup::Repeat] {
        let ids: Vec<&str> = list[group.name()].as_array().unwrap_or_else(|| panic!("no group {}: {list}", group.name())).iter().map(|id| id.as_str().unwrap()).collect();
        assert_eq!(ids, gpufx_editor::text_clip::presets_of(group), "{}", group.name());
        assert!(!ids.is_empty());
    }
    assert!(list["in"].as_array().unwrap().iter().any(|id| id == "fade"));
    assert!(list["loop"].as_array().unwrap().iter().any(|id| id == "bounce"));
    assert!(!list["in"].as_array().unwrap().iter().any(|id| id == "bounce"));
}

#[test]
fn the_schemas_of_the_text_tools_name_every_field_an_agent_may_give() {
    let tools = gpufx_mcp::tools::all_tools();
    let schema = |name: &str| Value::Object(tools.iter().find(|tool| tool.name == name).unwrap().tool().input_schema.as_ref().clone());
    let set = schema("text_set");
    for field in ["expected_revision", "clip", "content", "font", "size", "color", "outline", "background", "shadow", "align", "line_spacing", "animation"] {
        assert!(set["properties"][field].is_object(), "text_set has no {field}: {set}");
    }
    let add = schema("text_add");
    assert!(add["required"].as_array().unwrap().iter().all(|name| ["expected_revision", "start", "duration", "content"].contains(&name.as_str().unwrap())), "track and style are optional: {add}");
    let animation = add.to_string();
    assert!(animation.contains("\"in\"") && animation.contains("\"out\"") && animation.contains("\"loop\""), "{add}");
}
