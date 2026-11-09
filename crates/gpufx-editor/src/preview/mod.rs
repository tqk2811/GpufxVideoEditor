//! The preview: a worker thread composites the frame the UI wants most recently, and the UI shows
//! the latest finished one. `slot`, `plan` and `worker` are pure and tested without a GPU.

pub mod backend;
pub mod plan;
pub mod slot;
pub mod worker;
