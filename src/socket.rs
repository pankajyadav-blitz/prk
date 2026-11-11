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

/// Slack pings about every 30s. Nothing at all for this long means the link is
/// dead (typically after the laptop slept: the connection is gone, but the socket
/// never reports it).
const SILENCE_LIMIT: Duration = Duration::from_secs(90);
/// Connecting and the websocket handshake must finish within this.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Open the websocket with timeouts on connect, handshake and writes, so a dead
/// network after wake-up fails fast and is retried instead of hanging.
fn connect(url: &str) -> Result<Socket, String> {
    use std::net::ToSocketAddrs;
    let uri: tungstenite::http::Uri = url.parse().map_err(|e| format!("bad url ({e})"))?;
    let host = uri.host().ok_or("url has no host")?;
    let port = uri.port_u16().unwrap_or(443);
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host} ({e})"))?
        .next()
        .ok_or_else(|| format!("cannot resolve {host}"))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("cannot connect to {host} ({e})"))?;
    let _ = tcp.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = tcp.set_write_timeout(Some(CONNECT_TIMEOUT));
    tungstenite::client_tls(url, tcp)
        .map(|(socket, _)| socket)
        .map_err(|e| format!("websocket handshake failed ({e})"))
}

/// Connect and pump events until `stop` is set, reconnecting with backoff.
pub fn run(app_token: &str, on_event: &dyn Fn(&Value), stop: &Stop) {
    let mut attempt: u32 = 0;
    while !stop.is_set() {
        let connected = open_connection(app_token)
            .and_then(|url| connect(&url).map_err(|e| format!("socket connect failed: {e}")));
        let mut socket = match connected {
            Ok(socket) => socket,
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
        crate::activity::get().set_connected(true);
        attempt = 0;
        debug!("socket: connected");
        // Slack pings every ~30s, so a quiet read only means a dead link if it lasts.
        set_read_timeout(&mut socket, Duration::from_secs(45));
        // Wall clock, not Instant: a sleeping laptop stops the monotonic clock,
        // so only the wall clock shows how long the link was really silent.
        let mut last_heard = std::time::SystemTime::now();

        while !stop.is_set() {
            let read = socket.read();
            if read.is_ok() {
                last_heard = std::time::SystemTime::now();
            }
            match read {
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
                    let silent = last_heard.elapsed().unwrap_or_default();
                    if silent > SILENCE_LIMIT {
                        warn!(
                            "socket: nothing from Slack for {}s (the computer was asleep, or the network dropped) -- reconnecting",
                            silent.as_secs()
                        );
                        break;
                    }
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
        crate::activity::get().set_connected(false);
        if !stop.is_set() {
            stop.wait(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_url_fails_at_once() {
        assert!(connect("not a url").is_err());
        assert!(connect("wss:///no-host").is_err());
    }
}
