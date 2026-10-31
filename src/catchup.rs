//! Catch-up: the channel messages posted while prk was not running, read with
//! the app's bot token (conversations.history), oldest first. Thread replies are
//! not in the channel history, so only the live listener sees those.

use std::time::Duration;

use serde_json::Value;

use crate::warn;

/// Messages in `channel` newer than `oldest` (a Slack ts), oldest first, each
/// shaped like a live `message` event so the same matcher reads both.
pub fn messages(bot_token: &str, channel: &str, oldest: f64) -> Result<Vec<Value>, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut found: Vec<Value> = Vec::new();
    let mut cursor = String::new();
    let mut rate_limited = 0;
    loop {
        let mut form = vec![
            ("channel", channel.to_string()),
            ("oldest", format!("{oldest:.6}")),
            ("limit", "200".to_string()),
        ];
        if !cursor.is_empty() {
            form.push(("cursor", cursor.clone()));
        }
        let resp = http
            .post("https://slack.com/api/conversations.history")
            .bearer_auth(bot_token)
            .form(&form)
            .send()
            .map_err(|e| format!("conversations.history: {e}"))?;
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
        let data: Value = resp
            .json()
            .map_err(|e| format!("conversations.history: {e}"))?;
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
            return Err(format!("conversations.history: {err}{hint}"));
        }
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
            break;
        }
    }
    found.sort_by(|a, b| ts_of(a).total_cmp(&ts_of(b)));
    Ok(found)
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
}
