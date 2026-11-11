//! Catch-up: the channel messages posted while prk was not running, read with
//! the app's bot token, oldest first. Top-level messages come from
//! conversations.history; thread replies come from conversations.replies, one call
//! per thread that has a reply since the start of the window. Threads started in
//! the last few days count too: a reply today to an older thread is still a request.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;

use crate::warn;

/// How far back to look for threads that received a reply today.
const THREAD_LOOKBACK_SECONDS: f64 = 7.0 * 86_400.0;

type Http = reqwest::blocking::Client;

/// One Slack Web API call (form-encoded), waiting out rate limits.
fn call(http: &Http, token: &str, method: &str, form: &[(&str, String)]) -> Result<Value, String> {
    let mut rate_limited = 0;
    loop {
        let resp = http
            .post(format!("https://slack.com/api/{method}"))
            .bearer_auth(token)
            .form(form)
            .send()
            .map_err(|e| format!("{method}: {e}"))?;
        if resp.status().as_u16() == 429 && rate_limited < 5 {
            rate_limited += 1;
            let wait = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(5);
            warn!("Slack rate limit while catching up; waiting {wait}s");
            std::thread::sleep(Duration::from_secs(wait));
            continue;
        }
        let data: Value = resp.json().map_err(|e| format!("{method}: {e}"))?;
        if data.get("ok").and_then(Value::as_bool) != Some(true) {
            let err = data
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let hint = match err {
                "not_in_channel" => " -- invite the app to the channel",
                "missing_scope" => " -- add the channels:history bot scope and reinstall the app",
                "invalid_auth" | "not_authed" => {
                    " -- slack.bot_token must be the app's xoxb- token"
                }
                _ => "",
            };
            return Err(format!("{method}: {err}{hint}"));
        }
        return Ok(data);
    }
}

/// Every message the paged `method` returns, with `type` and `channel` filled in
/// so each looks like a live `message` event.
fn pages(
    http: &Http,
    token: &str,
    method: &str,
    channel: &str,
    extra: &[(&str, String)],
) -> Result<Vec<Value>, String> {
    let mut found: Vec<Value> = Vec::new();
    let mut cursor = String::new();
    loop {
        let mut form = vec![
            ("channel", channel.to_string()),
            ("limit", "200".to_string()),
        ];
        form.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
        if !cursor.is_empty() {
            form.push(("cursor", cursor.clone()));
        }
        let data = call(http, token, method, &form)?;
        for message in data
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let mut event = message.clone();
            event["type"] = Value::from("message");
            event["channel"] = Value::from(channel);
            found.push(event);
        }
        cursor = data
            .pointer("/response_metadata/next_cursor")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if cursor.is_empty() {
            return Ok(found);
        }
    }
}

/// Messages in `channel` newer than `oldest` (a Slack ts), oldest first, each
/// shaped like a live `message` event so the same matcher reads both. Includes
/// thread replies posted since `oldest`, in threads of any recent age.
pub fn messages(bot_token: &str, channel: &str, oldest: f64) -> Result<Vec<Value>, String> {
    let http = Http::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let window = format!("{:.6}", (oldest - THREAD_LOOKBACK_SECONDS).max(0.0));
    let history = pages(
        &http,
        bot_token,
        "conversations.history",
        channel,
        &[("oldest", window)],
    )?;
    let mut found = since(&history, oldest);
    for parent in threads_with_replies_since(&history, oldest) {
        let replies = pages(
            &http,
            bot_token,
            "conversations.replies",
            channel,
            &[("ts", parent), ("oldest", format!("{oldest:.6}"))],
        )?;
        found.extend(replies);
    }
    Ok(sorted_unique(found, oldest))
}

/// The top-level messages posted at or after `oldest`.
fn since(history: &[Value], oldest: f64) -> Vec<Value> {
    history
        .iter()
        .filter(|m| ts_of(m) >= oldest)
        .cloned()
        .collect()
}

/// The ts of every thread whose latest reply is at or after `oldest`.
fn threads_with_replies_since(history: &[Value], oldest: f64) -> Vec<String> {
    history
        .iter()
        .filter(|m| m.get("reply_count").and_then(Value::as_u64).unwrap_or(0) > 0)
        .filter(|m| {
            m.get("latest_reply")
                .and_then(Value::as_str)
                .and_then(|t| t.parse::<f64>().ok())
                .is_some_and(|t| t >= oldest)
        })
        .filter_map(|m| m.get("ts").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Drop duplicates (a broadcast reply is in both lists) and messages before
/// `oldest` (a thread's parent comes back with its replies), oldest first.
fn sorted_unique(mut messages: Vec<Value>, oldest: f64) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::new();
    messages.retain(|m| {
        let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
        ts_of(m) >= oldest && seen.insert(ts.to_string())
    });
    messages.sort_by(|a, b| ts_of(a).total_cmp(&ts_of(b)));
    messages
}

pub fn ts_of(message: &Value) -> f64 {
    message
        .get("ts")
        .and_then(Value::as_str)
        .and_then(|t| t.parse().ok())
        .unwrap_or(0.0)
}

/// Where catch-up starts: today 00:00 on this machine's clock. Every start
/// rescans the whole day; requests already queued are recognised and skipped.
pub fn today_start() -> f64 {
    use chrono::{Local, TimeZone};
    let midnight = Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default();
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|t| t.timestamp() as f64)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn today_starts_at_local_midnight() {
        let start = today_start();
        let now = chrono::Local::now().timestamp() as f64;
        assert!(start <= now && now - start < 86_400.0 + 3600.0);
        let t = chrono::DateTime::from_timestamp(start as i64, 0)
            .unwrap()
            .with_timezone(&chrono::Local);
        assert_eq!(t.format("%H:%M:%S").to_string(), "00:00:00");
    }

    fn msg(ts: &str) -> Value {
        json!({"ts": ts, "type": "message"})
    }

    fn thread(ts: &str, replies: u64, latest: &str) -> Value {
        json!({"ts": ts, "reply_count": replies, "latest_reply": latest})
    }

    #[test]
    fn only_threads_with_a_reply_since_the_window_are_opened() {
        let history = vec![
            msg("1000.0"),                             // plain old message
            thread("1001.0", 2, "1002.0"),             // old thread, old replies
            thread("1003.0", 3, "5000.0"),             // old thread, reply in the window
            thread("5001.0", 1, "5002.0"),             // new thread
            json!({"ts": "5003.0", "reply_count": 0}), // no replies
        ];
        assert_eq!(
            threads_with_replies_since(&history, 5000.0),
            vec!["1003.0".to_string(), "5001.0".to_string()]
        );
    }

    #[test]
    fn old_top_level_messages_are_dropped() {
        let history = vec![msg("1000.0"), msg("5000.5"), thread("5001.0", 1, "5002.0")];
        let kept: Vec<f64> = since(&history, 5000.0).iter().map(ts_of).collect();
        assert_eq!(kept, vec![5000.5, 5001.0]);
    }

    #[test]
    fn replies_merge_without_parents_or_duplicates() {
        // The old parent comes back with its replies, and a broadcast reply is in
        // both the history and the replies.
        let all = vec![
            msg("5001.0"),
            msg("1003.0"), // the old parent
            msg("5004.0"),
            msg("5002.0"),
            msg("5004.0"), // duplicate
        ];
        let ts: Vec<f64> = sorted_unique(all, 5000.0).iter().map(ts_of).collect();
        assert_eq!(ts, vec![5001.0, 5002.0, 5004.0]);
    }
}
