use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let target_triple = env::var("TARGET").unwrap_or_default();
    let binary_path = expected_sidecar_path(&target_triple);

    println!("cargo:rerun-if-changed=build.rs");

    if is_valid_sidecar(&binary_path) {
        println!(
            "cargo:warning=Found whisper.cpp sidecar at {:?}",
            binary_path
        );
        stage_runtime_deps_to_target();
        tauri_build::build();
        return;
    }

    if binary_path.exists() {
        let _ = fs::remove_file(&binary_path);
    }

    let whisper_dir = Path::new("..").join("whisper.cpp");
    if !whisper_dir.exists() {
        println!(
            "cargo:warning=No ../whisper.cpp checkout found; skipping local sidecar build. Cloud transcription will still work."
        );
        ensure_placeholder_sidecar(&binary_path);
        stage_runtime_deps_to_target();
        tauri_build::build();
        return;
    }

    println!(
        "cargo:warning=Building whisper.cpp sidecar from {:?}",
        whisper_dir
    );

    if !run(Command::new("cmake").args(["-B", "build", "-S", "."]).current_dir(&whisper_dir)) {
        println!(
            "cargo:warning=Failed to configure whisper.cpp with cmake; local transcription will be unavailable."
        );
        ensure_placeholder_sidecar(&binary_path);
        stage_runtime_deps_to_target();
        tauri_build::build();
        return;
    }

    if !run(
        Command::new("cmake")
            .args(["--build", "build", "--config", "Release"])
            .current_dir(&whisper_dir),
    ) {
        println!(
            "cargo:warning=Failed to build whisper.cpp; local transcription will be unavailable."
        );
        ensure_placeholder_sidecar(&binary_path);
        stage_runtime_deps_to_target();
        tauri_build::build();
        return;
    }

    if let Err(err) = copy_sidecar(&whisper_dir, &binary_path) {
        println!(
            "cargo:warning=Failed to stage whisper.cpp sidecar: {}. Local transcription will be unavailable.",
            err
        );
        ensure_placeholder_sidecar(&binary_path);
        stage_runtime_deps_to_target();
        tauri_build::build();
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755));
    }

    println!(
        "cargo:warning=whisper.cpp sidecar staged at {:?}",
        binary_path
    );
    stage_runtime_deps_to_target();
    tauri_build::build();
}

fn expected_sidecar_path(target_triple: &str) -> PathBuf {
    let mut path = PathBuf::from("binaries").join(format!("whisper-cpp-{}", target_triple));
    if target_triple.contains("windows") {
        path.set_extension("exe");
    }
    path
}

fn run(command: &mut Command) -> bool {
    match command.status() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

fn copy_sidecar(whisper_dir: &Path, dest: &Path) -> Result<(), String> {
    let candidates = if cfg!(target_os = "windows") {
        ["whisper-cli.exe", "main.exe"]
    } else {
        ["whisper-cli", "main"]
    };

    let built_binary = candidates
        .iter()
        .map(|name| whisper_dir.join("build").join("bin").join(name))
        .find(|path| path.exists())
        .ok_or_else(|| "No built whisper.cpp CLI binary found in build/bin".to_string())?;

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    fs::copy(&built_binary, dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn ensure_placeholder_sidecar(dest: &Path) {
    if dest.exists() {
        return;
    }

    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = fs::write(
        dest,
        b"This is a placeholder sidecar for cloud-only development builds.\n",
    );

    println!(
        "cargo:warning=Created placeholder sidecar at {:?}. Replace it with a real whisper.cpp binary to enable local transcription.",
        dest
    );
}

fn is_valid_sidecar(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.len() > 1024)
        .unwrap_or(false)
}

fn stage_runtime_deps_to_target() {
    let manifest_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(value) => PathBuf::from(value),
        Err(_) => return,
    };
    let profile = match env::var("PROFILE") {
        Ok(value) => value,
        Err(_) => return,
    };

    let binaries_dir = manifest_dir.join("binaries");
    let target_dir = manifest_dir.join("target").join(profile);

    if !binaries_dir.exists() || !target_dir.exists() {
        return;
    }

    let runtime_files = [
        "ggml-base.dll",
        "ggml-cpu.dll",
        "ggml-cuda.dll",
        "ggml.dll",
        "whisper.dll",
    ];

    for file_name in runtime_files {
        let source = binaries_dir.join(file_name);
        let dest = target_dir.join(file_name);
        if source.exists() {
            let _ = fs::copy(source, dest);
        }
    }
}
