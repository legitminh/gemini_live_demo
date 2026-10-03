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

pub async fn connect(api_key: &str, model: &str) -> Result<GeminiSocket> {
    let url = format!(
        "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={api_key}"
    );
    let (mut socket, response) = timeout(Duration::from_secs(20), connect_async(&url))
        .await
        .context("timed out connecting to Gemini Live")?
        .context("Gemini Live websocket failed")?;
    debug!(status = %response.status(), "gemini websocket opened");

    let setup = setup_message(model);
    socket
        .send(Message::Text(setup.to_string().into()))
        .await
        .context("failed to send Gemini setup")?;

    let deadline = Duration::from_secs(20);
    let first = timeout(deadline, socket.next())
        .await
        .context("timed out waiting for Gemini setup")?
        .context("Gemini closed before setup completed")?
        .context("Gemini setup read failed")?;

    match text_payload(first) {
        Some(payload) => {
            let value: Value = serde_json::from_str(&payload).unwrap_or(json!({ "raw": payload }));
            if value.get("setupComplete").is_some() {
                debug!(model, "gemini live session ready");
                return Ok(socket);
            }
            if let Some(message) = error_message(&value) {
                return Err(anyhow!("Gemini rejected the session: {message}"));
            }
            warn!(%payload, "unexpected first gemini message");
            Err(anyhow!("Gemini did not confirm setup: {payload}"))
        }
        None => Err(anyhow!("Gemini setup response was not text")),
    }
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
            "responseModalities": ["AUDIO"],
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "temperature": 0.8
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
    let Some(content) = value.get("serverContent") else {
        return Vec::new();
    };
    let mut signals = Vec::new();
    if let Some(text) = content
        .get("interimInputTranscription")
        .and_then(|item| item.get("text"))
        .and_then(|item| item.as_str())
    {
        signals.push(Signal::InterimUser(text.to_string()));
    }
    if let Some(text) = content
        .get("inputTranscription")
        .and_then(|item| item.get("text"))
        .and_then(|item| item.as_str())
    {
        signals.push(Signal::FinalUser(text.to_string()));
    }
    if let Some(text) = content
        .get("outputTranscription")
        .and_then(|item| item.get("text"))
        .and_then(|item| item.as_str())
    {
        // Native live audio is discarded. This transcript is what Grok speaks.
        signals.push(Signal::AssistantFragment(text.to_string()));
    } else if let Some(parts) = content
        .pointer("/modelTurn/parts")
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
    if content.get("interrupted").and_then(|item| item.as_bool()) == Some(true) {
        signals.push(Signal::Interrupted);
    } else if content
        .get("generationComplete")
        .and_then(|item| item.as_bool())
        == Some(true)
    {
        signals.push(Signal::GenerationComplete);
    }
    if content.get("turnComplete").and_then(|item| item.as_bool()) == Some(true) {
        signals.push(Signal::TurnComplete);
    }
    signals
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
