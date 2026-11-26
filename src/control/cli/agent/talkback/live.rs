//! One consented utterance per Gemini Live connection. The protocol only receives
//! audio and status. It has no dependency on the desktop, planner or tool registry.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};
use x_agent::cancellation::Cancellation;

const ENDPOINT: &str = "generativelanguage.googleapis.com";
const PATH: &str = "ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_TIMEOUT: Duration = Duration::from_millis(100);
const TURN_LIMIT: Duration = Duration::from_secs(25);
const FRAME_LIMIT: usize = 256 * 1024;
pub(super) const INPUT_RATE: u32 = 16_000;
pub(super) const OUTPUT_RATE: u32 = 24_000;
pub(super) const CLIP_LIMIT: Duration = Duration::from_secs(10);
const IN_LIMIT: usize = INPUT_RATE as usize * CLIP_LIMIT.as_secs() as usize * 2;
const OUT_LIMIT: usize = OUTPUT_RATE as usize * 20 * 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Fault {
    Cancelled,
    Unavailable,
    Credentials,
    Quota,
    Deadline,
    Rollover,
    Protocol,
    NoAudio,
}

impl Fault {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Cancelled => "Talkback stopped",
            Self::Unavailable => "Gemini Live is unavailable",
            Self::Credentials => "Gemini Live refused this project's API key or access",
            Self::Quota => "Gemini Live quota or capacity reached",
            Self::Deadline => "Talkback reached its time limit",
            Self::Rollover => "Gemini Live ended this connection. Hold Space for a new turn",
            Self::Protocol => "Gemini Live returned an unsupported reply",
            Self::NoAudio => "Talkback had no audio to send or play",
        }
    }
}

pub(super) fn model_valid(model: &str) -> bool {
    model.starts_with("gemini-")
        && model.len() <= 64
        && model
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-' || ch == b'.')
}

pub(super) fn key_valid(key: &str) -> bool {
    (20..=160).contains(&key.len())
        && key
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
}

trait Session {
    fn send(&mut self, request: Value) -> Result<(), Fault>;
    fn receive(&mut self) -> Result<Option<Value>, Fault>;
}

struct Socket(WebSocket<MaybeTlsStream<TcpStream>>);

impl Socket {
    fn connect(key: &str, stop: &Cancellation) -> Result<Self, Fault> {
        let addrs = (ENDPOINT, 443)
            .to_socket_addrs()
            .map_err(|_| Fault::Unavailable)?;
        let mut socket = None;
        for addr in addrs.take(4) {
            if stop.requested() {
                return Err(Fault::Cancelled);
            }
            if let Ok(stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                socket = Some(stream);
                break;
            }
        }
        let stream = socket.ok_or(Fault::Unavailable)?;
        stream
            .set_read_timeout(Some(CONNECT_TIMEOUT))
            .map_err(|_| Fault::Unavailable)?;
        stream
            .set_write_timeout(Some(CONNECT_TIMEOUT))
            .map_err(|_| Fault::Unavailable)?;
        if stop.requested() {
            return Err(Fault::Cancelled);
        }
        let url = format!("wss://{ENDPOINT}/{PATH}?key={key}");
        let config = WebSocketConfig::default()
            .max_frame_size(Some(FRAME_LIMIT))
            .max_message_size(Some(FRAME_LIMIT));
        let (mut websocket, _) =
            tungstenite::client_tls_with_config(url, stream, Some(config), None).map_err(
                |error| match error {
                    tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)) => {
                        http_status(response.status().as_u16())
                    }
                    _ => Fault::Unavailable,
                },
            )?;
        let MaybeTlsStream::NativeTls(stream) = websocket.get_mut() else {
            return Err(Fault::Unavailable);
        };
        stream
            .get_ref()
            .set_read_timeout(Some(POLL_TIMEOUT))
            .map_err(|_| Fault::Unavailable)?;
        Ok(Self(websocket))
    }
}

fn http_status(code: u16) -> Fault {
    match code {
        401 | 403 => Fault::Credentials,
        429 | 503 => Fault::Quota,
        _ => Fault::Unavailable,
    }
}

impl Session for Socket {
    fn send(&mut self, request: Value) -> Result<(), Fault> {
        self.0
            .send(Message::Text(request.to_string().into()))
            .map_err(|_| Fault::Unavailable)
    }

    fn receive(&mut self) -> Result<Option<Value>, Fault> {
        let bytes = match self.0.read() {
            Ok(Message::Text(text)) => text.as_bytes().to_vec(),
            Ok(Message::Binary(bytes)) => bytes.to_vec(),
            Ok(Message::Close(_)) => return Err(Fault::Rollover),
            Ok(_) => return Ok(None),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(None);
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Err(Fault::Rollover);
            }
            Err(_) => return Err(Fault::Unavailable),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Fault::Protocol)
    }
}

pub(super) fn turn(
    key: &str,
    model: &str,
    pcm: &[u8],
    stop: &Cancellation,
    play: impl FnMut(Vec<i16>) -> Result<(), Fault>,
) -> Result<(), Fault> {
    if !key_valid(key) {
        return Err(Fault::Credentials);
    }
    check_input(model, pcm)?;
    // This transport exists only in the standalone terminal command. Its WebSocket
    // library can trace the request URL and raw audio frames. Keep that process's
    // log facade off for the lifetime of its workers, even with RUST_LOG=trace.
    log::set_max_level(log::LevelFilter::Off);
    let deadline = Instant::now() + TURN_LIMIT;
    let mut socket = Socket::connect(key, stop)?;
    let result = exchange(&mut socket, model, pcm, stop, deadline, play);
    let _ = socket.0.close(None);
    result
}

fn check_input(model: &str, pcm: &[u8]) -> Result<(), Fault> {
    if pcm.is_empty() || pcm.len() > IN_LIMIT || !pcm.len().is_multiple_of(2) {
        return Err(Fault::NoAudio);
    }
    model_valid(model).then_some(()).ok_or(Fault::Protocol)
}

fn check(stop: &Cancellation, deadline: Instant) -> Result<(), Fault> {
    if stop.requested() {
        return Err(Fault::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Fault::Deadline);
    }
    Ok(())
}

fn receive(
    session: &mut impl Session,
    stop: &Cancellation,
    deadline: Instant,
) -> Result<Value, Fault> {
    loop {
        check(stop, deadline)?;
        if let Some(reply) = session.receive()? {
            if let Some(error) = reply.get("error") {
                return Err(match error.get("status").and_then(Value::as_str) {
                    Some("RESOURCE_EXHAUSTED") => Fault::Quota,
                    Some("UNAUTHENTICATED" | "PERMISSION_DENIED") => Fault::Credentials,
                    _ => error
                        .get("code")
                        .and_then(Value::as_u64)
                        .and_then(|code| u16::try_from(code).ok())
                        .map_or(Fault::Unavailable, http_status),
                });
            }
            if reply.get("goAway").is_some() {
                return Err(Fault::Rollover);
            }
            if reply.get("toolCall").is_some() {
                return Err(Fault::Protocol);
            }
            return Ok(reply);
        }
    }
}

fn exchange(
    session: &mut impl Session,
    model: &str,
    pcm: &[u8],
    stop: &Cancellation,
    deadline: Instant,
    mut play: impl FnMut(Vec<i16>) -> Result<(), Fault>,
) -> Result<(), Fault> {
    check_input(model, pcm)?;
    check(stop, deadline)?;
    session.send(json!({"setup": {
        "model": format!("models/{model}"),
        "generationConfig": {"responseModalities": ["AUDIO"]},
        "realtimeInputConfig": {"automaticActivityDetection": {"disabled": true}}
    }}))?;
    if receive(session, stop, deadline)?
        .get("setupComplete")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(Fault::Protocol);
    }
    check(stop, deadline)?;
    session.send(json!({"realtimeInput": {"activityStart": {}}}))?;
    for chunk in pcm.chunks(3_200) {
        check(stop, deadline)?;
        session.send(json!({"realtimeInput": {"audio": {
            "mimeType": "audio/pcm;rate=16000",
            "data": STANDARD.encode(chunk)
        }}}))?;
    }
    check(stop, deadline)?;
    session.send(json!({"realtimeInput": {"activityEnd": {}}}))?;

    let mut received = 0;
    loop {
        let reply = receive(session, stop, deadline)?;
        let Some(content) = reply.get("serverContent") else {
            continue;
        };
        if content.get("interrupted").and_then(Value::as_bool) == Some(true) {
            return Err(Fault::Cancelled);
        }
        if let Some(parts) = content
            .pointer("/modelTurn/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                let Some(data) = part.get("inlineData") else {
                    continue;
                };
                let format = data
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .ok_or(Fault::Protocol)?;
                if !matches!(format, "audio/pcm" | "audio/pcm;rate=24000") {
                    return Err(Fault::Protocol);
                }
                let encoded = data
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or(Fault::Protocol)?;
                let bytes = STANDARD.decode(encoded).map_err(|_| Fault::Protocol)?;
                if bytes.is_empty()
                    || !bytes.len().is_multiple_of(2)
                    || bytes.len() > OUT_LIMIT - received
                {
                    return Err(Fault::Protocol);
                }
                received += bytes.len();
                let (pairs, _) = bytes.as_chunks::<2>();
                let pcm = pairs.iter().copied().map(i16::from_le_bytes).collect();
                check(stop, deadline)?;
                play(pcm)?;
            }
        }
        if content.get("turnComplete").and_then(Value::as_bool) == Some(true) {
            return if received > 0 {
                Ok(())
            } else {
                Err(Fault::NoAudio)
            };
        }
    }
}

#[cfg(test)]
mod tests;
