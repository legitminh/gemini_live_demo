use serde::Serialize;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub gemini_api_key: Option<String>,
    pub xai_api_key: Option<String>,
    pub model: String,
    pub port: u16,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let gemini_api_key = non_empty("GEMINI_API_KEY").or_else(|| non_empty("GOOGLE_API_KEY"));
        let xai_api_key = non_empty("XAI_API_KEY");
        let model = non_empty("GEMINI_LIVE_MODEL").unwrap_or_else(|| "gemini-3.8-live".to_string());
        let port = std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8080);
        Self {
            gemini_api_key,
            xai_api_key,
            model,
            port,
        }
    }

    pub fn missing_keys(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.gemini_api_key.is_none() {
            missing.push("GEMINI_API_KEY");
        }
        if self.xai_api_key.is_none() {
            missing.push("XAI_API_KEY");
        }
        missing
    }
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Serialize, Clone)]
pub struct VoiceChoice {
    pub id: &'static str,
    pub name: &'static str,
    pub note: &'static str,
}

pub fn voices() -> &'static [VoiceChoice] {
    &[
        VoiceChoice {
            id: "eve",
            name: "Eve",
            note: "Energetic, upbeat",
        },
        VoiceChoice {
            id: "ara",
            name: "Ara",
            note: "Warm, friendly",
        },
        VoiceChoice {
            id: "rex",
            name: "Rex",
            note: "Confident, clear",
        },
        VoiceChoice {
            id: "sal",
            name: "Sal",
            note: "Smooth, balanced",
        },
        VoiceChoice {
            id: "leo",
            name: "Leo",
            note: "Authoritative, strong",
        },
        VoiceChoice {
            id: "aurora",
            name: "Aurora",
            note: "Serene, steady",
        },
        VoiceChoice {
            id: "orion",
            name: "Orion",
            note: "Rich, cinematic",
        },
        VoiceChoice {
            id: "luna",
            name: "Luna",
            note: "Soft, intimate",
        },
        VoiceChoice {
            id: "iris",
            name: "Iris",
            note: "Bright, charming",
        },
        VoiceChoice {
            id: "carina",
            name: "Carina",
            note: "Patient, supportive",
        },
        VoiceChoice {
            id: "atlas",
            name: "Atlas",
            note: "Commanding, calm",
        },
        VoiceChoice {
            id: "helios",
            name: "Helios",
            note: "Upbeat, clear",
        },
        VoiceChoice {
            id: "liora",
            name: "Liora",
            note: "Grounded, luminous",
        },
        VoiceChoice {
            id: "zenith",
            name: "Zenith",
            note: "Sharp, focused",
        },
    ]
}

#[derive(Serialize, Clone)]
pub struct LanguageChoice {
    pub id: &'static str,
    pub name: &'static str,
}

pub fn languages() -> &'static [LanguageChoice] {
    &[
        LanguageChoice {
            id: "en",
            name: "English",
        },
        LanguageChoice {
            id: "auto",
            name: "Auto-detect",
        },
        LanguageChoice {
            id: "es-MX",
            name: "Spanish (Mexico)",
        },
        LanguageChoice {
            id: "es-ES",
            name: "Spanish (Spain)",
        },
        LanguageChoice {
            id: "fr",
            name: "French",
        },
        LanguageChoice {
            id: "de",
            name: "German",
        },
        LanguageChoice {
            id: "pt-BR",
            name: "Portuguese (Brazil)",
        },
        LanguageChoice {
            id: "ja",
            name: "Japanese",
        },
        LanguageChoice {
            id: "zh",
            name: "Chinese",
        },
        LanguageChoice {
            id: "ko",
            name: "Korean",
        },
        LanguageChoice {
            id: "hi",
            name: "Hindi",
        },
        LanguageChoice {
            id: "ar-SA",
            name: "Arabic",
        },
    ]
}

pub fn safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_reject_query_injection() {
        assert!(safe_token("eve"));
        assert!(safe_token("pt-BR"));
        assert!(!safe_token("en&codec=wav"));
        assert!(!safe_token(""));
        assert!(!safe_token("voice id"));
    }
}
