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

/// Where catch-up starts: just after the last message seen, but never further
/// back than `hours`.
pub fn oldest(cursor: Option<f64>, hours: u64, now: f64) -> f64 {
    let floor = now - (hours as f64) * 3600.0;
    match cursor {
        Some(seen) => seen.max(floor),
        None => floor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_after_the_cursor_but_never_before_the_window() {
        let now = 1_000_000.0;
        assert_eq!(oldest(None, 24, now), now - 86_400.0);
        assert_eq!(oldest(Some(now - 60.0), 24, now), now - 60.0);
        assert_eq!(oldest(Some(now - 200_000.0), 24, now), now - 86_400.0);
    }
}
