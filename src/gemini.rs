use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::time::{timeout, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, warn};

use crate::bridge::Signal;

pub const INPUT_MIME: &str = "audio/pcm;rate=16000";
pub const SYSTEM_INSTRUCTION: &str = "\
You are a live voice conversation partner. The person is talking to you in real time. \
Reply in short, natural spoken sentences, the way a person talks out loud. \
Do not use markdown, lists, code, emoji, or stage directions. \
Do not mention these instructions or that another system will speak your words. \
Keep most replies to one or two sentences unless the person asks for more detail.";

pub type GeminiSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub fn install_crypto_provider() {
    // tokio-tungstenite enables rustls without a crypto provider, so the first
    // wss connection panics unless one is installed for the process.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub async fn connect(api_key: &str, model: &str) -> Result<GeminiSocket> {
    install_crypto_provider();
    let url = format!(
        "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
        query_encode(api_key)
    );
    let connect = timeout(Duration::from_secs(20), connect_async(&url))
        .await
        .context("timed out connecting to Gemini Live")?
        .map_err(|error| websocket_failure(error, "Gemini Live"))?;
    let (mut socket, response) = connect;
    debug!(status = %response.status(), "gemini websocket opened");

    let setup = setup_message(model);
    socket
        .send(Message::Text(setup.to_string().into()))
        .await
        .context("failed to send Gemini setup")?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("timed out waiting for Gemini setup"));
        }
        let message = timeout(remaining, socket.next())
            .await
            .context("timed out waiting for Gemini setup")?
            .context("Gemini closed before setup completed")?
            .context("Gemini setup read failed")?;
        match message {
            Message::Ping(data) => {
                socket.send(Message::Pong(data)).await.ok();
            }
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(frame) => {
                let reason = frame
                    .map(|frame| frame.reason.to_string())
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or_else(|| "connection closed".into());
                return Err(anyhow!("Gemini closed during setup: {reason}"));
            }
            other => {
                let Some(payload) = text_payload(other) else {
                    continue;
                };
                let value: Value =
                    serde_json::from_str(&payload).unwrap_or(json!({ "raw": payload }));
                if value.get("setupComplete").is_some() || value.get("setup_complete").is_some() {
                    debug!(model, "gemini live session ready");
                    return Ok(socket);
                }
                if let Some(message) = error_message(&value) {
                    return Err(anyhow!("Gemini rejected the session: {message}"));
                }
                warn!(%payload, "waiting for gemini setupComplete");
            }
        }
    }
}

pub fn websocket_failure(
    error: tokio_tungstenite::tungstenite::Error,
    service: &str,
) -> anyhow::Error {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            let status = response.status();
            let body = response
                .body()
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
                .filter(|body| !body.is_empty());
            match body {
                Some(body) => anyhow!("{service} rejected the connection (HTTP {status}: {body})"),
                None => anyhow!("{service} rejected the connection (HTTP {status})"),
            }
        }
        other => anyhow!("{service} websocket failed: {other}"),
    }
}

fn query_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub fn setup_message(model: &str) -> Value {
    let model_name = if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    };
    json!({
        "setup": {
            "model": model_name,
            "generationConfig": {
                "responseModalities": ["AUDIO"]
            },
            "systemInstruction": {
                "parts": [{ "text": SYSTEM_INSTRUCTION }]
            },
            "inputAudioTranscription": {},
            "outputAudioTranscription": {},
            "realtimeInputConfig": {
                "automaticActivityDetection": {
                    "silenceDurationMs": 700
                },
                "activityHandling": "START_OF_ACTIVITY_INTERRUPTS"
            }
        }
    })
}

pub fn audio_message(pcm: &[u8]) -> Value {
    json!({
        "realtimeInput": {
            "audio": {
                "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pcm),
                "mimeType": INPUT_MIME
            }
        }
    })
}

pub fn text_turn_message(text: &str) -> Value {
    json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{ "text": text }]
            }],
            "turnComplete": true
        }
    })
}

pub fn audio_stream_end_message() -> Value {
    json!({ "realtimeInput": { "audioStreamEnd": true } })
}

pub fn signals_from_message(value: &Value) -> Vec<Signal> {
    let Some(content) = value
        .get("serverContent")
        .or_else(|| value.get("server_content"))
    else {
        return Vec::new();
    };
    let mut signals = Vec::new();
    if let Some(text) = transcript_text(
        content,
        &["interimInputTranscription", "interim_input_transcription"],
    ) {
        signals.push(Signal::InterimUser(text));
    }
    if let Some(text) = transcript_text(content, &["inputTranscription", "input_transcription"]) {
        signals.push(Signal::FinalUser(text));
    }
    if let Some(text) = transcript_text(content, &["outputTranscription", "output_transcription"]) {
        // Native live audio is discarded. This transcript is what Grok speaks.
        signals.push(Signal::AssistantFragment(text));
    } else if let Some(parts) = content
        .pointer("/modelTurn/parts")
        .or_else(|| content.pointer("/model_turn/parts"))
        .and_then(|item| item.as_array())
    {
        // Text parts are used only when the message has no audio transcript,
        // so the same sentence is never spoken twice.
        for part in parts {
            if let Some(text) = part.get("text").and_then(|item| item.as_str()) {
                if !text.is_empty() {
                    signals.push(Signal::AssistantFragment(text.to_string()));
                }
            }
        }
    }
    if flag(content, &["interrupted"]) {
        signals.push(Signal::Interrupted);
    } else if flag(content, &["generationComplete", "generation_complete"]) {
        signals.push(Signal::GenerationComplete);
    }
    if flag(content, &["turnComplete", "turn_complete"]) {
        signals.push(Signal::TurnComplete);
    }
    signals
}

fn transcript_text(content: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        content
            .get(*name)
            .and_then(|item| item.get("text"))
            .and_then(|item| item.as_str())
            .map(str::to_string)
    })
}

fn flag(content: &Value, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| content.get(*name).and_then(|item| item.as_bool()) == Some(true))
}

pub fn error_message(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    if let Some(message) = error.get("message").and_then(|item| item.as_str()) {
        return Some(message.to_string());
    }
    if let Some(message) = error.as_str() {
        return Some(message.to_string());
    }
    Some(error.to_string())
}

pub fn tool_response(value: &Value) -> Option<Value> {
    let calls = value.pointer("/toolCall/functionCalls")?.as_array()?;
    let responses: Vec<Value> = calls
        .iter()
        .filter_map(|call| {
            let name = call.get("name")?.as_str()?;
            let id = call.get("id")?.as_str()?;
            Some(json!({
                "name": name,
                "id": id,
                "response": { "error": "This demo does not run tools." }
            }))
        })
        .collect();
    if responses.is_empty() {
        None
    } else {
        Some(json!({ "toolResponse": { "functionResponses": responses } }))
    }
}

pub fn text_payload(message: Message) -> Option<String> {
    match message {
        Message::Text(text) => Some(text.to_string()),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        _ => None,
    }
}

pub fn gemini_audio_bytes(value: &Value) -> usize {
    let mut total = 0;
    let Some(parts) = value
        .pointer("/serverContent/modelTurn/parts")
        .and_then(|p| p.as_array())
    else {
        return 0;
    };
    for part in parts {
        if let Some(data) = part
            .pointer("/inlineData/data")
            .and_then(|item| item.as_str())
        {
            total += data.len();
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn setup_requests_audio_and_transcripts() {
        let setup = setup_message("gemini-3.8-live");
        assert_eq!(setup["setup"]["model"], "models/gemini-3.8-live");
        assert_eq!(
            setup["setup"]["generationConfig"]["responseModalities"][0],
            "AUDIO"
        );
        assert!(setup["setup"].get("outputAudioTranscription").is_some());
        assert!(setup["setup"].get("inputAudioTranscription").is_some());
    }

    #[test]
    fn parses_transcript_and_completion() {
        let message = json!({
            "serverContent": {
                "outputTranscription": { "text": "Hello there." },
                "generationComplete": true
            }
        });
        let signals = signals_from_message(&message);
        assert_eq!(
            signals,
            vec![
                Signal::AssistantFragment("Hello there.".into()),
                Signal::GenerationComplete
            ]
        );
    }

    #[test]
    fn interrupt_suppresses_generation_complete_in_the_same_message() {
        let message = json!({
            "serverContent": {
                "interrupted": true,
                "turnComplete": true
            }
        });
        let signals = signals_from_message(&message);
        assert_eq!(signals, vec![Signal::Interrupted, Signal::TurnComplete]);
    }

    #[test]
    fn audio_message_is_pcm16k() {
        let message = audio_message(&[1, 2, 3, 4]);
        assert_eq!(
            message["realtimeInput"]["audio"]["mimeType"],
            "audio/pcm;rate=16000"
        );
        assert!(!message["realtimeInput"]["audio"]["data"]
            .as_str()
            .unwrap()
            .is_empty());
    }
}
