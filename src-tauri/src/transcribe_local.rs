use std::path::PathBuf;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

pub async fn transcribe_local(
    app: &AppHandle,
    model_path: &PathBuf,
    audio_path: &PathBuf,
) -> Result<String, String> {
    if !model_path.exists() {
        return Err("Whisper model not found. Please download a model first.".to_string());
    }

    println!("[typwrtr] Running whisper.cpp sidecar with model {:?}", model_path);
    let thread_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .to_string();

    let output = app
        .shell()
        .sidecar("whisper-cpp")
        .map_err(|e| format_sidecar_error(e.to_string()))?
        .args([
            "-t",
            thread_count.as_str(),
            "-m",
            model_path.to_str().unwrap(),
            "-f",
            audio_path.to_str().unwrap(),
            "--no-timestamps",
            "--no-prints",
            "-l",
            "en",
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run whisper.cpp: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let exit_code = output.status.code();

    // Some Windows sidecar runs report a non-zero status even when the transcript
    // is already present on stdout, so prefer the actual payload when available.
    if stdout.is_empty() && exit_code != Some(0) {
        return Err(format!(
            "whisper.cpp failed (exit code: {:?}): {}",
            exit_code, stderr
        ));
    }

    if stdout.is_empty() {
        return Err(format!(
            "whisper.cpp returned no transcript (exit code: {:?}). stderr: {}",
            exit_code, stderr
        ));
    }

    let text = stdout;
    println!("[typwrtr] Whisper output: {}", text);
    Ok(text)
}

fn format_sidecar_error(error: String) -> String {
    format!(
        "Local Whisper is not available because the whisper.cpp sidecar is missing or not bundled correctly. Expected a Tauri sidecar named 'whisper-cpp'. Build or place the sidecar under src-tauri/binaries, or switch to Groq Cloud. Original error: {}",
        error
    )
}

pub fn model_filename(model_size: &str) -> String {
    format!("ggml-{}.bin", model_size)
}

pub fn model_download_url(model_size: &str) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin",
        model_size
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_filename() {
        assert_eq!(model_filename("small"), "ggml-small.bin");
        assert_eq!(model_filename("medium"), "ggml-medium.bin");
    }

    #[test]
    fn test_model_download_url() {
        assert_eq!(
            model_download_url("small"),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin"
        );
    }

    #[test]
    fn test_sidecar_error_message_mentions_sidecar() {
        let err = format_sidecar_error("boom".to_string());
        assert!(err.contains("whisper.cpp sidecar"));
        assert!(err.contains("src-tauri/binaries"));
    }
}
