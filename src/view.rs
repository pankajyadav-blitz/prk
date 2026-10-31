//! Plain terminal output: `prk -H`, `prk -c`, `prk --stats`, and the step lines
//! of `prk review` / `prk check`. Coloured only when stdout is a terminal and
//! NO_COLOR is unset, so piped output stays plain text.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::OnceLock;

use chrono::{Local, TimeZone};
use crossterm::style::{Color, Stylize};

use crate::db::{self, Comment, Db, Job};

pub fn color_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

pub fn paint(text: &str, color: Color) -> String {
    if color_on() {
        text.with(color).to_string()
    } else {
        text.to_string()
    }
}

pub fn bold(text: &str) -> String {
    if color_on() {
        text.bold().to_string()
    } else {
        text.to_string()
    }
}

pub fn dim(text: &str) -> String {
    paint(text, Color::DarkGrey)
}

/// Glyph and colour for a job status.
pub fn status_look(status: &str) -> (&'static str, Color) {
    match status {
        db::REVIEWED => ("✓", Color::Green),
        db::SKIPPED => ("↷", Color::Yellow),
        db::FAILED => ("✗", Color::Red),
        db::RUNNING => ("◉", Color::Cyan),
        _ => ("•", Color::Blue),
    }
}

/// Glyph and colour for a follow-up verdict.
pub fn verdict_look(verdict: &str) -> (&'static str, &'static str, Color) {
    match verdict {
        "fixed" => ("✅", "fixed", Color::Green),
        "open" => ("⚠️", "open", Color::Yellow),
        "partly" => ("↔️", "partly", Color::Cyan),
        _ => ("", "", Color::Reset),
    }
}

/// "today 11:42", "yesterday 18:05", "Oct 28 10:12".
pub fn when(secs: i64) -> String {
    if secs <= 0 {
        return "-".to_string();
    }
    let Some(t) = Local.timestamp_opt(secs, 0).single() else {
        return "-".to_string();
    };
    let today = Local::now().date_naive();
    let day = t.date_naive();
    if day == today {
        format!("today {}", t.format("%H:%M"))
    } else if Some(day) == today.pred_opt() {
        format!("yesterday {}", t.format("%H:%M"))
    } else {
        t.format("%b %d %H:%M").to_string()
    }
}

/// "4m10s", "52s", "1h03m".
pub fn took(secs: i64) -> String {
    match secs {
        s if s <= 0 => "-".to_string(),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Like `took`, but "0s" rather than "-", for timers that are running.
pub fn elapsed(secs: i64) -> String {
    if secs <= 0 {
        "0s".to_string()
    } else {
        took(secs)
    }
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// "@916a2e0", or "916a2e0→a1b2c3d" for a follow-up.
pub fn commit_label(job: &Job) -> String {
    match (job.kind.as_str(), job.head.is_empty()) {
        (_, true) => String::new(),
        ("follow-up", _) if !job.prev_head.is_empty() => {
            format!("{}→{}", short(&job.prev_head), short(&job.head))
        }
        _ => format!("@{}", short(&job.head)),
    }
}

/// Status word as history shows it: a follow-up that finished says so.
pub fn status_word(job: &Job) -> &str {
    if job.status == db::REVIEWED && job.kind == "follow-up" {
        "follow-up"
    } else {
        &job.status
    }
}

pub fn clip(text: &str, width: usize) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() <= width {
        return text;
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The terminal's width; piped output is not clipped.
fn term_width() -> usize {
    if !std::io::stdout().is_terminal() {
        return usize::MAX / 2;
    }
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(120)
        .max(80)
}

/// `prk -H`: one line per job, newest first.
pub fn history(jobs: &[Job]) {
    if jobs.is_empty() {
        println!("No reviews yet.");
        return;
    }
    let reviewed = jobs.iter().filter(|j| j.status == db::REVIEWED).count();
    let skipped = jobs.iter().filter(|j| j.status == db::SKIPPED).count();
    let failed = jobs.iter().filter(|j| j.status == db::FAILED).count();
    let waiting = jobs
        .iter()
        .filter(|j| j.status == db::WAITING || j.status == db::RUNNING)
        .count();
    println!(
        "{}",
        bold(&format!(
            "{} jobs · {reviewed} reviewed · {skipped} skipped · {failed} failed · {waiting} queued",
            jobs.len()
        ))
    );
    let width = term_width();
    for job in jobs {
        let (glyph, color) = status_look(&job.status);
        let when_at = job.finished_at.max(job.started_at).max(job.queued_at);
        let fixed = format!(
            "{:<15} {} {:<10} {:<22} {:<16} {:>7}  ",
            when(when_at),
            paint(glyph, color),
            paint(status_word(job), color),
            clip(&format!("{}#{}", job.repo, job.number), 22),
            commit_label(job),
            took(job.took()),
        );
        let used = 15 + 1 + 1 + 1 + 10 + 1 + 22 + 1 + 16 + 1 + 7 + 2;
        let rest = width.saturating_sub(used + 1);
        let note = if job.reason.is_empty() || job.status == db::REVIEWED && job.kind != "follow-up"
        {
            if job.status == db::REVIEWED && !job.reason.is_empty() {
                format!("{} · {}", job.reason, job.title)
            } else {
                job.title.clone()
            }
        } else {
            job.reason.clone()
        };
        println!("{fixed}{}", dim(&clip(&note, rest)));
    }
}

/// `prk -H <PR url>`: every attempt for one PR, with the details.
pub fn history_of_pr(db: &Db, jobs: &[Job]) {
    let Some(first) = jobs.first() else {
        println!("No reviews of that PR yet.");
        return;
    };
    println!("{}  {}", bold(&first.key()), first.title);
    println!("{}\n", dim(&first.url()));
    for job in jobs {
        let (glyph, color) = status_look(&job.status);
        println!(
            "{} {}  {}  {}",
            paint(glyph, color),
            paint(&format!("{:<10}", status_word(job)), color),
            when(job.finished_at.max(job.started_at).max(job.queued_at)),
            commit_label(job)
        );
        let detail = |label: &str, value: String| {
            if !value.is_empty() {
                println!("    {:<10} {value}", dim(label));
            }
        };
        detail("result", job.reason.clone());
        detail(
            "asked by",
            match (job.requested_by.is_empty(), job.source.as_str()) {
                (true, source) => source.to_string(),
                (false, source) => format!("{} ({source})", job.requested_by),
            },
        );
        detail("message", slack_time(&job.slack_ts));
        detail(
            "took",
            if job.took() > 0 {
                took(job.took())
            } else {
                String::new()
            },
        );
        detail(
            "attempts",
            if job.attempts > 1 {
                job.attempts.to_string()
            } else {
                String::new()
            },
        );
        detail("comments", {
            let n = db.comments(job.id).len();
            if n > 0 {
                format!("{n} saved · prk -c {}", job.url())
            } else {
                String::new()
            }
        });
        detail("log", job.log.clone());
        println!();
    }
}

/// When a Slack message (by its ts) was posted, or empty.
pub fn slack_time(ts: &str) -> String {
    ts.split('.')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|secs| format!("posted in Slack {}", when(secs)))
        .unwrap_or_default()
}

/// `prk -c`: the comments one review posted, grouped by file.
pub fn comments(job: &Job, list: &[Comment]) {
    let inline = list.iter().filter(|c| c.kind == "inline").count();
    println!(
        "{}  {}",
        bold(&format!(
            "{} · {} {}",
            job.key(),
            status_word(job),
            commit_label(job)
        )),
        dim(&format!(
            "{} · {} comments",
            when(job.finished_at.max(job.started_at)),
            inline
        ))
    );
    if !job.title.is_empty() {
        println!("{}", job.title);
    }
    println!("{}\n", dim(&job.url()));
    if list.is_empty() {
        println!("  {}", dim("no comments saved for this review"));
        return;
    }
    let width = term_width();
    let mut by_file: BTreeMap<&str, Vec<&Comment>> = BTreeMap::new();
    for c in list.iter().filter(|c| c.kind != "summary") {
        by_file.entry(c.path.as_str()).or_default().push(c);
    }
    for (path, items) in &by_file {
        println!(
            "{}",
            paint(
                if path.is_empty() { "(no file)" } else { path },
                Color::Cyan
            )
        );
        for c in items {
            let (mark, word, color) = verdict_look(&c.verdict);
            let tag = if c.kind == "reply" {
                dim("reply ")
            } else if word.is_empty() {
                "      ".to_string()
            } else {
                paint(&format!("{mark} {word:<6}"), color)
            };
            let prefix = format!("  {:>5}  ", format!(":{}", c.line));
            let indent = 2 + 5 + 2 + 9;
            let mut lines = wrap(&c.body, width.saturating_sub(indent + 1).max(30)).into_iter();
            println!("{}{tag} {}", dim(&prefix), lines.next().unwrap_or_default());
            for line in lines {
                println!("{:indent$}{line}", "");
            }
            if !c.verdict_commit.is_empty() && c.verdict == "fixed" {
                println!(
                    "{:indent$}{}",
                    "",
                    dim(&format!("fixed in {}", short(&c.verdict_commit)))
                );
            }
        }
        println!();
    }
    for c in list.iter().filter(|c| c.kind == "summary") {
        let mut lines = wrap(&c.body, width.saturating_sub(10).max(30)).into_iter();
        println!(
            "{} {}",
            paint("summary ", Color::Magenta),
            lines.next().unwrap_or_default()
        );
        for line in lines {
            println!("         {line}");
        }
    }
}

/// Word-wrap `text` to `width` columns, keeping its own line breaks.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// `prk --stats`.
pub fn stats(db: &Db) {
    let all = db.history(&db::HistoryFilter::default());
    if all.is_empty() {
        println!("No reviews yet.");
        return;
    }
    let now = db::now();
    let reviewed: Vec<&Job> = all.iter().filter(|j| j.status == db::REVIEWED).collect();
    let in_window = |secs: i64| {
        reviewed
            .iter()
            .filter(|j| j.finished_at >= now - secs)
            .count()
    };
    let count = |status: &str| all.iter().filter(|j| j.status == status).count();
    let avg = if reviewed.is_empty() {
        0
    } else {
        reviewed.iter().map(|j| j.took()).sum::<i64>() / reviewed.len() as i64
    };
    let follow_ups = reviewed.iter().filter(|j| j.kind == "follow-up").count();
    let mut comments = 0;
    let (mut fixed, mut open, mut partly) = (0, 0, 0);
    for job in &reviewed {
        for c in db.comments(job.id) {
            if c.kind == "inline" {
                comments += 1;
                match c.verdict.as_str() {
                    "fixed" => fixed += 1,
                    "open" => open += 1,
                    "partly" => partly += 1,
                    _ => {}
                }
            }
        }
    }
    let row = |label: &str, value: String| println!("  {:<22} {value}", dim(label));
    println!("{}", bold("prk stats"));
    row(
        "reviewed",
        format!("{} ({} follow-ups)", reviewed.len(), follow_ups),
    );
    row(
        "  today / 7 days",
        format!("{} / {}", in_window(86_400), in_window(7 * 86_400)),
    );
    row("skipped", count(db::SKIPPED).to_string());
    row(
        "failed",
        paint(
            &count(db::FAILED).to_string(),
            if count(db::FAILED) > 0 {
                Color::Red
            } else {
                Color::Reset
            },
        ),
    );
    row(
        "queued",
        (count(db::WAITING) + count(db::RUNNING)).to_string(),
    );
    row("average review", took(avg));
    row("comments posted", comments.to_string());
    if fixed + open + partly > 0 {
        row(
            "follow-up verdicts",
            format!(
                "{} fixed · {} open · {} partly",
                paint(&fixed.to_string(), Color::Green),
                paint(&open.to_string(), Color::Yellow),
                paint(&partly.to_string(), Color::Cyan)
            ),
        );
    }
    let mut repos: BTreeMap<&str, usize> = BTreeMap::new();
    for job in &reviewed {
        *repos.entry(job.repo.as_str()).or_default() += 1;
    }
    let mut repos: Vec<_> = repos.into_iter().collect();
    repos.sort_by_key(|r| std::cmp::Reverse(r.1));
    if !repos.is_empty() {
        row(
            "by repo",
            repos
                .iter()
                .take(6)
                .map(|(r, n)| format!("{r} {n}"))
                .collect::<Vec<_>>()
                .join(" · "),
        );
    }
}

/// Parse "7d", "12h", "30m", "2w" into seconds.
pub fn parse_since(text: &str) -> Option<i64> {
    let text = text.trim();
    let (num, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = num.parse().ok()?;
    let unit = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return None,
    };
    Some(n * unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_windows() {
        assert_eq!(took(0), "-");
        assert_eq!(took(52), "52s");
        assert_eq!(took(250), "4m10s");
        assert_eq!(took(3780), "1h03m");
        assert_eq!(parse_since("7d"), Some(604_800));
        assert_eq!(parse_since("12h"), Some(43_200));
        assert_eq!(parse_since("2w"), Some(1_209_600));
        assert_eq!(parse_since("7"), None);
        assert_eq!(parse_since("x"), None);
    }

    #[test]
    fn wraps_and_clips() {
        assert_eq!(wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]);
        assert_eq!(wrap("one\ntwo", 40), vec!["one", "two"]);
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abc", 4), "abc");
    }

    #[test]
    fn follow_ups_show_both_commits() {
        let job = Job {
            kind: "follow-up".into(),
            prev_head: "aaaaaaa111".into(),
            head: "bbbbbbb222".into(),
            status: db::REVIEWED.into(),
            ..Default::default()
        };
        assert_eq!(commit_label(&job), "aaaaaaa→bbbbbbb");
        assert_eq!(status_word(&job), "follow-up");
    }
}
