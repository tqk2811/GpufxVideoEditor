//! Opt-in D3D12 validation (`POC_D3D12_DEBUG=1`): collects the debug layer's messages by ID.
//!
//! Numbers measured with this switch on are not benchmark numbers.

use std::collections::BTreeMap;
use std::ffi::CStr;

use serde_json::{json, Value};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12Device, ID3D12InfoQueue, D3D12_MESSAGE, D3D12_MESSAGE_SEVERITY, D3D12_MESSAGE_SEVERITY_CORRUPTION, D3D12_MESSAGE_SEVERITY_ERROR,
    D3D12_MESSAGE_SEVERITY_INFO, D3D12_MESSAGE_SEVERITY_MESSAGE, D3D12_MESSAGE_SEVERITY_WARNING,
};

const SWITCH: &str = "POC_D3D12_DEBUG";
/// Far more than one step of the benchmark produces, so nothing is dropped between drains.
const STORED_MESSAGE_LIMIT: u64 = 1 << 20;

pub fn is_requested() -> bool {
    std::env::var(SWITCH).is_ok_and(|value| value == "1")
}

fn severity_name(severity: D3D12_MESSAGE_SEVERITY) -> &'static str {
    match severity {
        D3D12_MESSAGE_SEVERITY_CORRUPTION => "corruption",
        D3D12_MESSAGE_SEVERITY_ERROR => "error",
        D3D12_MESSAGE_SEVERITY_WARNING => "warning",
        D3D12_MESSAGE_SEVERITY_INFO => "info",
        D3D12_MESSAGE_SEVERITY_MESSAGE => "message",
        _ => "unknown",
    }
}

struct Entry {
    count: u64,
    sample: String,
}

pub struct ValidationLog {
    queue: ID3D12InfoQueue,
    entries: BTreeMap<(&'static str, i32), Entry>,
}

impl ValidationLog {
    pub fn attach(device: &ID3D12Device) -> Result<Self, String> {
        let queue: ID3D12InfoQueue = device.cast().map_err(|e| format!("{SWITCH}=1 but the device has no ID3D12InfoQueue: {e}"))?;
        // SAFETY: plain COM call on a live object.
        unsafe { queue.SetMessageCountLimit(STORED_MESSAGE_LIMIT) }.map_err(|e| format!("SetMessageCountLimit failed: {e}"))?;
        Ok(Self { queue, entries: BTreeMap::new() })
    }

    /// Moves the stored messages into the per-ID counts.
    pub fn drain(&mut self) -> Result<(), String> {
        // SAFETY: the first GetMessage reports the size, the second fills a buffer of that size,
        // aligned for D3D12_MESSAGE; pDescription points into the same buffer.
        unsafe {
            for index in 0..self.queue.GetNumStoredMessages() {
                let mut size = 0usize;
                self.queue.GetMessage(index, None, &mut size).map_err(|e| format!("GetMessage size failed: {e}"))?;
                let mut buffer = vec![0u64; size.div_ceil(size_of::<u64>())];
                let message = buffer.as_mut_ptr().cast::<D3D12_MESSAGE>();
                self.queue.GetMessage(index, Some(message), &mut size).map_err(|e| format!("GetMessage failed: {e}"))?;
                let key = (severity_name((*message).Severity), (*message).ID.0);
                let entry = self.entries.entry(key).or_insert_with(|| Entry {
                    count: 0,
                    sample: CStr::from_ptr((*message).pDescription.cast()).to_string_lossy().into_owned(),
                });
                entry.count += 1;
            }
            self.queue.ClearStoredMessages();
        }
        Ok(())
    }

    pub fn summary(&self) -> Value {
        // SAFETY: plain COM call on a live object.
        let discarded = unsafe { self.queue.GetNumMessagesDiscardedByMessageCountLimit() };
        let messages: Vec<Value> = self
            .entries
            .iter()
            .map(|((severity, id), entry)| json!({ "severity": severity, "id": id, "count": entry.count, "sample": entry.sample }))
            .collect();
        json!({ "discarded_by_limit": discarded, "distinct": messages.len(), "messages": messages })
    }
}
