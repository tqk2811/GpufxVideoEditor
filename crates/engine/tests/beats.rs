//! Beat detection on synthetic click tracks: the tempo within 1 BPM and nine beats in ten within
//! 20 ms of a click, at several tempos and with noise under the clicks.

use engine::beats::{detect_beats, downmix_quarter, BeatAnalysis, ANALYSIS_RATE};
use std::time::Instant;

/// A small deterministic noise source, so the tests never depend on luck.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// `seconds` of clicks at `bpm` from `offset` seconds, at `rate`, with white noise of `noise`
/// amplitude under them. A click is 15 ms of a 1.5 kHz tone with a fast decay; every fourth one is
/// louder, like a downbeat. Returns the samples and the click times.
fn click_track(bpm: f64, seconds: f64, offset: f64, rate: u32, noise: f32) -> (Vec<f32>, Vec<f64>) {
    let rate_f = f64::from(rate);
    let mut samples = vec![0.0_f32; (seconds * rate_f) as usize];
    let mut source = Noise(bpm.to_bits());
    for sample in &mut samples {
        *sample = noise * source.next();
    }
    let period = 60.0 / bpm;
    let mut clicks = Vec::new();
    let mut index = 0;
    loop {
        let at = offset + index as f64 * period;
        if at + 0.05 >= seconds {
            break;
        }
        clicks.push(at);
        let loudness = if index % 4 == 0 { 0.9 } else { 0.5 };
        let first = (at * rate_f).round() as usize;
        for n in 0..(0.015 * rate_f) as usize {
            let time = n as f64 / rate_f;
            let value = loudness * (-time / 0.004).exp() * (std::f64::consts::TAU * 1500.0 * time).sin();
            samples[first + n] += value as f32;
        }
        index += 1;
    }
    (samples, clicks)
}

/// The part of `clicks` with a beat within `tolerance` seconds, and the part of the beats with a click.
fn hit_rates(analysis: &BeatAnalysis, clicks: &[f64], tolerance: f64) -> (f64, f64) {
    let near = |time: f64, list: &[f64]| list.iter().any(|other| (other - time).abs() <= tolerance);
    let recall = clicks.iter().filter(|click| near(**click, &analysis.beats)).count() as f64 / clicks.len() as f64;
    let precision = analysis.beats.iter().filter(|beat| near(**beat, clicks)).count() as f64 / analysis.beats.len().max(1) as f64;
    (recall, precision)
}

fn mean_error(analysis: &BeatAnalysis, clicks: &[f64]) -> f64 {
    let errors: Vec<f64> = clicks.iter().filter_map(|click| analysis.beats.iter().map(|beat| beat - click).min_by(|a, b| a.abs().total_cmp(&b.abs()))).collect();
    errors.iter().sum::<f64>() / errors.len() as f64
}

fn check(bpm: f64, noise: f32) {
    let seconds = 30.0;
    let (samples, clicks) = click_track(bpm, seconds, 0.37, ANALYSIS_RATE, noise);
    let started = Instant::now();
    let analysis = detect_beats(&samples, ANALYSIS_RATE, &mut |_| true).expect("not cancelled");
    let elapsed = started.elapsed();
    let (recall, precision) = hit_rates(&analysis, &clicks, 0.020);
    println!(
        "{bpm} BPM, noise {noise}: tempo {:.3}, {} beats for {} clicks, recall {:.3}, precision {:.3}, mean offset {:.1} ms, {:.0} ms for {seconds} s",
        analysis.tempo,
        analysis.beats.len(),
        clicks.len(),
        recall,
        precision,
        mean_error(&analysis, &clicks) * 1000.0,
        elapsed.as_secs_f64() * 1000.0
    );
    assert!((analysis.tempo - bpm).abs() < 1.0, "tempo {} for {bpm}", analysis.tempo);
    assert!(recall >= 0.9, "recall {recall}");
    assert!(precision >= 0.9, "precision {precision}");
    // The loud clicks are every fourth from the first.
    let downbeats: Vec<usize> = analysis.downbeats.iter().enumerate().filter(|(_, down)| **down).map(|(index, _)| index).collect();
    assert!(!downbeats.is_empty() && downbeats.windows(2).all(|pair| pair[1] - pair[0] == 4));
    let first_down = analysis.beats[downbeats[0]];
    assert!(clicks.iter().step_by(4).any(|click| (click - first_down).abs() <= 0.02), "downbeat at {first_down}");
}

#[test]
fn ninety_bpm() {
    check(90.0, 0.02);
}

#[test]
fn one_hundred_twenty_bpm() {
    check(120.0, 0.02);
}

#[test]
fn one_hundred_forty_bpm() {
    check(140.0, 0.02);
}

#[test]
fn an_uneven_tempo_with_more_noise() {
    check(103.5, 0.06);
}

#[test]
fn the_rate_of_the_samples_does_not_matter() {
    // Stereo at four times the analysis rate, decimated as the editor does.
    let (mono, clicks) = click_track(128.0, 20.0, 0.21, ANALYSIS_RATE * 4, 0.02);
    let stereo: Vec<f32> = mono.iter().flat_map(|sample| [*sample, *sample]).collect();
    let mut quarter = Vec::new();
    downmix_quarter(&stereo, &mut quarter);
    for (samples, rate) in [(&quarter, ANALYSIS_RATE), (&mono, ANALYSIS_RATE * 4)] {
        let analysis = detect_beats(samples, rate, &mut |_| true).unwrap();
        let (recall, precision) = hit_rates(&analysis, &clicks, 0.020);
        assert!((analysis.tempo - 128.0).abs() < 1.0 && recall >= 0.9 && precision >= 0.9, "rate {rate}: {} {recall} {precision}", analysis.tempo);
    }
}

#[test]
fn silence_before_and_after_has_no_beats() {
    let (clicks_only, clicks) = click_track(120.0, 20.0, 0.25, ANALYSIS_RATE, 0.0);
    let pad = vec![0.0_f32; ANALYSIS_RATE as usize * 5];
    let samples: Vec<f32> = pad.iter().chain(&clicks_only).chain(&pad).copied().collect();
    let analysis = detect_beats(&samples, ANALYSIS_RATE, &mut |_| true).unwrap();
    let first = analysis.beats.first().copied().unwrap();
    let last = analysis.beats.last().copied().unwrap();
    assert!(first > 5.0 + clicks[0] - 0.03 && last < 5.0 + clicks.last().unwrap() + 0.03, "{first} {last}");
}

#[test]
fn a_clip_of_a_few_seconds_gets_its_tempo() {
    for (bpm, seconds) in [(120.0, 3.0), (100.0, 4.0), (90.0, 5.0)] {
        let (samples, clicks) = click_track(bpm, seconds, 0.2, ANALYSIS_RATE, 0.02);
        let analysis = detect_beats(&samples, ANALYSIS_RATE, &mut |_| true).unwrap();
        assert!((analysis.tempo - bpm).abs() < 1.5, "{bpm} BPM over {seconds} s: tempo {}", analysis.tempo);
        let (recall, _) = hit_rates(&analysis, &clicks, 0.020);
        assert!(recall >= 0.8, "{bpm} BPM over {seconds} s: recall {recall}");
    }
}

#[test]
fn a_sound_shorter_than_the_minimum_has_no_tempo() {
    let (samples, _) = click_track(120.0, engine::beats::MIN_TEMPO_SECONDS * 0.9, 0.1, ANALYSIS_RATE, 0.0);
    assert_eq!(detect_beats(&samples, ANALYSIS_RATE, &mut |_| true).unwrap().tempo, 0.0);
}

#[test]
fn samples_that_are_not_numbers_count_as_silence() {
    let (mut samples, clicks) = click_track(120.0, 12.0, 0.25, ANALYSIS_RATE, 0.0);
    // A broken decoder output: a burst of NaN and infinities between two clicks.
    let at = (3.0 * f64::from(ANALYSIS_RATE)) as usize + 1500;
    for (index, sample) in samples[at..at + 600].iter_mut().enumerate() {
        *sample = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][index % 3];
    }
    let mut stereo = Vec::new();
    for sample in &samples {
        stereo.extend(std::iter::repeat_n(*sample, 8));
    }
    let mut mono = Vec::new();
    downmix_quarter(&stereo, &mut mono);
    assert!(mono.iter().all(|sample| sample.is_finite()), "the downmix lets non-finite samples through");
    let analysis = detect_beats(&mono, ANALYSIS_RATE, &mut |_| true).unwrap();
    assert!((analysis.tempo - 120.0).abs() < 1.0, "tempo {}", analysis.tempo);
    let (recall, _) = hit_rates(&analysis, &clicks, 0.020);
    assert!(recall >= 0.9, "recall {recall}");
}

#[test]
fn silence_and_short_sounds_give_no_beats() {
    let silent = vec![0.0_f32; ANALYSIS_RATE as usize * 10];
    let analysis = detect_beats(&silent, ANALYSIS_RATE, &mut |_| true).unwrap();
    assert!(analysis.beats.is_empty() && analysis.tempo == 0.0);
    assert!(analysis.clip_beats().is_empty());
    let short = vec![0.1_f32; 100];
    assert_eq!(detect_beats(&short, ANALYSIS_RATE, &mut |_| true).unwrap(), BeatAnalysis::default());
}

#[test]
fn a_false_progress_cancels() {
    let (samples, _) = click_track(120.0, 10.0, 0.0, ANALYSIS_RATE, 0.0);
    let mut calls = 0;
    assert_eq!(detect_beats(&samples, ANALYSIS_RATE, &mut |_| { calls += 1; false }), None);
    assert_eq!(calls, 1);
}

#[test]
fn the_beats_become_strict_clip_beats_in_ticks() {
    let (samples, _) = click_track(120.0, 12.0, 0.5, ANALYSIS_RATE, 0.01);
    let analysis = detect_beats(&samples, ANALYSIS_RATE, &mut |_| true).unwrap();
    let beats = analysis.clip_beats();
    assert_eq!(beats.beats.len(), analysis.beats.len());
    assert!(beats.beats.windows(2).all(|pair| pair[0].at < pair[1].at));
    assert!((beats.tempo - 120.0).abs() < 1.0);
    assert_eq!(beats.beats.iter().filter(|beat| beat.downbeat).count(), analysis.downbeats.iter().filter(|down| **down).count());
}

/// Run with `--ignored --nocapture` in release to see the cost per minute of audio.
#[test]
#[ignore]
fn cost_per_minute() {
    let (samples, _) = click_track(120.0, 300.0, 0.1, ANALYSIS_RATE, 0.02);
    let started = Instant::now();
    let analysis = detect_beats(&samples, ANALYSIS_RATE, &mut |_| true).unwrap();
    let per_minute = started.elapsed().as_secs_f64() * 1000.0 / 5.0;
    println!("{:.1} ms per minute of audio ({} beats, tempo {:.2})", per_minute, analysis.beats.len(), analysis.tempo);
}

