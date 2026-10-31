//! Live state the dashboard draws and the database does not hold: which step the
//! running review is on, whether Slack is connected, whether the queue is
//! paused, and the recent log lines (which go here instead of stderr while the
//! full-screen UI owns the terminal).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use crate::db::now;

const LOG_LINES: usize = 200;

#[derive(Default)]
struct Inner {
    job_id: i64,
    step: String,
    step_since: i64,
    connected: bool,
    paused: bool,
    pause_until: i64,
    pause_reason: String,
    caught_up: Option<Result<usize, String>>,
    started: i64,
    tui: bool,
    log: VecDeque<(i64, &'static str, String)>,
}

pub struct Activity {
    inner: Mutex<Inner>,
}

pub fn get() -> &'static Activity {
    static ACTIVITY: OnceLock<Activity> = OnceLock::new();
    ACTIVITY.get_or_init(|| Activity {
        inner: Mutex::new(Inner {
            started: now(),
            ..Default::default()
        }),
    })
}

impl Activity {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_step(&self, job_id: i64, step: &str) {
        let mut a = self.lock();
        a.job_id = job_id;
        a.step = step.to_string();
        a.step_since = now();
    }

    pub fn clear_step(&self) {
        let mut a = self.lock();
        a.job_id = 0;
        a.step.clear();
    }

    /// (job id, step, since) of the review in progress, if any.
    pub fn step(&self) -> Option<(i64, String, i64)> {
        let a = self.lock();
        (a.job_id != 0).then(|| (a.job_id, a.step.clone(), a.step_since))
    }

    pub fn set_connected(&self, connected: bool) {
        self.lock().connected = connected;
    }

    pub fn connected(&self) -> bool {
        self.lock().connected
    }

    pub fn toggle_pause(&self) -> bool {
        let mut a = self.lock();
        a.paused = !a.paused;
        a.paused
    }

    pub fn paused(&self) -> bool {
        self.lock().paused
    }

    /// Hold the queue until `until` (Claude usage limit).
    pub fn pause_until(&self, until: i64, reason: &str) {
        let mut a = self.lock();
        a.pause_until = until;
        a.pause_reason = reason.to_string();
    }

    /// The usage-limit hold still in force: (until, reason).
    pub fn held(&self) -> Option<(i64, String)> {
        let a = self.lock();
        (a.pause_until > now()).then(|| (a.pause_until, a.pause_reason.clone()))
    }

    pub fn set_caught_up(&self, result: Result<usize, String>) {
        self.lock().caught_up = Some(result);
    }

    pub fn caught_up(&self) -> Option<Result<usize, String>> {
        self.lock().caught_up.clone()
    }

    pub fn started(&self) -> i64 {
        self.lock().started
    }

    pub fn set_tui(&self, on: bool) {
        self.lock().tui = on;
    }

    pub fn tui(&self) -> bool {
        self.lock().tui
    }

    pub fn push_log(&self, level: &'static str, message: &str) {
        let mut a = self.lock();
        if a.log.len() == LOG_LINES {
            a.log.pop_front();
        }
        a.log.push_back((now(), level, message.to_string()));
    }

    /// The newest `n` log lines, oldest first.
    pub fn logs(&self, n: usize) -> Vec<(i64, &'static str, String)> {
        let a = self.lock();
        a.log
            .iter()
            .skip(a.log.len().saturating_sub(n))
            .cloned()
            .collect()
    }
}
