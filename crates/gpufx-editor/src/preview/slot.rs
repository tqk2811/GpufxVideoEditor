//! A one-value mailbox: a new value replaces the one waiting, so stale work is dropped, not queued.

use std::sync::{Condvar, Mutex, MutexGuard};

struct State<T> {
    value: Option<T>,
    closed: bool,
}

pub struct LatestSlot<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self { state: Mutex::new(State { value: None, closed: false }), ready: Condvar::new() }
    }
}

impl<T> LatestSlot<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Stores `value` and returns the unread value it replaced.
    pub fn put(&self, value: T) -> Option<T> {
        let replaced = self.lock().value.replace(value);
        self.ready.notify_one();
        replaced
    }

    /// Takes the waiting value without blocking.
    pub fn take(&self) -> Option<T> {
        self.lock().value.take()
    }

    /// Blocks until a value is waiting. `None` once the slot is closed.
    pub fn wait(&self) -> Option<T> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(value) = state.value.take() {
                return Some(value);
            }
            state = self.ready.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Wakes every waiter; waits return `None` from now on.
    pub fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
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
    fn close_wakes_a_waiter() {
        let slot: Arc<LatestSlot<i32>> = Arc::new(LatestSlot::default());
        let waiter = slot.clone();
        let handle = thread::spawn(move || waiter.wait());
        slot.close();
        assert_eq!(handle.join().unwrap(), None);
    }
}
