//! The local database (workdir/state.sqlite3): the review queue, which doubles
//! as the history `prk -H` lists, and the comments each review posted (`prk -c`).
//!
//! A job goes waiting -> running -> reviewed | skipped | failed. A failed job is
//! put back to waiting (with a delay) until it has used its attempts, and a
//! job left running by a process that died is put back on the next start.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params};

pub const WAITING: &str = "waiting";
pub const RUNNING: &str = "running";
pub const REVIEWED: &str = "reviewed";
pub const SKIPPED: &str = "skipped";
pub const FAILED: &str = "failed";

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default)]
pub struct Job {
    pub id: i64,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub status: String,
    /// "review" or "follow-up"; empty until the job has run.
    pub kind: String,
    pub head: String,
    /// For a follow-up: the commit the previous review saw.
    pub prev_head: String,
    pub title: String,
    pub author: String,
    pub requested_by: String,
    pub slack_ts: String,
    /// "slack", "catch-up" or "manual".
    pub source: String,
    pub force: bool,
    pub attempts: i64,
    pub reason: String,
    pub log: String,
    pub queued_at: i64,
    pub started_at: i64,
    pub finished_at: i64,
    pub not_before: i64,
    /// Slack thread to tell about the comments; empty = none.
    pub slack_thread: String,
    /// Slack name of `requested_by`; empty if unknown.
    pub requested_name: String,
}

impl Job {
    pub fn key(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }

    /// `repo#n`, for lists where the org is the same on every line.
    pub fn short_key(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.repo, self.number
        )
    }

    /// Seconds from start to finish (or to now while running); 0 if never started.
    pub fn took(&self) -> i64 {
        if self.started_at == 0 {
            return 0;
        }
        let end = if self.finished_at > 0 {
            self.finished_at
        } else {
            now()
        };
        (end - self.started_at).max(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Comment {
    pub job_id: i64,
    pub gh_id: i64,
    /// "inline", "reply" or "summary".
    pub kind: String,
    pub path: String,
    pub line: i64,
    pub body: String,
    pub diff_hunk: String,
    pub url: String,
    pub in_reply_to: i64,
    pub commit: String,
    /// Set by a later follow-up: "fixed", "open" or "partly".
    pub verdict: String,
    pub verdict_commit: String,
    pub created_at: String,
}

/// Whether `pid` is a live prk process; a pid the system handed to something
/// else since does not count.
#[cfg(target_os = "linux")]
fn is_prk(pid: i64) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|cmd| {
            let argv0 = cmd.split(|b| *b == 0).next().unwrap_or_default();
            named_prk(&String::from_utf8_lossy(argv0))
        })
        .unwrap_or(false)
}

/// Whether `pid` is a live prk process (no /proc here: ask tasklist).
#[cfg(windows)]
fn is_prk(pid: i64) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|out| {
            // "prk.exe","1234",... -- or an INFO line when there is no such process
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.split(',').next())
                .any(|image| named_prk(image.trim_matches('"')))
        })
        .unwrap_or(false)
}

/// Whether `pid` is a live prk process (no /proc here: ask ps).
#[cfg(all(unix, not(target_os = "linux")))]
fn is_prk(pid: i64) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .map(|out| out.status.success() && named_prk(String::from_utf8_lossy(&out.stdout).trim()))
        .unwrap_or(false)
}

/// Whether a program path or name is prk (or its test binary, prk-<hash>).
fn named_prk(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .is_some_and(|name| name.to_string_lossy().to_lowercase().starts_with("prk"))
}

/// What `enqueue` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Enqueued {
    New(i64),
    /// The PR is already waiting or being reviewed (that job's id and status).
    Pending(i64, String),
    /// This exact Slack message was already turned into a job.
    Seen(i64),
}

pub struct Db {
    conn: Mutex<Connection>,
}

const JOB_COLUMNS: &str = "id, owner, repo, number, status, kind, head, prev_head, title, author, \
     requested_by, slack_ts, source, force, attempts, reason, log, queued_at, started_at, \
     finished_at, not_before, slack_thread, requested_name";

fn job_from(row: &Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: row.get(0)?,
        owner: row.get(1)?,
        repo: row.get(2)?,
        number: row.get::<_, i64>(3)? as u64,
        status: row.get(4)?,
        kind: row.get(5)?,
        head: row.get(6)?,
        prev_head: row.get(7)?,
        title: row.get(8)?,
        author: row.get(9)?,
        requested_by: row.get(10)?,
        slack_ts: row.get(11)?,
        source: row.get(12)?,
        force: row.get::<_, i64>(13)? != 0,
        attempts: row.get(14)?,
        reason: row.get(15)?,
        log: row.get(16)?,
        queued_at: row.get(17)?,
        started_at: row.get(18)?,
        finished_at: row.get(19)?,
        not_before: row.get(20)?,
        slack_thread: row.get(21)?,
        requested_name: row.get(22)?,
    })
}

const COMMENT_COLUMNS: &str = "job_id, gh_id, kind, path, line, body, diff_hunk, url, in_reply_to, \
     commit_sha, verdict, verdict_commit, created_at";

fn comment_from(row: &Row) -> rusqlite::Result<Comment> {
    Ok(Comment {
        job_id: row.get(0)?,
        gh_id: row.get(1)?,
        kind: row.get(2)?,
        path: row.get(3)?,
        line: row.get(4)?,
        body: row.get(5)?,
        diff_hunk: row.get(6)?,
        url: row.get(7)?,
        in_reply_to: row.get(8)?,
        commit: row.get(9)?,
        verdict: row.get(10)?,
        verdict_commit: row.get(11)?,
        created_at: row.get(12)?,
    })
}

/// Filters for `history`.
#[derive(Debug, Default)]
pub struct HistoryFilter {
    pub limit: usize,
    pub since: Option<i64>,
    pub repo: Option<String>,
    pub status: Option<String>,
    pub pr: Option<(String, String, u64)>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let err = |e: rusqlite::Error| format!("{}: {e}", path.display());
        let conn = Connection::open(path).map_err(err)?;
        // The listener and a manual `prk review` may share the file.
        conn.busy_timeout(Duration::from_secs(10)).map_err(err)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS jobs (
               id           INTEGER PRIMARY KEY AUTOINCREMENT,
               owner        TEXT NOT NULL,
               repo         TEXT NOT NULL,
               number       INTEGER NOT NULL,
               status       TEXT NOT NULL,
               kind         TEXT NOT NULL DEFAULT '',
               head         TEXT NOT NULL DEFAULT '',
               prev_head    TEXT NOT NULL DEFAULT '',
               title        TEXT NOT NULL DEFAULT '',
               author       TEXT NOT NULL DEFAULT '',
               requested_by TEXT NOT NULL DEFAULT '',
               slack_ts     TEXT NOT NULL DEFAULT '',
               source       TEXT NOT NULL DEFAULT '',
               force        INTEGER NOT NULL DEFAULT 0,
               attempts     INTEGER NOT NULL DEFAULT 0,
               reason       TEXT NOT NULL DEFAULT '',
               log          TEXT NOT NULL DEFAULT '',
               pid          INTEGER NOT NULL DEFAULT 0,
               queued_at    INTEGER NOT NULL DEFAULT 0,
               started_at   INTEGER NOT NULL DEFAULT 0,
               finished_at  INTEGER NOT NULL DEFAULT 0,
               not_before   INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS jobs_pr ON jobs (owner, repo, number);
             CREATE INDEX IF NOT EXISTS jobs_status ON jobs (status);
             CREATE TABLE IF NOT EXISTS comments (
               job_id         INTEGER NOT NULL,
               gh_id          INTEGER NOT NULL,
               kind           TEXT NOT NULL,
               path           TEXT NOT NULL DEFAULT '',
               line           INTEGER NOT NULL DEFAULT 0,
               body           TEXT NOT NULL DEFAULT '',
               diff_hunk      TEXT NOT NULL DEFAULT '',
               url            TEXT NOT NULL DEFAULT '',
               in_reply_to    INTEGER NOT NULL DEFAULT 0,
               commit_sha     TEXT NOT NULL DEFAULT '',
               verdict        TEXT NOT NULL DEFAULT '',
               verdict_commit TEXT NOT NULL DEFAULT '',
               created_at     TEXT NOT NULL DEFAULT '',
               PRIMARY KEY (job_id, gh_id));
             CREATE TABLE IF NOT EXISTS cursors (
               channel TEXT PRIMARY KEY,
               last_ts TEXT NOT NULL);",
        )
        .map_err(err)?;
        // Added later: databases made before them get the columns now.
        for column in ["slack_thread", "requested_name"] {
            if conn
                .prepare(&format!("SELECT {column} FROM jobs LIMIT 0"))
                .is_err()
            {
                conn.execute_batch(&format!(
                    "ALTER TABLE jobs ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
                ))
                .map_err(err)?;
            }
        }
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a review of a PR, unless it is already waiting or running, or this
    /// Slack message (`slack_ts`) was already queued once.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        requested_by: &str,
        slack_ts: &str,
        slack_thread: &str,
        source: &str,
        force: bool,
    ) -> Result<Enqueued, String> {
        let conn = self.conn();
        let n = number as i64;
        if !slack_ts.is_empty() {
            let seen: Option<i64> = conn
                .query_row(
                    "SELECT id FROM jobs WHERE owner = ?1 AND repo = ?2 AND number = ?3 AND slack_ts = ?4",
                    params![owner, repo, n, slack_ts],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(id) = seen {
                return Ok(Enqueued::Seen(id));
            }
        }
        let pending: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, status FROM jobs WHERE owner = ?1 AND repo = ?2 AND number = ?3 \
                 AND status IN ('waiting', 'running') ORDER BY id LIMIT 1",
                params![owner, repo, n],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((id, status)) = pending {
            if force {
                conn.execute("UPDATE jobs SET force = 1 WHERE id = ?1", params![id])
                    .map_err(|e| e.to_string())?;
            }
            return Ok(Enqueued::Pending(id, status));
        }
        conn.execute(
            "INSERT INTO jobs (owner, repo, number, status, requested_by, slack_ts, slack_thread, source, force, queued_at) \
             VALUES (?1, ?2, ?3, 'waiting', ?4, ?5, ?6, ?7, ?8, ?9)",
            params![owner, repo, n, requested_by, slack_ts, slack_thread, source, force as i64, now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(Enqueued::New(conn.last_insert_rowid()))
    }

    /// Take a waiting job for this process: true if it was still waiting.
    pub fn claim(&self, id: i64) -> Result<bool, String> {
        let changed = self
            .conn()
            .execute(
                "UPDATE jobs SET status = 'running', pid = ?2, started_at = ?3, finished_at = 0, \
                 attempts = attempts + 1 WHERE id = ?1 AND status = 'waiting'",
                params![id, std::process::id() as i64, now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(changed == 1)
    }

    /// The oldest waiting job that is due, claimed for this process.
    pub fn claim_next(&self) -> Result<Option<Job>, String> {
        loop {
            let id: Option<i64> = self
                .conn()
                .query_row(
                    "SELECT id FROM jobs WHERE status = 'waiting' AND not_before <= ?1 \
                     ORDER BY queued_at, id LIMIT 1",
                    params![now()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some(id) = id else { return Ok(None) };
            // Another process may take it between the read and the claim.
            if self.claim(id)? {
                return self.job(id);
            }
        }
    }

    pub fn job(&self, id: i64) -> Result<Option<Job>, String> {
        self.conn()
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
                params![id],
                job_from,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Record what the PR lookup found.
    /// Save the Slack name of who asked for the review.
    pub fn set_requested_name(&self, id: i64, name: &str) {
        self.exec(
            "UPDATE jobs SET requested_name = ?2 WHERE id = ?1",
            params![id, name],
        );
    }

    pub fn describe(
        &self,
        id: i64,
        head: &str,
        title: &str,
        author: &str,
        kind: &str,
        prev_head: &str,
    ) {
        self.exec(
            "UPDATE jobs SET head = ?2, title = ?3, author = ?4, kind = ?5, prev_head = ?6 WHERE id = ?1",
            params![id, head, title, author, kind, prev_head],
        );
    }

    pub fn set_log(&self, id: i64, log: &str) {
        self.exec("UPDATE jobs SET log = ?2 WHERE id = ?1", params![id, log]);
    }

    /// Mark a job reviewed or skipped, with a one-line reason.
    pub fn finish(&self, id: i64, status: &str, reason: &str) {
        self.exec(
            "UPDATE jobs SET status = ?2, reason = ?3, finished_at = ?4, pid = 0 WHERE id = ?1",
            params![id, status, reason, now()],
        );
    }

    /// A failed attempt: back to waiting after a delay while attempts remain,
    /// otherwise failed for good. Returns true if it will be retried.
    pub fn fail(&self, id: i64, reason: &str, max_attempts: i64, retry_in: i64) -> bool {
        let attempts: i64 = self
            .conn()
            .query_row(
                "SELECT attempts FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap_or(max_attempts);
        let retry = attempts < max_attempts;
        let reason = format!("{reason} (attempt {attempts}/{max_attempts})");
        if retry {
            self.exec(
                "UPDATE jobs SET status = 'waiting', reason = ?2, finished_at = ?3, pid = 0, \
                 not_before = ?4 WHERE id = ?1",
                params![id, reason, now(), now() + retry_in * attempts],
            );
        } else {
            self.finish(id, FAILED, &reason);
        }
        retry
    }

    /// Put a job back without spending an attempt (Claude usage limit, shutdown).
    pub fn requeue(&self, id: i64, reason: &str, not_before: i64) {
        self.exec(
            "UPDATE jobs SET status = 'waiting', reason = ?2, pid = 0, attempts = MAX(attempts - 1, 0), \
             started_at = 0, not_before = ?3 WHERE id = ?1",
            params![id, reason, not_before],
        );
    }

    /// Drop a waiting job (from the dashboard).
    pub fn drop_waiting(&self, id: i64) -> bool {
        self.conn()
            .execute(
                "UPDATE jobs SET status = 'skipped', reason = 'dropped from the queue', \
                 finished_at = ?2 WHERE id = ?1 AND status = 'waiting'",
                params![id, now()],
            )
            .map(|n| n == 1)
            .unwrap_or(false)
    }

    /// Jobs left running by a process that is gone go back to waiting. Returns
    /// how many were put back.
    pub fn recover(&self) -> usize {
        let running: Vec<(i64, i64)> = {
            let conn = self.conn();
            let Ok(mut stmt) = conn.prepare("SELECT id, pid FROM jobs WHERE status = 'running'")
            else {
                return 0;
            };
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default()
        };
        let mut recovered = 0;
        for (id, pid) in running {
            if pid > 0 && is_prk(pid) {
                continue;
            }
            self.requeue(id, "interrupted; queued again", 0);
            recovered += 1;
        }
        recovered
    }

    /// The last finished review (status reviewed) of a PR, if any.
    pub fn last_reviewed(&self, owner: &str, repo: &str, number: u64) -> Option<Job> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {JOB_COLUMNS} FROM jobs WHERE owner = ?1 AND repo = ?2 AND number = ?3 \
                     AND status = 'reviewed' ORDER BY finished_at DESC, id DESC LIMIT 1"
                ),
                params![owner, repo, number as i64],
                job_from,
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Slack members who asked for a review whose name is not saved yet.
    pub fn unnamed_requesters(&self) -> Vec<String> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT requested_by FROM jobs WHERE requested_by != '' AND requested_name = ''",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Save a member's name on every job they asked for that has none yet.
    pub fn name_requester(&self, user: &str, name: &str) {
        self.exec(
            "UPDATE jobs SET requested_name = ?2 WHERE requested_by = ?1 AND requested_name = ''",
            params![user, name],
        );
    }

    pub fn waiting(&self) -> Vec<Job> {
        self.select(
            &format!(
                "SELECT {JOB_COLUMNS} FROM jobs WHERE status = 'waiting' ORDER BY queued_at, id"
            ),
            params![],
        )
    }

    pub fn running(&self) -> Vec<Job> {
        self.select(
            &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE status = 'running' ORDER BY started_at"),
            params![],
        )
    }

    /// Newest first.
    pub fn history(&self, f: &HistoryFilter) -> Vec<Job> {
        let mut sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE 1 = 1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(since) = f.since {
            sql.push_str(" AND MAX(queued_at, finished_at) >= ?");
            args.push(since.into());
        }
        if let Some(repo) = &f.repo {
            sql.push_str(" AND lower(repo) = lower(?)");
            args.push(repo.clone().into());
        }
        if let Some(status) = &f.status {
            sql.push_str(" AND status = ?");
            args.push(status.clone().into());
        }
        if let Some((owner, repo, number)) = &f.pr {
            sql.push_str(" AND lower(owner) = lower(?) AND lower(repo) = lower(?) AND number = ?");
            args.push(owner.clone().into());
            args.push(repo.clone().into());
            args.push((*number as i64).into());
        }
        sql.push_str(" ORDER BY MAX(queued_at, started_at, finished_at) DESC, id DESC");
        if f.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", f.limit));
        }
        self.select(&sql, rusqlite::params_from_iter(args))
    }

    pub fn save_comments(&self, comments: &[Comment]) {
        for c in comments {
            self.exec(
                &format!(
                    "INSERT OR REPLACE INTO comments ({COMMENT_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
                ),
                params![
                    c.job_id,
                    c.gh_id,
                    c.kind,
                    c.path,
                    c.line,
                    c.body,
                    c.diff_hunk,
                    c.url,
                    c.in_reply_to,
                    c.commit,
                    c.verdict,
                    c.verdict_commit,
                    c.created_at
                ],
            );
        }
    }

    /// A follow-up's verdict on an earlier comment, wherever it was saved.
    pub fn set_verdict(&self, gh_id: i64, verdict: &str, commit: &str) {
        self.exec(
            "UPDATE comments SET verdict = ?2, verdict_commit = ?3 WHERE gh_id = ?1",
            params![gh_id, verdict, commit],
        );
    }

    pub fn comments(&self, job_id: i64) -> Vec<Comment> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT {COMMENT_COLUMNS} FROM comments WHERE job_id = ?1 ORDER BY kind = 'summary', path, line, gh_id"
        )) else {
            return Vec::new();
        };
        stmt.query_map(params![job_id], comment_from)
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The latest verdict on each earlier top-level comment of a PR, by GitHub id.
    pub fn verdicts(&self, owner: &str, repo: &str, number: u64) -> Vec<(i64, String)> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare(
            "SELECT c.gh_id, c.verdict FROM comments c JOIN jobs j ON j.id = c.job_id \
             WHERE j.owner = ?1 AND j.repo = ?2 AND j.number = ?3 AND c.kind = 'inline'",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![owner, repo, number as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    fn select(&self, sql: &str, args: impl rusqlite::Params) -> Vec<Job> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare(sql) else {
            return Vec::new();
        };
        stmt.query_map(args, job_from)
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    fn exec(&self, sql: &str, args: impl rusqlite::Params) {
        if let Err(e) = self.conn().execute(sql, args) {
            crate::error!("database: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> Db {
        let dir = std::env::temp_dir().join(format!("prk-db-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        Db::open(&dir.join("state.sqlite3")).unwrap()
    }

    #[test]
    fn a_pr_is_queued_once_while_pending_and_a_message_only_once() {
        let db = temp("enqueue");
        let first = db
            .enqueue("o", "r", 1, "U1", "1.0", "", "slack", false)
            .unwrap();
        let Enqueued::New(id) = first else { panic!() };
        assert_eq!(
            db.enqueue("o", "r", 1, "U2", "2.0", "", "slack", false)
                .unwrap(),
            Enqueued::Pending(id, WAITING.into())
        );
        assert_eq!(
            db.enqueue("o", "r", 1, "U1", "1.0", "", "catch-up", false)
                .unwrap(),
            Enqueued::Seen(id)
        );
        assert!(db.claim(id).unwrap());
        db.finish(id, REVIEWED, "2 comments");
        // finished: a new message queues it again; the old message still does not
        assert!(matches!(
            db.enqueue("o", "r", 1, "U2", "3.0", "", "slack", false)
                .unwrap(),
            Enqueued::New(_)
        ));
        assert_eq!(
            db.enqueue("o", "r", 1, "U1", "1.0", "", "catch-up", false)
                .unwrap(),
            Enqueued::Seen(id)
        );
    }

    #[test]
    fn a_job_is_claimed_by_one_taker_only() {
        let db = temp("claim");
        let Enqueued::New(id) = db
            .enqueue("o", "r", 1, "", "", "", "manual", false)
            .unwrap()
        else {
            panic!()
        };
        assert!(db.claim(id).unwrap());
        assert!(!db.claim(id).unwrap());
        assert!(db.claim_next().unwrap().is_none());
    }

    #[test]
    fn failures_retry_with_a_delay_until_the_attempts_run_out() {
        let db = temp("fail");
        let Enqueued::New(id) = db.enqueue("o", "r", 1, "", "", "", "slack", false).unwrap() else {
            panic!()
        };
        for attempt in 1..=3 {
            assert!(db.claim(id).unwrap());
            let retried = db.fail(id, "boom", 3, 0);
            assert_eq!(retried, attempt < 3);
        }
        let job = db.job(id).unwrap().unwrap();
        assert_eq!(job.status, FAILED);
        assert_eq!(job.reason, "boom (attempt 3/3)");

        let Enqueued::New(id) = db.enqueue("o", "r", 2, "", "", "", "slack", false).unwrap() else {
            panic!()
        };
        assert!(db.claim(id).unwrap());
        db.fail(id, "boom", 3, 300);
        // not due yet
        assert!(db.claim_next().unwrap().is_none());
    }

    #[test]
    fn a_usage_limit_requeue_does_not_spend_an_attempt() {
        let db = temp("requeue");
        let Enqueued::New(id) = db.enqueue("o", "r", 1, "", "", "", "slack", false).unwrap() else {
            panic!()
        };
        db.claim(id).unwrap();
        db.requeue(id, "usage limit", 0);
        let job = db.job(id).unwrap().unwrap();
        assert_eq!((job.status.as_str(), job.attempts), (WAITING, 0));
    }

    #[test]
    fn jobs_of_a_dead_process_are_recovered() {
        let db = temp("recover");
        let Enqueued::New(id) = db.enqueue("o", "r", 1, "", "", "", "slack", false).unwrap() else {
            panic!()
        };
        db.claim(id).unwrap();
        // our own pid is alive: left alone
        assert_eq!(db.recover(), 0);
        db.exec("UPDATE jobs SET pid = 999999999 WHERE id = ?1", params![id]);
        assert_eq!(db.recover(), 1);
        assert_eq!(db.job(id).unwrap().unwrap().status, WAITING);
    }

    #[test]
    fn last_reviewed_and_history_filters() {
        let db = temp("history");
        for (n, status) in [(1, REVIEWED), (2, SKIPPED), (1, REVIEWED)] {
            let Enqueued::New(id) = db
                .enqueue("o", "suchna", n, "", "", "", "slack", false)
                .unwrap()
            else {
                panic!()
            };
            db.claim(id).unwrap();
            db.describe(id, &format!("head{id}"), "t", "a", "review", "");
            db.finish(id, status, "");
        }
        assert_eq!(db.last_reviewed("o", "suchna", 1).unwrap().head, "head3");
        assert!(db.last_reviewed("o", "suchna", 2).is_none());
        let all = db.history(&HistoryFilter::default());
        assert_eq!(all.len(), 3);
        let skipped = db.history(&HistoryFilter {
            status: Some(SKIPPED.into()),
            ..Default::default()
        });
        assert_eq!(skipped.len(), 1);
        let one = db.history(&HistoryFilter {
            pr: Some(("O".into(), "Suchna".into(), 1)),
            limit: 1,
            ..Default::default()
        });
        assert_eq!(one[0].head, "head3");
    }

    #[test]
    fn verdicts_land_on_the_saved_comment() {
        let db = temp("verdict");
        let Enqueued::New(id) = db.enqueue("o", "r", 1, "", "", "", "slack", false).unwrap() else {
            panic!()
        };
        db.save_comments(&[Comment {
            job_id: id,
            gh_id: 77,
            kind: "inline".into(),
            path: "a.ts".into(),
            line: 3,
            body: "bug".into(),
            ..Default::default()
        }]);
        db.set_verdict(77, "fixed", "abc");
        assert_eq!(db.comments(id)[0].verdict, "fixed");
        assert_eq!(db.verdicts("o", "r", 1), vec![(77, "fixed".to_string())]);
    }
}
