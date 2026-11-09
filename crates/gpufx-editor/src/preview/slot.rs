//! A one-value mailbox: a new value replaces the one waiting, so stale work is dropped, not queued.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

struct State<T> {
    value: Option<T>,
    closed: bool,
}

pub struct LatestSlot<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
    /// True while a value waits and once the slot is closed.
    pending: Arc<AtomicBool>,
}

impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self { state: Mutex::new(State { value: None, closed: false }), ready: Condvar::new(), pending: Arc::default() }
    }
}

impl<T> LatestSlot<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Stores `value` and returns the unread value it replaced.
    pub fn put(&self, value: T) -> Option<T> {
        let replaced = {
            let mut state = self.lock();
            self.pending.store(true, Ordering::SeqCst);
            state.value.replace(value)
        };
        self.ready.notify_one();
        replaced
    }

    /// Takes the waiting value without blocking.
    pub fn take(&self) -> Option<T> {
        let mut state = self.lock();
        self.pending.store(state.closed, Ordering::SeqCst);
        state.value.take()
    }

    /// Blocks until a value is waiting. `None` once the slot is closed.
    pub fn wait(&self) -> Option<T> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(value) = state.value.take() {
                self.pending.store(false, Ordering::SeqCst);
                return Some(value);
            }
            state = self.ready.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Wakes every waiter; waits return `None` from now on.
    pub fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        self.pending.store(true, Ordering::SeqCst);
        drop(state);
        self.ready.notify_all();
    }

    /// A flag true while a value waits and once the slot is closed: the holder of the value
    /// being worked on can see that its work is no longer wanted.
    pub fn pending(&self) -> Arc<AtomicBool> {
        self.pending.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn a_new_value_replaces_the_unread_one() {
        let slot = LatestSlot::default();
        assert_eq!(slot.put(1), None);
        assert_eq!(slot.put(2), Some(1));
        assert_eq!(slot.take(), Some(2));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn wait_receives_a_value_from_another_thread() {
        let slot = Arc::new(LatestSlot::default());
        let sender = slot.clone();
        let handle = thread::spawn(move || sender.put(7));
        assert_eq!(slot.wait(), Some(7));
        handle.join().unwrap();
    }

    #[test]
    fn the_pending_flag_follows_the_waiting_value_and_stays_set_after_close() {
        let slot = LatestSlot::default();
        let pending = slot.pending();
        assert!(!pending.load(Ordering::SeqCst));
        slot.put(1);
        assert!(pending.load(Ordering::SeqCst));
        assert_eq!(slot.wait(), Some(1));
        assert!(!pending.load(Ordering::SeqCst), "the value is being worked on, nothing newer waits");
        slot.close();
        assert!(pending.load(Ordering::SeqCst));
        assert_eq!(slot.wait(), None);
        assert!(pending.load(Ordering::SeqCst), "close is permanent");
    }

    #[test]
    fn close_wakes_a_waiter() {
        let slot: Arc<LatestSlot<i32>> = Arc::new(LatestSlot::default());
        let waiter = slot.clone();
        let handle = thread::spawn(move || waiter.wait());
        slot.close();
        assert_eq!(handle.join().unwrap(), None);
    }
}
