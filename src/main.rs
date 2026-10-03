mod bridge;
mod config;
mod gemini;
mod grok;
mod session;
mod speech;

use std::sync::Arc;

use axum::{
    extract::{State, WebSocketUpgrade},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

use crate::config::{languages, voices, AppConfig};

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    /// Production re-reads `.env` on each status check and conversation.
    /// Tests pin a config so they do not depend on the developer machine.
    reload_env: bool,
}

#[derive(Serialize)]
struct StatusBody {
    gemini: bool,
    xai: bool,
    model: String,
    env_file: Option<String>,
    missing: Vec<&'static str>,
    voices: &'static [config::VoiceChoice],
    languages: &'static [config::LanguageChoice],
}

#[tokio::main]
async fn main() {
    let config = Arc::new(AppConfig::load());
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("gemini_grok_voice=info")),
        )
        .init();

    let missing = config.missing_keys();
    if missing.is_empty() {
        tracing::info!(
            model = %config.model,
            port = config.port,
            env_file = config.env_file.as_deref().unwrap_or("environment"),
            "starting voice booth"
        );
    } else {
        tracing::warn!(
            missing = %missing.join(", "),
            env_file = config.env_file.as_deref().unwrap_or("none"),
            port = config.port,
            "API keys are missing; the page will say which names to set"
        );
    }

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
        .await
        .expect("failed to bind port");
    let address = listener.local_addr().expect("local address");
    tracing::info!("open http://{address}");
    axum::serve(listener, router_with(config, true))
        .await
        .expect("server exited");
}

#[cfg(test)]
fn router(config: Arc<AppConfig>) -> Router {
    router_with(config, false)
}

fn router_with(config: Arc<AppConfig>, reload_env: bool) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/pcm-worklet.js", get(worklet))
        .route("/api/status", get(status))
        .route("/ws", get(ws))
        .with_state(AppState { config, reload_env })
}

async fn index() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../assets/index.html"),
    )
        .into_response()
}

async fn worklet() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../assets/pcm-worklet.js"),
    )
        .into_response()
}

async fn status(State(state): State<AppState>) -> Json<StatusBody> {
    let config = if state.reload_env {
        AppConfig::load()
    } else {
        (*state.config).clone()
    };
    Json(status_body(&config))
}

fn status_body(config: &AppConfig) -> StatusBody {
    StatusBody {
        gemini: config.gemini_api_key.is_some(),
        xai: config.xai_api_key.is_some(),
        model: config.model.clone(),
        env_file: config.env_file.clone(),
        missing: config.missing_keys(),
        voices: voices(),
        languages: languages(),
    }
}

async fn ws(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    let config = if state.reload_env {
        Arc::new(AppConfig::load())
    } else {
        state.config.clone()
    };
    upgrade.on_upgrade(move |socket| session::run(socket, config))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn test_config() -> Arc<AppConfig> {
        Arc::new(AppConfig {
            gemini_api_key: None,
            xai_api_key: None,
            model: "gemini-3.8-live".into(),
            port: 0,
            env_file: None,
        })
    }

    #[tokio::test]
    async fn status_reports_missing_keys_without_leaking_them() {
        let response = router(test_config())
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["gemini"], false);
        assert_eq!(body["xai"], false);
        assert_eq!(body["model"], "gemini-3.8-live");
        assert!(body["voices"].as_array().unwrap().len() >= 5);
    }

    #[tokio::test]
    async fn websocket_tells_the_browser_when_keys_are_missing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(test_config());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        use futures_util::{SinkExt, StreamExt};
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"type":"start","voice":"eve","language":"en"}"#.into(),
            ))
            .await
            .unwrap();
        let message = socket.next().await.unwrap().unwrap();
        let text = message.to_text().unwrap();
        assert!(text.contains("GEMINI_API_KEY"));
        assert!(text.contains("XAI_API_KEY"));
        assert!(!text.contains("sk-"));
    }

    #[tokio::test]
    async fn index_describes_the_pipeline() {
        let response = router(test_config())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(html.contains("Gemini Live"));
        assert!(html.contains("Grok Voice"));
        assert!(html.contains("pcm-worklet.js"));
    }
}
