//! Phase 4 §4.2 — optional LLM cleanup pass.
//!
//! Time-budgeted (default 800 ms). On timeout or error we fall back to the
//! input — the dictation hot path must not regress because a network call
//! flaked. Local llama.cpp sidecar is deferred per the plan.

use std::time::Duration;

const SYSTEM_PROMPT: &str = "Fix only punctuation, capitalization, and obvious dictation errors. Do not rephrase. Preserve all proper nouns and code-like tokens exactly. Return only the corrected text with no quoting or commentary.";

/// Where to run the cleanup pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupBackend {
    Off,
    Groq,
}

impl CleanupBackend {
    pub fn from_str(s: &str) -> Self {
        match s {
            "groq" => CleanupBackend::Groq,
            _ => CleanupBackend::Off,
        }
    }
}

/// Quality preset for the Groq cleanup pass. `Quality` uses Llama-3.3 70B
/// for noticeably better punctuation / casing restoration; `Fast` uses the
/// 8B-instant for ~150–400 ms RTT when the user prefers minimum latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupModel {
    Fast,
    Quality,
}

impl CleanupModel {
    pub fn from_str(s: &str) -> Self {
        match s {
            "fast" => CleanupModel::Fast,
            _ => CleanupModel::Quality,
        }
    }

    fn groq_model_id(self) -> &'static str {
        match self {
            CleanupModel::Fast => "llama-3.1-8b-instant",
            CleanupModel::Quality => "llama-3.3-70b-versatile",
        }
    }
}

/// Run the configured cleanup pass with `budget`. Returns the cleaned text on
/// success; on `Off`, timeout, or error returns the original text unchanged
/// (so callers can `let text = cleanup(...).await` without a fallback branch).
pub async fn cleanup(
    backend: CleanupBackend,
    api_key: &str,
    text: &str,
    budget: Duration,
    model: CleanupModel,
) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }

    match backend {
        CleanupBackend::Off => text.to_string(),
        CleanupBackend::Groq => {
            let fut = cleanup_with_groq(api_key, text, model);
            match tokio::time::timeout(budget, fut).await {
                Ok(Ok(cleaned)) => cleaned,
                Ok(Err(e)) => {
                    eprintln!("[typwrtr] LLM cleanup error: {}", e);
                    text.to_string()
                }
                Err(_) => {
                    eprintln!(
                        "[typwrtr] LLM cleanup exceeded {}ms budget; using uncleaned text",
                        budget.as_millis()
                    );
                    text.to_string()
                }
            }
        }
    }
}

async fn cleanup_with_groq(
    api_key: &str,
    text: &str,
    model: CleanupModel,
) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("Groq API key not set; LLM cleanup is unavailable.".to_string());
    }

    let body = serde_json::json!({
        "model": model.groq_model_id(),
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user",   "content": text},
        ],
        "temperature": 0,
        "max_tokens": 512,
    });

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.groq.com/openai/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Groq cleanup request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Groq cleanup HTTP {}: {}", status, body));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse Groq response: {}", e))?;

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("Missing choices[0].message.content in Groq response")?
        .trim()
        .to_string();

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn off_passes_through() {
        let out = cleanup(
            CleanupBackend::Off,
            "",
            "hello world.",
            Duration::from_millis(800),
            CleanupModel::Quality,
        )
        .await;
        assert_eq!(out, "hello world.");
    }

    #[tokio::test]
    async fn empty_input_short_circuits() {
        let out = cleanup(
            CleanupBackend::Groq,
            "key",
            "",
            Duration::from_millis(800),
            CleanupModel::Quality,
        )
        .await;
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn missing_api_key_falls_back() {
        let out = cleanup(
            CleanupBackend::Groq,
            "",
            "hello world",
            Duration::from_millis(800),
            CleanupModel::Fast,
        )
        .await;
        // No key -> error -> identity fallback (must not panic).
        assert_eq!(out, "hello world");
    }

    #[test]
    fn cleanup_model_from_str_defaults_to_quality() {
        assert_eq!(CleanupModel::from_str("fast"), CleanupModel::Fast);
        assert_eq!(CleanupModel::from_str("quality"), CleanupModel::Quality);
        assert_eq!(CleanupModel::from_str(""), CleanupModel::Quality);
        assert_eq!(CleanupModel::from_str("nonsense"), CleanupModel::Quality);
    }
}
