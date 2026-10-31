//! One-line, timestamped log to stderr; debug lines only with --verbose. While
//! the dashboard owns the terminal, lines go to its log panel instead.

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn emit(level: &'static str, message: &str) {
    let activity = crate::activity::get();
    if activity.tui() {
        activity.push_log(level, message);
        return;
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    eprintln!("{now} {level:<5} {message}");
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::log::verbose() {
            $crate::log::emit("DEBUG", &format!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::emit("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::emit("WARN", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::emit("ERROR", &format!($($arg)*)) };
}
