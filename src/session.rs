use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::ws::{Message as ClientMessage, WebSocket};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tracing::{info, warn};

use crate::bridge::{Effect, TurnBridge};
use crate::config::AppConfig;
use crate::gemini::{self, gemini_audio_bytes};
use crate::grok;
use crate::speech::{SpeechAction, SpeechQueue};
use crate::vad::{silence_pcm, MicAction, MicGate};

const MAX_PCM_BYTES: usize = 64 * 1024;
const SAMPLE_RATE: u32 = 24_000;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Inbound {
    Start {
        voice: Option<String>,
        language: Option<String>,
    },
    Audio {
        pcm: String,
    },
    Text {
        text: String,
    },
    Barge,
    /// The browser heard the person stop talking.
    AudioEnd,
    Stop,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Outbound {
    Status {
        phase: String,
    },
    Ready {
        model: String,
        voice: String,
        language: String,
    },
    User {
        text: String,
        #[serde(rename = "final")]
        is_final: bool,
    },
    Assistant {
        text: String,
        #[serde(rename = "final")]
        is_final: bool,
    },
    Audio {
        pcm: String,
        sample_rate: u32,
        epoch: u64,
    },
    AudioEnd {
        epoch: u64,
    },
    ClearAudio {
        epoch: u64,
    },
    Error {
        message: String,
    },
}

enum ClientFrame {
    Event(Outbound),
    Pong(Vec<u8>),
}

pub async fn run(socket: WebSocket, config: Arc<AppConfig>) {
    if let Err(error) = conversation(socket, config).await {
        info!(%error, "conversation ended");
    }
}

async fn conversation(socket: WebSocket, config: Arc<AppConfig>) -> Result<()> {
    let (mut client_write, mut client_read) = socket.split();
    let (to_client, mut client_events) = mpsc::channel::<ClientFrame>(256);
    let writer = tokio::spawn(async move {
        while let Some(frame) = client_events.recv().await {
            let message = match frame {
                ClientFrame::Pong(data) => ClientMessage::Pong(data.into()),
                ClientFrame::Event(event) => {
                    let payload = match serde_json::to_string(&event) {
                        Ok(payload) => payload,
                        Err(error) => {
                            warn!(%error, "failed to encode client event");
                            continue;
                        }
                    };
                    ClientMessage::Text(payload.into())
                }
            };
            if client_write.send(message).await.is_err() {
                break;
            }
        }
    });

    let result = drive(&to_client, &mut client_read, &config).await;
    if let Err(error) = &result {
        let message = public_error(error, &config);
        info!(%message, "conversation ended");
        let _ = send_error(&to_client, &message).await;
    }
    drop(to_client);
    let _ = writer.await;
    result
}

async fn drive(
    to_client: &mpsc::Sender<ClientFrame>,
    client_read: &mut (impl StreamExt<Item = Result<ClientMessage, axum::Error>> + Unpin),
    config: &AppConfig,
) -> Result<()> {
    let start = match client_read.next().await {
        Some(Ok(ClientMessage::Text(text))) => parse_inbound(&text)?,
        Some(Ok(ClientMessage::Close(_))) | None => return Ok(()),
        Some(Ok(_)) => return Err(anyhow!("Expected a start message")),
        Some(Err(error)) => return Err(anyhow!(error)),
    };
    let Inbound::Start { voice, language } = start else {
        send_error(to_client, "Send a start message before audio.").await?;
        return Ok(());
    };

    let missing = config.missing_keys();
    if !missing.is_empty() {
        let names = missing.join(" and ");
        let verb = if missing.len() == 1 { "is" } else { "are" };
        let found = config
            .env_file
            .as_deref()
            .map(|path| format!("Read {path}, but"))
            .unwrap_or_else(|| "No .env file was found, and".to_string());
        send_error(
            to_client,
            &format!(
                "{found} {names} {verb} missing. Use those names in the project .env and click Start again."
            ),
        )
        .await?;
        return Ok(());
    }

    let voice = voice.unwrap_or_else(|| "eve".to_string());
    let language = language.unwrap_or_else(|| "en".to_string());
    if !crate::config::safe_token(&voice) || !crate::config::safe_token(&language) {
        send_error(
            to_client,
            "Voice or language contains unsupported characters.",
        )
        .await?;
        return Ok(());
    }

    send(
        to_client,
        Outbound::Status {
            phase: "connecting".into(),
        },
    )
    .await?;
    let gemini_key = config
        .gemini_api_key
        .as_deref()
        .context("missing Gemini key")?;
    let xai_key = config.xai_api_key.as_deref().context("missing xAI key")?;

    let gemini = match gemini::connect(gemini_key, &config.model).await {
        Ok(socket) => socket,
        Err(error) => {
            send_error(to_client, &public_error(&error, config)).await?;
            return Ok(());
        }
    };
    let grok = match grok::connect(xai_key, &voice, &language).await {
        Ok(socket) => socket,
        Err(error) => {
            send_error(to_client, &public_error(&error, config)).await?;
            return Ok(());
        }
    };
    let (mut gemini_write, mut gemini_read) = gemini.split();
    let (mut grok_write, mut grok_read) = grok.split();

    send(
        to_client,
        Outbound::Ready {
            model: config.model.clone(),
            voice: voice.clone(),
            language: language.clone(),
        },
    )
    .await?;
    send(
        to_client,
        Outbound::Status {
            phase: "listening".into(),
        },
    )
    .await?;
    info!(model = %config.model, %voice, %language, "conversation started");

    let mut bridge = TurnBridge::default();
    let mut speech = SpeechQueue::default();
    let mut gemini_generating = false;
    let mut announced_reply = false;
    let mut suppress_speech = false;
    let mut dropped_gemini_audio = 0usize;
    let mut discard_deadline: Option<tokio::time::Instant> = None;
    let mut mic_gate = MicGate::default();
    let mut forwarded_audio = 0usize;

    loop {
        let discard_wait = async {
            match discard_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };
        let flush_at = mic_gate.deadline();
        let flush_wait = async {
            match flush_at {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            incoming = client_read.next() => {
                let Some(incoming) = incoming else { break };
                let message = incoming.context("browser websocket failed")?;
                match message {
                    ClientMessage::Text(text) => {
                        let inbound = parse_inbound(&text)?;
                        match inbound {
                            Inbound::Start { .. } => {}
                            Inbound::Stop => break,
                            Inbound::Audio { pcm } => {
                                let bytes = decode_pcm(&pcm)?;
                                match mic_gate.observe(&bytes, tokio::time::Instant::now()) {
                                    MicAction::Drop => {}
                                    MicAction::Forward => {
                                        forwarded_audio += bytes.len();
                                        forward_mic_audio(&mut gemini_write, &bytes).await?;
                                    }
                                    MicAction::Close => {
                                        forwarded_audio += bytes.len();
                                        info!(forwarded_audio, "pause after speech");
                                        forwarded_audio = 0;
                                        forward_mic_audio(&mut gemini_write, &bytes).await?;
                                        send_turn_silence(&mut gemini_write).await?;
                                    }
                                }
                            }
                            Inbound::AudioEnd => {
                                if mic_gate.end_now(tokio::time::Instant::now()) {
                                    info!(forwarded_audio, "browser ended the spoken turn");
                                    forwarded_audio = 0;
                                    send_turn_silence(&mut gemini_write).await?;
                                }
                            }
                            Inbound::Text { text } => {
                                let text = text.trim();
                                if text.is_empty() {
                                    continue;
                                }
                                if speech.is_busy() {
                                    let actions = speech.cancel();
                                    apply_actions(
                                        &mut speech,
                                        actions,
                                        to_client,
                                        &mut grok_write,
                                        &mut discard_deadline,
                                    ).await?;
                                }
                                suppress_speech = false;
                                let payload = gemini::text_turn_message(text).to_string();
                                gemini_write
                                    .send(UpstreamMessage::Text(payload.into()))
                                    .await
                                    .context("failed to send text to Gemini")?;
                                send(to_client, Outbound::User { text: text.to_string(), is_final: true }).await?;
                                send(to_client, Outbound::Status { phase: "thinking".into() }).await?;
                            }
                            Inbound::Barge => {
                                if speech.is_busy() {
                                    let actions = speech.cancel();
                                    apply_actions(
                                        &mut speech,
                                        actions,
                                        to_client,
                                        &mut grok_write,
                                        &mut discard_deadline,
                                    ).await?;
                                    if gemini_generating {
                                        suppress_speech = true;
                                    }
                                    send(to_client, Outbound::Status { phase: "listening".into() }).await?;
                                }
                            }
                        }
                    }
                    ClientMessage::Ping(data) => {
                        to_client
                            .send(ClientFrame::Pong(data.to_vec()))
                            .await
                            .ok();
                    }
                    ClientMessage::Close(_) => break,
                    ClientMessage::Pong(_) | ClientMessage::Binary(_) => {}
                }
            }
            incoming = gemini_read.next() => {
                let Some(incoming) = incoming else {
                    send_error(to_client, "Gemini Live closed the session.").await?;
                    break;
                };
                let message = incoming.context("Gemini websocket failed")?;
                match message {
                    UpstreamMessage::Ping(data) => {
                        gemini_write.send(UpstreamMessage::Pong(data)).await.ok();
                    }
                    UpstreamMessage::Close(frame) => {
                        let reason = frame
                            .map(|frame| frame.reason.to_string())
                            .filter(|reason| !reason.is_empty())
                            .unwrap_or_else(|| "connection closed".into());
                        send_error(to_client, &format!("Gemini Live closed the session: {reason}")).await?;
                        break;
                    }
                    other => {
                        let Some(payload) = gemini::text_payload(other) else {
                            continue;
                        };
                        let value: serde_json::Value = match serde_json::from_str(&payload) {
                            Ok(value) => value,
                            Err(_) => continue,
                        };
                        if let Some(message) = gemini::error_message(&value) {
                            send_error(to_client, &format!("Gemini Live: {message}")).await?;
                            break;
                        }
                        if value.get("goAway").is_some() {
                            send_error(to_client, "Gemini Live is ending this session.").await?;
                            break;
                        }
                        if let Some(response) = gemini::tool_response(&value) {
                            gemini_write
                                .send(UpstreamMessage::Text(response.to_string().into()))
                                .await
                                .ok();
                        }
                        dropped_gemini_audio += gemini_audio_bytes(&value);
                        for signal in gemini::signals_from_message(&value) {
                            let interrupted = matches!(signal, crate::bridge::Signal::Interrupted);
                            let finished = matches!(
                                signal,
                                crate::bridge::Signal::GenerationComplete
                                    | crate::bridge::Signal::TurnComplete
                                    | crate::bridge::Signal::Interrupted
                            );
                            if matches!(signal, crate::bridge::Signal::AssistantFragment(_)) {
                                gemini_generating = true;
                                if !suppress_speech && !announced_reply {
                                    announced_reply = true;
                                    send(to_client, Outbound::Status { phase: "thinking".into() }).await?;
                                }
                            }
                            for effect in bridge.handle(signal) {
                                apply_effect(
                                    effect,
                                    suppress_speech,
                                    &mut speech,
                                    to_client,
                                    &mut grok_write,
                                    &mut discard_deadline,
                                ).await?;
                            }
                            if interrupted || finished {
                                gemini_generating = false;
                                announced_reply = false;
                                suppress_speech = false;
                                if dropped_gemini_audio > 0 {
                                    tracing::debug!(
                                        bytes = dropped_gemini_audio,
                                        "discarded Gemini audio; Grok Voice is speaking"
                                    );
                                    dropped_gemini_audio = 0;
                                }
                            }
                        }
                    }
                }
            }
            incoming = grok_read.next() => {
                let Some(incoming) = incoming else {
                    send_error(to_client, "Grok Voice closed the session.").await?;
                    break;
                };
                let message = incoming.context("Grok websocket failed")?;
                let event = match message {
                    UpstreamMessage::Text(text) => grok::parse_event(&text),
                    UpstreamMessage::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => grok::parse_event(&text),
                        Err(_) => grok::GrokEvent::Error("Grok sent unexpected binary audio".into()),
                    },
                    UpstreamMessage::Ping(data) => {
                        grok_write.send(UpstreamMessage::Pong(data)).await.ok();
                        continue;
                    }
                    UpstreamMessage::Close(frame) => {
                        let reason = frame
                            .map(|frame| frame.reason.to_string())
                            .unwrap_or_else(|| "connection closed".into());
                        send_error(to_client, &format!("Grok Voice closed the session: {reason}")).await?;
                        break;
                    }
                    UpstreamMessage::Pong(_) | UpstreamMessage::Frame(_) => continue,
                };
                let failed = matches!(event, grok::GrokEvent::Error(_));
                let actions = speech.on_grok(event);
                let hard_fail = apply_actions(
                    &mut speech,
                    actions,
                    to_client,
                    &mut grok_write,
                    &mut discard_deadline,
                ).await?;
                if failed && hard_fail {
                    break;
                }
            }
            _ = discard_wait => {
                discard_deadline = None;
                let actions = speech.force_ready();
                apply_actions(&mut speech, actions, to_client, &mut grok_write, &mut discard_deadline).await?;
            }
            _ = flush_wait => {
                if mic_gate.end_now(tokio::time::Instant::now()) {
                    info!(forwarded_audio, "pause timer after speech");
                    forwarded_audio = 0;
                    send_turn_silence(&mut gemini_write).await?;
                }
            }
        }
    }

    let _ = gemini_write
        .send(UpstreamMessage::Text(
            gemini::audio_stream_end_message().to_string().into(),
        ))
        .await;
    Ok(())
}

async fn apply_effect(
    effect: Effect,
    suppress_speech: bool,
    speech: &mut SpeechQueue,
    to_client: &mpsc::Sender<ClientFrame>,
    grok_write: &mut (impl SinkExt<UpstreamMessage, Error = tokio_tungstenite::tungstenite::Error>
              + Unpin),
    discard_deadline: &mut Option<tokio::time::Instant>,
) -> Result<()> {
    match effect {
        Effect::User { text, is_final } => {
            send(to_client, Outbound::User { text, is_final }).await?;
        }
        Effect::Assistant { text, is_final } => {
            send(to_client, Outbound::Assistant { text, is_final }).await?;
        }
        Effect::Speak(text) if !suppress_speech => {
            let actions = speech.speak(text);
            apply_actions(speech, actions, to_client, grok_write, discard_deadline).await?;
        }
        Effect::Speak(_) => {}
        Effect::FinishSpeech if !suppress_speech => {
            let actions = speech.finish();
            apply_actions(speech, actions, to_client, grok_write, discard_deadline).await?;
        }
        Effect::FinishSpeech => {}
        Effect::CancelSpeech => {
            let actions = speech.cancel();
            apply_actions(speech, actions, to_client, grok_write, discard_deadline).await?;
            send(
                to_client,
                Outbound::Status {
                    phase: "listening".into(),
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn apply_actions(
    speech: &mut SpeechQueue,
    actions: Vec<SpeechAction>,
    to_client: &mpsc::Sender<ClientFrame>,
    grok_write: &mut (impl SinkExt<UpstreamMessage, Error = tokio_tungstenite::tungstenite::Error>
              + Unpin),
    discard_deadline: &mut Option<tokio::time::Instant>,
) -> Result<bool> {
    let mut hard_fail = false;
    for action in actions {
        match action {
            SpeechAction::Send(payload) => {
                if payload.contains("text.clear") {
                    *discard_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(2));
                }
                grok_write
                    .send(UpstreamMessage::Text(payload.into()))
                    .await
                    .context("failed to write to Grok Voice")?;
            }
            SpeechAction::Audio { bytes, epoch } => {
                let first = speech.note_playback(epoch);
                send(
                    to_client,
                    Outbound::Audio {
                        pcm: base64::engine::general_purpose::STANDARD.encode(bytes),
                        sample_rate: SAMPLE_RATE,
                        epoch,
                    },
                )
                .await?;
                if first {
                    send(
                        to_client,
                        Outbound::Status {
                            phase: "speaking".into(),
                        },
                    )
                    .await?;
                }
            }
            SpeechAction::Ended { epoch } => {
                send(to_client, Outbound::AudioEnd { epoch }).await?;
                send(
                    to_client,
                    Outbound::Status {
                        phase: "listening".into(),
                    },
                )
                .await?;
            }
            SpeechAction::Cleared { epoch } => {
                send(to_client, Outbound::ClearAudio { epoch }).await?;
            }
            SpeechAction::Failed(message) => {
                let benign = message.to_ascii_lowercase().contains("no active")
                    || message.to_ascii_lowercase().contains("nothing to")
                    || message.to_ascii_lowercase().contains("not currently");
                if benign {
                    tracing::debug!(%message, "ignored grok clear error");
                    let actions = speech.force_ready();
                    *discard_deadline = None;
                    Box::pin(apply_actions(
                        speech,
                        actions,
                        to_client,
                        grok_write,
                        discard_deadline,
                    ))
                    .await?;
                } else {
                    send_error(to_client, &format!("Grok Voice: {message}")).await?;
                    hard_fail = true;
                }
            }
        }
    }
    Ok(hard_fail)
}

async fn forward_mic_audio(
    gemini_write: &mut (impl SinkExt<UpstreamMessage, Error = tokio_tungstenite::tungstenite::Error>
              + Unpin),
    pcm: &[u8],
) -> Result<()> {
    let payload = gemini::audio_message(pcm).to_string();
    gemini_write
        .send(UpstreamMessage::Text(payload.into()))
        .await
        .context("failed to forward audio to Gemini")
}

async fn send_turn_silence(
    gemini_write: &mut (impl SinkExt<UpstreamMessage, Error = tokio_tungstenite::tungstenite::Error>
              + Unpin),
) -> Result<()> {
    info!("user stopped talking; sending a pause so Gemini can answer");
    forward_mic_audio(gemini_write, &silence_pcm()).await
}

fn parse_inbound(text: &str) -> Result<Inbound> {
    serde_json::from_str(text).context("the browser sent a message this server does not understand")
}

fn decode_pcm(pcm: &str) -> Result<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(pcm)
        .context("audio chunk was not valid base64")?;
    if bytes.len() > MAX_PCM_BYTES {
        return Err(anyhow!("audio chunk is too large"));
    }
    if bytes.len() % 2 != 0 {
        return Err(anyhow!("audio chunk must be 16-bit PCM"));
    }
    Ok(bytes)
}

async fn send(to_client: &mpsc::Sender<ClientFrame>, event: Outbound) -> Result<()> {
    to_client
        .send(ClientFrame::Event(event))
        .await
        .map_err(|_| anyhow!("browser disconnected"))
}

async fn send_error(to_client: &mpsc::Sender<ClientFrame>, message: &str) -> Result<()> {
    send(
        to_client,
        Outbound::Error {
            message: message.chars().take(500).collect(),
        },
    )
    .await
}

fn public_error(error: &anyhow::Error, config: &AppConfig) -> String {
    crate::config::redact(
        &error.to_string(),
        &[
            config.gemini_api_key.as_deref(),
            config.xai_api_key.as_deref(),
        ],
    )
}
