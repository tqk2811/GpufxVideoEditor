mod common;

use common::*;
use engine::{animation_pose, layers_at_frame, trim_start, validate, AcceptAll, AnimationGroup, Clip, ClipClock, ClipKey, ClipTransform, DecoderSpec, LayerContent, Project, TextAnimation, TextAnimationRef, TextContent, Time, Track, TrackKind, ValidationError, TEXT_ANIMATION_PRESETS, TEXT_SOURCE};

const KEY: ClipKey = ClipKey { track: 0, clip: 0 };
const FRAME: i64 = 3003;
const ASPECT: f64 = 16.0 / 9.0;

fn animated(start: i64, duration: i64, animation: TextAnimation) -> Clip {
    let mut text = TextContent::new("Hi", 0.08);
    text.animation = animation;
    Clip { source: TEXT_SOURCE.to_owned(), text: Some(text), ..clip(TEXT_SOURCE, start, 0, duration) }
}

fn reference(preset: &str, ticks: i64) -> Option<TextAnimationRef> {
    Some(TextAnimationRef { preset: preset.into(), duration: t(ticks) })
}

fn project_with(clips: Vec<Clip>) -> Project {
    let mut p = project(FRAME, vec![]);
    p.version = engine::SUPPORTED_VERSION;
    p.tracks.push(Track { kind: TrackKind::Video, clips, transitions: vec![], locked: false, hidden: false });
    p
}

fn transform_at_frame(p: &Project, frame: i64) -> ClipTransform {
    let layers = layers_at_frame(p, frame);
    let LayerContent::Clip(sample) = &layers[0].content else { panic!("one clip: {layers:?}") };
    sample.transform
}

fn clock(elapsed: i64, duration: i64) -> ClipClock {
    ClipClock { elapsed: t(elapsed), duration: t(duration) }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn fade_in_starts_clear_and_ends_opaque() {
    let p = project_with(vec![animated(0, 30 * FRAME, TextAnimation { enter: reference("fade", 3 * FRAME), ..Default::default() })]);
    assert!(close(transform_at_frame(&p, 0).opacity, 0.0));
    assert!(close(transform_at_frame(&p, 1).opacity, 1.0 / 3.0), "linear fade, one frame of three");
    assert!(close(transform_at_frame(&p, 3).opacity, 1.0));
    assert!(close(transform_at_frame(&p, 20).opacity, 1.0));
}

#[test]
fn fade_out_mirrors_fade_in_at_the_end() {
    let total = 30 * FRAME;
    let enter = TextAnimation { enter: reference("fade", 3 * FRAME), ..Default::default() };
    let exit = TextAnimation { exit: reference("fade", 3 * FRAME), ..Default::default() };
    for elapsed in [0, FRAME, 2 * FRAME, 3 * FRAME] {
        let going = animation_pose(&exit, clock(total - elapsed, total));
        let coming = animation_pose(&enter, clock(elapsed, total));
        assert!(close(going.opacity, coming.opacity), "{elapsed}: {going:?} vs {coming:?}");
    }
    assert!(close(animation_pose(&exit, clock(total - 3 * FRAME, total)).opacity, 1.0));
    let p = project_with(vec![animated(0, total, exit)]);
    assert!(close(transform_at_frame(&p, 29).opacity, 1.0 / 3.0), "two frames into a fade of three");
}

#[test]
fn a_loop_repeats_every_duration() {
    for name in ["bounce", "pulse", "shake"] {
        let animation = TextAnimation { repeat: reference(name, 9000), ..Default::default() };
        let mut moved = false;
        for elapsed in (0..9000).step_by(750) {
            let first = animation_pose(&animation, clock(elapsed, 90_000));
            let later = animation_pose(&animation, clock(elapsed + 3 * 9000, 90_000));
            assert!(close(first.dx, later.dx) && close(first.dy, later.dy) && close(first.scale, later.scale), "{name} at {elapsed}");
            moved |= first != engine::AnimationPose::IDENTITY;
        }
        assert!(moved, "{name} does something");
    }
}

#[test]
fn slides_move_the_way_they_are_named() {
    let total = 30 * FRAME;
    let settled = |p: &Project| transform_at_frame(p, 15).rect(ASPECT, 2.0);
    for (name, sign_x, sign_y) in [("slide-left", -1.0, 0.0), ("slide-right", 1.0, 0.0), ("slide-up", 0.0, -1.0), ("slide-down", 0.0, 1.0)] {
        let p = project_with(vec![animated(0, total, TextAnimation { enter: reference(name, 3 * FRAME), exit: reference(name, 3 * FRAME), ..Default::default() })]);
        let rest = settled(&p);
        let first = transform_at_frame(&p, 0).rect(ASPECT, 2.0);
        let last = transform_at_frame(&p, 29).rect(ASPECT, 2.0);
        // Coming in it moves towards `sign`, so it starts on the other side; going out it carries on.
        assert!((rest.x - first.x) * sign_x >= 0.0 && (rest.y - first.y) * sign_y >= 0.0, "{name} in: {first:?} -> {rest:?}");
        assert!((last.x - rest.x) * sign_x >= 0.0 && (last.y - rest.y) * sign_y >= 0.0, "{name} out: {rest:?} -> {last:?}");
        assert!((first.x - rest.x).abs() + (first.y - rest.y).abs() > 0.1, "{name} moved");
        assert!(close(first.width, rest.width), "{name} keeps its size");
    }
}

#[test]
fn a_short_clip_shrinks_enter_and_exit_alike() {
    let animation = TextAnimation { enter: reference("fade", 9000), exit: reference("fade", 27_000), ..Default::default() };
    // 36000 ticks of animation in 18000 visible: both halve, the enter to 4500 and the exit to 13500.
    assert!(close(animation_pose(&animation, clock(0, 18_000)).opacity, 0.0));
    assert!(close(animation_pose(&animation, clock(2250, 18_000)).opacity, 0.5));
    assert!(close(animation_pose(&animation, clock(4500, 18_000)).opacity, 1.0), "enter done, exit about to start");
    assert!(close(animation_pose(&animation, clock(11_250, 18_000)).opacity, 0.5));
    assert!(close(animation_pose(&animation, clock(18_000, 18_000)).opacity, 0.0));
}

#[test]
fn an_unknown_preset_does_nothing_and_validation_names_it() {
    let animation = TextAnimation { enter: reference("wobble", 9000), exit: reference("fade", 0), repeat: reference("fade", 9000) };
    assert_eq!(animation_pose(&animation, clock(100, 90_000)), engine::AnimationPose::IDENTITY);
    let p = project_with(vec![animated(0, 90_000, animation)]);
    assert!(close(transform_at_frame(&p, 0).opacity, 1.0));
    let errors = validate(&p, &AcceptAll);
    assert!(errors.iter().any(|e| matches!(e, ValidationError::UnknownTextAnimation { group: "in", preset, .. } if preset == "wobble")), "{errors:?}");
    assert!(errors.iter().any(|e| matches!(e, ValidationError::UnknownTextAnimation { group: "loop", preset, .. } if preset == "fade")), "fade does not loop: {errors:?}");
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidTextAnimationDuration { group: "out", .. })), "{errors:?}");
    let fine = project_with(vec![animated(0, 90_000, TextAnimation { enter: reference("pop", 9000), exit: reference("spin", 9000), repeat: reference("pulse", 9000) })]);
    assert_eq!(validate(&fine, &AcceptAll), Vec::new());
}

#[test]
fn enter_runs_from_the_first_frame_shown_after_trimming_the_start() {
    let p = project_with(vec![animated(0, 30 * FRAME, TextAnimation { enter: reference("fade", 3 * FRAME), ..Default::default() })]);
    let trimmed = trim_start(&p, KEY, t(9 * FRAME)).unwrap();
    assert!(close(transform_at_frame(&trimmed, 9).opacity, 0.0));
    assert!(close(transform_at_frame(&trimmed, 12).opacity, 1.0));
}

#[test]
fn presets_keep_the_decoder_and_only_turning_ones_rotate() {
    let p = project_with(vec![animated(0, 30 * FRAME, TextAnimation { enter: reference("pop", 6 * FRAME), repeat: reference("pulse", 9000), ..Default::default() })]);
    let specs: Vec<DecoderSpec> = (0..30)
        .map(|frame| {
            let layers = layers_at_frame(&p, frame);
            let LayerContent::Clip(sample) = &layers[0].content else { panic!() };
            assert!(!sample.transform.rotates, "pop and pulse do not turn");
            DecoderSpec::of(sample.model)
        })
        .collect();
    assert!(specs.windows(2).all(|pair| pair[0] == pair[1]), "the same picture at every frame");
    assert!((0..30).map(|frame| transform_at_frame(&p, frame).zoom).any(|zoom| zoom > 1.0), "pop and pulse grow");
    let spin = animated(0, 90_000, TextAnimation { enter: reference("spin", 9000), ..Default::default() });
    assert!(spin.transform_at(Time::ZERO).rotates);
    assert!(spin.transform_at(t(50_000)).rotates, "drawn as turning even once settled");
}

#[test]
fn the_widest_counts_the_largest_scale_of_the_presets() {
    let plain = animated(0, 90_000, TextAnimation::default());
    let popping = animated(0, 90_000, TextAnimation { enter: reference("pop", 9000), repeat: reference("pulse", 9000), ..Default::default() });
    let base = plain.widest(ASPECT, 2.0);
    let peak = TEXT_ANIMATION_PRESETS.iter().find(|p| p.id == "pop" && p.group == AnimationGroup::Enter).unwrap().max_scale
        * TEXT_ANIMATION_PRESETS.iter().find(|p| p.id == "pulse").unwrap().max_scale;
    assert!(close(popping.widest(ASPECT, 2.0), base * peak));
    let fading = animated(0, 90_000, TextAnimation { enter: reference("fade", 9000), ..Default::default() });
    assert!(close(fading.widest(ASPECT, 2.0), base));
}
