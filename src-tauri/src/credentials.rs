use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    Openai,
    Claude,
    Gemini,
    Groq,
    Openrouter,
    Compatible,
    Ollama,
    Bedrock,
    Vertex,
}
impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Groq => "groq",
            Self::Openrouter => "openrouter",
            Self::Compatible => "compatible",
            Self::Ollama => "ollama",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
        }
    }
    fn env(self) -> &'static str {
        match self {
            Self::Openai => "OPENAI_API_KEY",
            Self::Claude => "ANTHROPIC_API_KEY",
            Self::Gemini => "GEMINI_API_KEY",
            Self::Groq => "GROQ_API_KEY",
            Self::Openrouter => "OPENROUTER_API_KEY",
            Self::Compatible => "OPENAI_COMPATIBLE_API_KEY",
            Self::Ollama => "OLLAMA_API_KEY",
            Self::Bedrock => "AWS_BEARER_TOKEN_BEDROCK",
            Self::Vertex => "VERTEX_ACCESS_TOKEN",
        }
    }
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Storage {
    Os,
    Session,
}
#[derive(Default)]
pub struct Credentials(Mutex<HashMap<String, Zeroizing<String>>>);
impl Credentials {
    fn entry(provider: Provider, scope: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(
            "app.studiocut.director",
            &format!("{}:{scope}", provider.name()),
        )
        .map_err(|_| {
            "OS credential storage unavailable; explicitly choose session-only storage".into()
        })
    }
    pub fn set(
        &self,
        provider: Provider,
        scope: &str,
        key: String,
        storage: Storage,
    ) -> Result<(), String> {
        let key = Zeroizing::new(key);
        if key.trim().is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
            return Err("Invalid credential".into());
        }
        let id = format!("{}:{scope}", provider.name());
        match storage {
            Storage::Os => {
                Self::entry(provider, scope)?
                    .set_password(&key)
                    .map_err(|_| {
                        "OS credential storage unavailable; explicitly choose session-only storage"
                    })?;
                self.0
                    .lock()
                    .map_err(|_| "Credential lock unavailable")?
                    .remove(&id);
            }
            Storage::Session => {
                self.0
                    .lock()
                    .map_err(|_| "Credential lock unavailable")?
                    .insert(id, key);
            }
        }
        Ok(())
    }
    pub fn get(&self, provider: Provider, scope: &str) -> Result<Zeroizing<String>, String> {
        if provider == Provider::Ollama {
            return Ok(Zeroizing::new(String::new()));
        }
        if let Some(key) = self
            .0
            .lock()
            .map_err(|_| "Credential lock unavailable")?
            .get(&format!("{}:{scope}", provider.name()))
        {
            return Ok(key.clone());
        }
        if let Ok(entry) = Self::entry(provider, scope) {
            if let Ok(key) = entry.get_password() {
                return Ok(Zeroizing::new(key));
            }
        }
        if provider != Provider::Compatible {
            if let Ok(key) = std::env::var(provider.env()) {
                if !key.is_empty() {
                    return Ok(Zeroizing::new(key));
                }
            }
        }
        Err(format!(
            "No {} credential. Save one in OS storage or explicitly select session-only.",
            provider.name()
        ))
    }
    pub fn delete(&self, provider: Provider, scope: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "Credential lock unavailable")?
            .remove(&format!("{}:{scope}", provider.name()));
        match Self::entry(provider, scope)?.delete_credential() { Ok(()) | Err(keyring::Error::NoEntry) => Ok(()), Err(_) => Err("Session credential cleared, but OS deletion failed. Environment credentials are unchanged.".into()) }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_keys_are_scoped_and_validated() {
        let store = Credentials::default();
        store
            .set(
                Provider::Compatible,
                "https://one.example/v1",
                "test-only".into(),
                Storage::Session,
            )
            .unwrap();
        assert_eq!(
            &**store
                .get(Provider::Compatible, "https://one.example/v1")
                .unwrap(),
            "test-only"
        );
        assert!(store
            .get(Provider::Compatible, "https://two.example/v1")
            .is_err());
        assert!(store
            .set(Provider::Openai, "", "bad\nkey".into(), Storage::Session)
            .is_err());
    }
}
