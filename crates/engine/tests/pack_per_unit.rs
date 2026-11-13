//! A per-unit preset of a pack, checked like a built-in one. Its own test binary, since the table
//! of pack presets is one for the whole process.

mod common;

use common::*;
use engine::{CurveKey, validate, AcceptAll, AnimationCurve, AnimationGroup, AnimationRef, CurveProperty, Ease, PackPreset, PackPresets, PerUnit, Project, Track, TrackKind, ValidationError, TEXT_SOURCE};

#[test]
fn validate_rejects_a_per_unit_pack_preset_on_a_clip_that_is_not_text() {
    let curve = AnimationCurve::default().with(CurveProperty::Opacity, vec![CurveKey { t: 0.0, value: 0.0, ease: Ease::Linear }, CurveKey { t: 1.0, value: 1.0, ease: Ease::Linear }]);
    let per_unit = PerUnit { stagger: Some(0.5), curve: curve.clone(), ..PerUnit::default() };
    engine::set_pack_presets(PackPresets::new(vec![PackPreset::new("pack/rise".to_owned(), AnimationGroup::Enter, curve).with_per_unit(Some(per_unit))]));
    let mut c = clip("a.png", 0, 0, 9000);
    c.animation.enter = Some(AnimationRef { preset: "pack/rise".into(), duration: t(9000) });
    let mut p: Project = project(3003, vec![]);
    p.version = engine::SUPPORTED_VERSION;
    p.tracks.push(Track { kind: TrackKind::Video, clips: vec![c], transitions: vec![], locked: false, hidden: false, subtitle: None });
    assert_eq!(validate(&p, &AcceptAll), vec![ValidationError::PerUnitAnimationNotText { track: 0, clip: 0, group: "in", preset: "pack/rise".into() }]);
    p.tracks[0].clips[0].source = TEXT_SOURCE.to_owned();
    p.tracks[0].clips[0].text = Some(engine::TextContent::new("Hi", 0.08));
    assert_eq!(validate(&p, &AcceptAll), vec![], "for text it is fine");
    engine::set_pack_presets(PackPresets::default());
    p.tracks[0].clips[0].text = None;
    p.tracks[0].clips[0].source = "a.png".into();
    assert_eq!(validate(&p, &AcceptAll), vec![], "a pack missing on this machine is left out, not refused");
}
