use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();

    // Moonshine v2 sidecar integrity (experiment branch, opt-in engines only).
    //
    // If a staged sidecar exe exists, hash it and bake the digest in as
    // MOONSHINE_V2_SERVER_SHA256 so ensure_server_running can enforce the
    // verify_sha256 gate at spawn time. Lookup order mirrors the runtime
    // resolver (profile output dir first, then the Tauri binaries/ staging
    // dir), so every build — local debug or CI release — pins the exact
    // artifact it will actually execute. A single committed hash could never
    // do this: debug/dev and release/CI artifacts legitimately differ.
    //
    // Absence is NOT an error here: fresh clones and machines without the
    // sidecar built must still compile and run every other flow. The runtime
    // fails closed (refuses to spawn v2 engines) when no hash is available.
    if let Some(exe) = find_staged_sidecar() {
        match sha256_file(&exe) {
            Ok(digest) => {
                println!("cargo:rustc-env=MOONSHINE_V2_SERVER_SHA256={digest}");
                // Rebuild the app when the sidecar relinks so the pin never
                // goes stale within a working tree.
                println!("cargo:rerun-if-changed={}", exe.display());
            }
            Err(e) => {
                println!("cargo:warning=moonshine-v2-server hash skipped ({e})");
            }
        }
    }
}

/// Ordered candidates for a staged sidecar exe at app-compile time.
fn find_staged_sidecar() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    // Workspace target dir honors CARGO_TARGET_DIR like cargo itself does.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("target"));

    let mut candidates = vec![target_dir.join(&profile).join(exe_name())];

    // Tauri externalBin staging: binaries/<name>-<triple>.exe
    if let Ok(target_triple) = std::env::var("TARGET") {
        candidates
            .push(manifest_dir.join(format!("binaries/moonshine-v2-server-{target_triple}.exe")));
    }

    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(windows)]
fn exe_name() -> &'static str {
    "moonshine-v2-server.exe"
}

#[cfg(not(windows))]
fn exe_name() -> &'static str {
    "moonshine-v2-server"
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&data);
    Ok(hex::encode(hasher.finalize()))
}
