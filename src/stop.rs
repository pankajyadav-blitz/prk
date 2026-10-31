//! A stop flag whose waits a signal can cut short.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
pub struct Stop {
    set: Mutex<bool>,
    cv: Condvar,
}

impl Stop {
    pub fn set(&self) {
        *self.set.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }

    pub fn is_set(&self) -> bool {
        *self.set.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait up to `seconds`; true once stopped.
    pub fn wait(&self, seconds: f64) -> bool {
        let guard = self.set.lock().unwrap_or_else(|e| e.into_inner());
        if *guard {
            return true;
        }
        let (guard, _) = self
            .cv
            .wait_timeout(guard, Duration::from_secs_f64(seconds.max(0.0)))
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    pub fn wait_forever(&self) {
        let mut guard = self.set.lock().unwrap_or_else(|e| e.into_inner());
        while !*guard {
            guard = self.cv.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }
}
