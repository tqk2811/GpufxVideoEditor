//! Faces found frame by frame, joined into face tracks for the automatic face censor.
//!
//! A face detector (YuNet, in the editor) gives boxes per frame with no identity. [`face_tracks`]
//! joins them into one path per face: each frame's boxes go to the open paths by the largest
//! overlap (IoU) first, a box no path takes starts a new path, and a path that misses more than
//! [`FaceTrackOptions::max_gap`] frames in a row ends. Short paths and paths of low scores are
//! dropped as noise. Each path is a list of [`TrackSample`]s (confidence is the detector's score),
//! ready to become a [`crate::MotionTrack`].

use crate::{Time, TrackSample};

/// A face in one frame: middle and size in parts of the picture width and height, and the
/// detector's score, 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceDetection {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub score: f64,
}

impl FaceDetection {
    /// Intersection over union with `other`; 0 when either box is empty. The same in parts of the
    /// picture as in pixels, as it does not change when an axis is scaled.
    pub fn iou(&self, other: &FaceDetection) -> f64 {
        let ix = ((self.cx + self.w / 2.0).min(other.cx + other.w / 2.0) - (self.cx - self.w / 2.0).max(other.cx - other.w / 2.0)).max(0.0);
        let iy = ((self.cy + self.h / 2.0).min(other.cy + other.h / 2.0) - (self.cy - self.h / 2.0).max(other.cy - other.h / 2.0)).max(0.0);
        let inter = ix * iy;
        let union = self.w * self.h + other.w * other.h - inter;
        if union > 0.0 { inter / union } else { 0.0 }
    }
}

/// The faces of one frame of the source.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FaceFrame {
    /// Time in the source file.
    pub source_time: Time,
    pub faces: Vec<FaceDetection>,
}

/// How faces are joined into tracks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceTrackOptions {
    /// The least overlap of a box with the last box of a path to go on it.
    pub min_iou: f64,
    /// The most frames in a row a path may miss and still go on.
    pub max_gap: usize,
    /// The fewest boxes of a path that is kept.
    pub min_frames: usize,
    /// The lowest mean score of a path that is kept.
    pub min_mean_score: f64,
}

impl Default for FaceTrackOptions {
    fn default() -> Self {
        FaceTrackOptions { min_iou: 0.3, max_gap: 5, min_frames: 5, min_mean_score: 0.7 }
    }
}

struct Path {
    samples: Vec<TrackSample>,
    last: FaceDetection,
    missed: usize,
}

fn sample(at: Time, face: &FaceDetection) -> TrackSample {
    TrackSample { source_time: at, cx: face.cx, cy: face.cy, w: face.w, h: face.h, angle: 0.0, confidence: face.score.clamp(0.0, 1.0), manual: false, quad: None }
}

/// The face tracks of `frames` (in source time order), longest first: each a list of samples
/// sorted by time, at most one per frame.
pub fn face_tracks(frames: &[FaceFrame], options: FaceTrackOptions) -> Vec<Vec<TrackSample>> {
    let mut open: Vec<Path> = Vec::new();
    let mut done: Vec<Path> = Vec::new();
    for frame in frames {
        let faces: Vec<&FaceDetection> = frame.faces.iter().filter(|f| f.w > 0.0 && f.h > 0.0 && f.score.is_finite()).collect();
        // Every pair close enough, the largest overlap first.
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for (p, path) in open.iter().enumerate() {
            for (f, face) in faces.iter().enumerate() {
                let iou = path.last.iou(face);
                if iou >= options.min_iou {
                    pairs.push((iou, p, f));
                }
            }
        }
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let (mut path_taken, mut face_taken) = (vec![false; open.len()], vec![false; faces.len()]);
        for (_, p, f) in pairs {
            if path_taken[p] || face_taken[f] {
                continue;
            }
            path_taken[p] = true;
            face_taken[f] = true;
            let path = &mut open[p];
            path.samples.push(sample(frame.source_time, faces[f]));
            path.last = *faces[f];
            path.missed = 0;
        }
        for (path, taken) in open.iter_mut().zip(&path_taken) {
            if !taken {
                path.missed += 1;
            }
        }
        let (going, ended): (Vec<Path>, Vec<Path>) = open.into_iter().partition(|path| path.missed <= options.max_gap);
        open = going;
        done.extend(ended);
        for (f, face) in faces.iter().enumerate() {
            if !face_taken[f] {
                open.push(Path { samples: vec![sample(frame.source_time, face)], last: **face, missed: 0 });
            }
        }
    }
    done.extend(open);
    let mut tracks: Vec<Vec<TrackSample>> = done
        .into_iter()
        .map(|path| path.samples)
        .filter(|samples| {
            let mean = samples.iter().map(|s| s.confidence).sum::<f64>() / samples.len().max(1) as f64;
            samples.len() >= options.min_frames && mean >= options.min_mean_score
        })
        .collect();
    tracks.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].source_time.cmp(&b[0].source_time)));
    tracks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(cx: f64, cy: f64, size: f64, score: f64) -> FaceDetection {
        FaceDetection { cx, cy, w: size, h: size, score }
    }

    fn frames(count: usize, at: impl Fn(usize) -> Vec<FaceDetection>) -> Vec<FaceFrame> {
        (0..count).map(|i| FaceFrame { source_time: Time::from_ticks(i as i64 * 1000), faces: at(i) }).collect()
    }

    #[test]
    fn iou_of_boxes() {
        assert!((face(0.5, 0.5, 0.2, 1.0).iou(&face(0.5, 0.5, 0.2, 1.0)) - 1.0).abs() < 1e-9);
        assert_eq!(face(0.2, 0.5, 0.2, 1.0).iou(&face(0.8, 0.5, 0.2, 1.0)), 0.0);
        let half = face(0.5, 0.5, 0.2, 1.0).iou(&FaceDetection { cx: 0.6, ..face(0.5, 0.5, 0.2, 1.0) });
        assert!((half - 1.0 / 3.0).abs() < 1e-9, "{half}");
    }

    #[test]
    fn two_moving_faces_make_two_tracks() {
        // one face goes right, one goes left; they never overlap and are found in a shuffled order
        let found = frames(20, |i| {
            let a = face(0.1 + 0.01 * i as f64, 0.3, 0.1, 0.9);
            let b = face(0.9 - 0.01 * i as f64, 0.7, 0.12, 0.95);
            if i % 2 == 0 { vec![a, b] } else { vec![b, a] }
        });
        let tracks = face_tracks(&found, FaceTrackOptions::default());
        assert_eq!(tracks.len(), 2);
        for track in &tracks {
            assert_eq!(track.len(), 20);
            let going_right = track[0].cy < 0.5;
            for (i, s) in track.iter().enumerate() {
                let want = if going_right { 0.1 + 0.01 * i as f64 } else { 0.9 - 0.01 * i as f64 };
                assert!((s.cx - want).abs() < 1e-9 && s.source_time == Time::from_ticks(i as i64 * 1000) && !s.manual);
            }
        }
    }

    #[test]
    fn a_short_gap_keeps_the_track_and_a_long_one_splits_it() {
        let options = FaceTrackOptions::default();
        let gap = |from: usize, to: usize| frames(30, move |i| if (from..to).contains(&i) { vec![] } else { vec![face(0.5, 0.5, 0.2, 0.9)] });
        let short = face_tracks(&gap(10, 10 + options.max_gap), options);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].len(), 30 - options.max_gap);
        let long = face_tracks(&gap(10, 11 + options.max_gap), options);
        assert_eq!(long.len(), 2, "a gap longer than max_gap ends the track");
    }

    #[test]
    fn noise_is_dropped() {
        let options = FaceTrackOptions::default();
        // a face in every frame, a flicker of 3 frames, and a long run of low scores
        let found = frames(20, |i| {
            let mut faces = vec![face(0.3, 0.3, 0.1, 0.9)];
            if (5..8).contains(&i) {
                faces.push(face(0.8, 0.2, 0.05, 0.95));
            }
            faces.push(face(0.7, 0.8, 0.1, 0.5));
            faces
        });
        let tracks = face_tracks(&found, options);
        assert_eq!(tracks.len(), 1);
        assert!((tracks[0][0].cx - 0.3).abs() < 1e-9);
        assert!(face_tracks(&[], options).is_empty());
    }

    #[test]
    fn the_largest_overlap_wins_when_two_faces_meet() {
        // two faces side by side; the left one shifts a little each frame
        let found = frames(10, |i| vec![face(0.40 + 0.002 * i as f64, 0.5, 0.1, 0.9), face(0.52, 0.5, 0.1, 0.9)]);
        let tracks = face_tracks(&found, FaceTrackOptions::default());
        assert_eq!(tracks.len(), 2);
        let left = tracks.iter().find(|t| t[0].cx < 0.45).unwrap();
        assert!(left.iter().all(|s| s.cx < 0.5), "the left track stays on the left face");
    }
}
