use reqwest::multipart;

pub async fn transcribe_groq(
    api_key: &str,
    wav_bytes: Vec<u8>,
    language: &str,
    initial_prompt: &str,
) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("Groq API key not set. Please enter your API key in settings.".to_string());
    }

    let file_part = multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;

    let mut form = multipart::Form::new()
        .text("model", "whisper-large-v3-turbo")
        .text("response_format", "json")
        .part("file", file_part);

    // Groq's transcription endpoint requires an explicit ISO-639-1 code; "auto"
    // is not supported, so we just omit the field in that case and let the
    // server infer.
    if !language.is_empty() && language != "auto" {
        form = form.text("language", language.to_string());
    }

    if !initial_prompt.is_empty() {
        form = form.text("prompt", initial_prompt.to_string());
    }

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Groq API request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Groq API error ({}): {}", status, body));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse Groq response: {}", e))?;

    json["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or("No 'text' field in Groq response".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_empty_api_key() {
        let result = transcribe_groq("", Vec::new(), "en", "").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("API key not set"));
    }
}
