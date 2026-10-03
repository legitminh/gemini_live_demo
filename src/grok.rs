use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use tokio::time::{timeout, Duration};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::header},
};
use tracing::debug;

use crate::config::safe_token;

pub type GrokSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokEvent {
    Audio(Vec<u8>),
    Done,
    Cleared,
    Error(String),
    Ignore,
}

pub fn stream_url(voice: &str, language: &str) -> Result<String> {
    if !safe_token(voice) {
        return Err(anyhow!("Voice id contains unsupported characters"));
    }
    if !safe_token(language) {
        return Err(anyhow!("Language code contains unsupported characters"));
    }
    Ok(format!(
        "wss://api.x.ai/v1/tts?language={language}&voice={voice}&codec=pcm&sample_rate=24000&speed=1.0&optimize_streaming_latency=1&text_normalization=true"
    ))
}

pub async fn connect(api_key: &str, voice: &str, language: &str) -> Result<GrokSocket> {
    crate::gemini::install_crypto_provider();
    let url = stream_url(voice, language)?;
    let mut request = url.into_client_request().context("invalid Grok TTS url")?;
    let header_value = format!("Bearer {api_key}")
        .parse()
        .context("invalid xAI API key header")?;
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, header_value);
    let (socket, response) = timeout(Duration::from_secs(20), connect_async(request))
        .await
        .context("timed out connecting to Grok Voice")?
        .map_err(|error| crate::gemini::websocket_failure(error, "Grok Voice"))?;
    debug!(status = %response.status(), voice, language, "grok tts websocket opened");
    Ok(socket)
}

pub fn text_delta(text: &str) -> String {
    json!({ "type": "text.delta", "delta": text }).to_string()
}

pub fn text_done() -> String {
    json!({ "type": "text.done" }).to_string()
}

pub fn text_clear() -> String {
    json!({ "type": "text.clear" }).to_string()
}

pub fn parse_event(payload: &str) -> GrokEvent {
    let value: Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(_) => {
            return GrokEvent::Error("Grok sent a message that was not JSON".into());
        }
    };
    match value.get("type").and_then(|item| item.as_str()) {
        Some("audio.delta") => {
            let encoded = value
                .get("delta")
                .and_then(|item| item.as_str())
                .unwrap_or("");
            match base64::engine::general_purpose::STANDARD.decode(encoded) {
                Ok(bytes) => GrokEvent::Audio(bytes),
                Err(error) => {
                    GrokEvent::Error(format!("Grok audio chunk was not valid base64: {error}"))
                }
            }
        }
        Some("audio.done") => GrokEvent::Done,
        Some("audio.clear") => GrokEvent::Cleared,
        Some("session.updated") => GrokEvent::Ignore,
        Some("error") => {
            let message = value
                .get("message")
                .and_then(|item| item.as_str())
                .unwrap_or("Grok Voice returned an error");
            GrokEvent::Error(message.to_string())
        }
        _ => GrokEvent::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_uses_pcm_for_streaming_playback() {
        let url = stream_url("ara", "en").unwrap();
        assert!(url.contains("voice=ara"));
        assert!(url.contains("codec=pcm"));
        assert!(url.contains("sample_rate=24000"));
        assert!(url.contains("text_normalization=true"));
        assert!(stream_url("eve&x", "en").is_err());
    }

    #[test]
    fn parses_audio_and_control_events() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2, 3]);
        let payload = format!(r#"{{"type":"audio.delta","delta":"{encoded}"}}"#);
        assert_eq!(parse_event(&payload), GrokEvent::Audio(vec![0, 1, 2, 3]));
        assert_eq!(parse_event(r#"{"type":"audio.done"}"#), GrokEvent::Done);
        assert_eq!(parse_event(r#"{"type":"audio.clear"}"#), GrokEvent::Cleared);
        assert_eq!(
            parse_event(r#"{"type":"error","message":"nope"}"#),
            GrokEvent::Error("nope".into())
        );
    }

    #[test]
    fn client_messages_match_the_streaming_protocol() {
        assert!(text_delta("Hi").contains("text.delta"));
        assert!(text_done().contains("text.done"));
        assert!(text_clear().contains("text.clear"));
    }
}
