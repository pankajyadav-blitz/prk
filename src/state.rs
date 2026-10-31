//! Which (PR, head commit) pairs have been reviewed, so a repeat tag on an
//! unchanged PR is skipped and a new push is reviewed again.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, ErrorCode, params};

pub struct State {
    db: Mutex<Connection>,
}

impl State {
    pub fn open(path: &Path) -> Result<State, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let db = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS reviews (
               pr     TEXT NOT NULL,
               head   TEXT NOT NULL,
               status TEXT NOT NULL DEFAULT 'running',
               at     TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
               PRIMARY KEY (pr, head));",
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        // A review still marked running was cut off by a crash or restart, so it
        // never finished; let the next tag try it again.
        db.execute("DELETE FROM reviews WHERE status = 'running'", [])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(State { db: Mutex::new(db) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Mark (pr, head) as being reviewed. False when it already was.
    pub fn claim(&self, pr: &str, head: &str) -> Result<bool, String> {
        match self.conn().execute(
            "INSERT INTO reviews (pr, head) VALUES (?1, ?2)",
            params![pr, head],
        ) {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ErrorCode::ConstraintViolation =>
            {
                Ok(false)
            }
            Err(e) => Err(format!("state: claim {pr}@{head}: {e}")),
        }
    }

    pub fn finish(&self, pr: &str, head: &str) {
        if let Err(e) = self.conn().execute(
            "UPDATE reviews SET status = 'done', at = CURRENT_TIMESTAMP WHERE pr = ?1 AND head = ?2",
            params![pr, head],
        ) {
            crate::error!("state: finish {pr}@{head}: {e}");
        }
    }

    /// Forget a claim so the next tag retries it (used when a review fails).
    pub fn release(&self, pr: &str, head: &str) {
        if let Err(e) = self.conn().execute(
            "DELETE FROM reviews WHERE pr = ?1 AND head = ?2",
            params![pr, head],
        ) {
            crate::error!("state: release {pr}@{head}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("prk-{}-{name}", std::process::id()))
            .join("state.sqlite3");
        std::fs::remove_file(&path).ok();
        path
    }

    #[test]
    fn a_head_is_claimed_once_and_a_new_head_is_new() {
        let state = State::open(&temp("claim")).unwrap();
        assert!(state.claim("o/r#1", "aaa").unwrap());
        assert!(!state.claim("o/r#1", "aaa").unwrap());
        assert!(state.claim("o/r#1", "bbb").unwrap());
        state.release("o/r#1", "aaa");
        assert!(state.claim("o/r#1", "aaa").unwrap());
    }

    #[test]
    fn unfinished_reviews_are_dropped_on_reopen_and_finished_ones_kept() {
        let path = temp("reopen");
        {
            let state = State::open(&path).unwrap();
            state.claim("o/r#1", "aaa").unwrap();
            state.claim("o/r#2", "bbb").unwrap();
            state.finish("o/r#2", "bbb");
        }
        let state = State::open(&path).unwrap();
        assert!(state.claim("o/r#1", "aaa").unwrap());
        assert!(!state.claim("o/r#2", "bbb").unwrap());
    }
}
