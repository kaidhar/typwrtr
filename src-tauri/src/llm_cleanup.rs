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

/// Run the configured cleanup pass with `budget`. Returns the cleaned text on
/// success; on `Off`, timeout, or error returns the original text unchanged
/// (so callers can `let text = cleanup(...).await` without a fallback branch).
pub async fn cleanup(
    backend: CleanupBackend,
    api_key: &str,
    text: &str,
    budget: Duration,
) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }

    match backend {
        CleanupBackend::Off => text.to_string(),
        CleanupBackend::Groq => {
            let fut = cleanup_with_groq(api_key, text);
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

async fn cleanup_with_groq(api_key: &str, text: &str) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("Groq API key not set; LLM cleanup is unavailable.".to_string());
    }

    let body = serde_json::json!({
        // 8B-instant is cheap and typically hits ~150–400 ms RTT on Groq —
        // well under our 800 ms budget. Bump to 70B-versatile if quality
        // turns out to matter more than latency.
        "model": "llama-3.1-8b-instant",
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
        )
        .await;
        assert_eq!(out, "hello world.");
    }

    #[tokio::test]
    async fn empty_input_short_circuits() {
        let out = cleanup(CleanupBackend::Groq, "key", "", Duration::from_millis(800)).await;
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn missing_api_key_falls_back() {
        let out = cleanup(
            CleanupBackend::Groq,
            "",
            "hello world",
            Duration::from_millis(800),
        )
        .await;
        // No key -> error -> identity fallback (must not panic).
        assert_eq!(out, "hello world");
    }
}
