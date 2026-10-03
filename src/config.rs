use serde::Serialize;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub gemini_api_key: Option<String>,
    pub xai_api_key: Option<String>,
    pub model: String,
    pub port: u16,
    pub env_file: Option<String>,
}

impl AppConfig {
    /// Read `.env` again, then the process environment. Safe to call per request
    /// so saving the file and clicking Start does not require a restart.
    pub fn load() -> Self {
        let env_file = load_dotenv().map(|path| path.display().to_string());
        Self {
            env_file,
            ..Self::from_env()
        }
    }

    pub fn from_env() -> Self {
        let gemini_api_key =
            first_secret(&["GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_GENAI_API_KEY"]);
        let xai_api_key = first_secret(&["XAI_API_KEY", "GROK_API_KEY"]);
        let model =
            first_secret(&["GEMINI_LIVE_MODEL"]).unwrap_or_else(|| "gemini-3.8-live".to_string());
        let port = std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8080);
        Self {
            gemini_api_key,
            xai_api_key,
            model,
            port,
            env_file: None,
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

fn first_secret(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .and_then(|value| clean_secret(&value))
    })
}

pub fn clean_secret(raw: &str) -> Option<String> {
    let mut value = raw.trim();
    if value.starts_with('\u{feff}') {
        value = value.trim_start_matches('\u{feff}').trim();
    }
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let quote = bytes[0];
        if (quote == b'"' || quote == b'\'') && bytes[bytes.len() - 1] == quote {
            value = value[1..value.len() - 1].trim();
        }
    }
    if value.is_empty() || is_placeholder(value) {
        None
    } else {
        Some(value.to_string())
    }
}

fn is_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower == "changeme"
        || lower.contains("your_api_key")
        || lower.contains("your-api-key")
        || lower.contains("paste")
        || lower.contains("todo")
}

/// Directories to search, nearest first. Includes parents of `target/debug`
/// so a binary launched outside the project still finds the project `.env`.
pub fn env_candidates(start: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    let mut dir = Some(start);
    for _ in 0..8 {
        let Some(current) = dir else { break };
        paths.push(current.join(".env"));
        dir = current.parent();
    }
    paths
}

pub fn load_dotenv() -> Option<std::path::PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            starts.push(dir.to_path_buf());
        }
    }

    let mut seen = std::collections::HashSet::new();
    for start in starts {
        for path in env_candidates(&start) {
            if !seen.insert(path.clone()) || !path.is_file() {
                continue;
            }
            match read_dotenv(&path) {
                Ok(()) => return Some(path),
                Err(error) => eprintln!("failed to read {}: {error}", path.display()),
            }
        }
    }
    None
}

fn read_dotenv(path: &std::path::Path) -> Result<(), dotenvy::Error> {
    let text = std::fs::read_to_string(path).map_err(dotenvy::Error::Io)?;
    let text = text.trim_start_matches('\u{feff}');
    dotenvy::from_read_override(text.as_bytes())
}

pub fn redact(text: &str, secrets: &[Option<&str>]) -> String {
    let mut redacted = text.to_string();
    for secret in secrets.iter().flatten() {
        if secret.len() >= 8 {
            redacted = redacted.replace(secret, "[redacted]");
        }
    }
    redacted.chars().take(500).collect()
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
    fn secrets_ignore_quotes_and_placeholders() {
        assert_eq!(clean_secret("  \"abc12345\" ").as_deref(), Some("abc12345"));
        assert_eq!(clean_secret("'xai-test'").as_deref(), Some("xai-test"));
        assert_eq!(clean_secret(""), None);
        assert_eq!(clean_secret("your_api_key"), None);
        assert_eq!(clean_secret("paste-key-here"), None);
    }

    #[test]
    fn target_debug_binary_still_sees_the_project_env_file() {
        let start = std::path::Path::new("/Users/minh/Projects/cs/gemini_live_demo/target/debug");
        let paths = env_candidates(start);
        assert!(paths
            .iter()
            .any(|path| path
                == std::path::Path::new("/Users/minh/Projects/cs/gemini_live_demo/.env")));
    }

    #[test]
    fn redaction_removes_secrets_from_errors() {
        let text = redact("rejected key AIzaSyTESTKEY", &[Some("AIzaSyTESTKEY")]);
        assert!(!text.contains("AIzaSyTESTKEY"));
        assert!(text.contains("[redacted]"));
    }

    #[test]
    fn tokens_reject_query_injection() {
        assert!(safe_token("eve"));
        assert!(safe_token("pt-BR"));
        assert!(!safe_token("en&codec=wav"));
        assert!(!safe_token(""));
        assert!(!safe_token("voice id"));
    }
}
