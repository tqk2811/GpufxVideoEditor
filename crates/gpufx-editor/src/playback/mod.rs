//! Playback: a preparing thread mixes ahead into the audio ring, the device callback reads it, and
//! the picture follows the audio clock. `plan` and `thread` are tested without an audio device.

pub mod device;
pub mod plan;
pub mod preview;
pub mod thread;
