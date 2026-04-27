fn main() {
    // whisper.cpp is now linked directly via the `whisper-rs` crate; we no longer
    // stage a sidecar binary or runtime DLLs. The crate's own build script handles
    // compiling whisper.cpp + ggml.
    println!("cargo:rerun-if-changed=build.rs");
    tauri_build::build();
}
