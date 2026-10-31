//! Slack Socket Mode, adapted from ack-bot's listener.
//!
//! `apps.connections.open` (xapp- token) hands back a single-use wss URL; every
//! envelope is acknowledged by its envelope_id or Slack redelivers it. Slack
//! cycles the connection about hourly, so a close means reconnect, not fail.

use std::net::TcpStream;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::stop::Stop;
use crate::{debug, error, warn};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Ask Slack for a websocket URL. Also how `check` proves the app token works.
pub fn open_connection(app_token: &str) -> Result<String, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let text = http
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .and_then(|r| r.text())
        .map_err(|e| format!("could not reach Slack: {e}"))?;
    let data: Value = serde_json::from_str(&text).map_err(|_| {
        let snippet: String = text.chars().take(200).collect();
        format!("apps.connections.open returned non-JSON: {snippet}")
    })?;
    if data.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(format!(
            "apps.connections.open: {err} -- slack.app_token must be an xapp- token with connections:write"
        ));
    }
    data.get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "apps.connections.open returned no url".to_string())
}

/// Acknowledge one envelope, then pass an events_api event on. False = reconnect.
fn handle(raw: &str, socket: &mut Socket, on_event: &dyn Fn(&Value)) -> bool {
    let Ok(envelope) = serde_json::from_str::<Value>(raw) else {
        return true;
    };
    if let Some(id) = envelope.get("envelope_id").and_then(Value::as_str)
        && let Err(e) = socket.send(Message::text(json!({"envelope_id": id}).to_string()))
    {
        warn!("socket: could not acknowledge an envelope: {e}");
        return false;
    }
    match envelope.get("type").and_then(Value::as_str) {
        Some("disconnect") => {
            debug!("socket: Slack asked for a reconnect");
            false
        }
        Some("events_api") => {
            if let Some(event) = envelope.get("payload").and_then(|p| p.get("event")) {
                on_event(event);
            }
            true
        }
        _ => true,
    }
}

/// Reads time out so the loop notices `stop` between frames.
fn set_read_timeout(socket: &mut Socket, timeout: Duration) {
    let tcp = match socket.get_mut() {
        MaybeTlsStream::Plain(tcp) => Some(&*tcp),
        MaybeTlsStream::Rustls(tls) => Some(&tls.sock),
        _ => None,
    };
    if let Some(tcp) = tcp {
        let _ = tcp.set_read_timeout(Some(timeout));
    }
}

/// Connect and pump events until `stop` is set, reconnecting with backoff.
pub fn run(app_token: &str, on_event: &dyn Fn(&Value), stop: &Stop) {
    let mut attempt: u32 = 0;
    while !stop.is_set() {
        let connected = open_connection(app_token).and_then(|url| {
            tungstenite::connect(&url).map_err(|e| format!("socket connect failed ({e})"))
        });
        let mut socket = match connected {
            Ok((socket, _)) => socket,
            Err(why) => {
                attempt += 1;
                let wait = (1u64 << (attempt - 1).min(6)).min(60);
                error!("{why} -- retrying in {wait}s");
                stop.wait(wait as f64);
                continue;
            }
        };
        if attempt > 0 {
            crate::info!("reconnected to Slack");
        }
        attempt = 0;
        debug!("socket: connected");
        // Slack pings every ~30s, so this only fires on a dead link.
        set_read_timeout(&mut socket, Duration::from_secs(45));

        while !stop.is_set() {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    if !handle(text.as_str(), &mut socket, on_event) {
                        break;
                    }
                }
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {
                    let _ = socket.flush();
                }
                Ok(Message::Close(_)) => break,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let _ = socket.flush();
                }
                Err(e) => {
                    if !stop.is_set() {
                        warn!("socket: read failed ({e}) -- reconnecting");
                    }
                    break;
                }
            }
        }
        let _ = socket.close(None);
        if !stop.is_set() {
            stop.wait(1.0);
        }
    }
}
