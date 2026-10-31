// Links the official prebuilt Moonshine v2 C++ core + ONNX Runtime.
// The vendor tree (headers + .lib + onnxruntime.dll) lives in
// <repo>/.vendor/moonshine (gitignored, provisioned per-machine from the
// official moonshine-ai/moonshine release). MOONSHINE_V2_LIB_DIR overrides
// the search path when set.

fn main() {
    let vendor_lib = std::env::var("MOONSHINE_V2_LIB_DIR").unwrap_or_else(|_| {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        format!("{manifest_dir}/../../.vendor/moonshine/moonshine-voice-windows-x86_64/lib")
    });
    println!("cargo:rustc-link-search=native={vendor_lib}");
    // Dependents first for MSVC static linking.
    println!("cargo:rustc-link-lib=static=moonshine");
    println!("cargo:rustc-link-lib=static=moonshine-utils");
    println!("cargo:rustc-link-lib=static=bin-tokenizer");
    println!("cargo:rustc-link-lib=static=ort-utils");
    println!("cargo:rustc-link-lib=onnxruntime");
    println!("cargo:rerun-if-env-changed=MOONSHINE_V2_LIB_DIR");

    // Stage onnxruntime.dll next to the built sidecar exe so the loader
    // finds it in every layout (cargo target dir, Tauri binaries/ staging,
    // installed bundle). Idempotent: skipped when already identical.
    stage_ort_dll(&vendor_lib);
}

/// Copies the vendor onnxruntime.dll beside the final binary output dir.
/// The exe and dll must stay siblings: Windows implicit DLL search starts
/// at the loading executable's own directory.
fn stage_ort_dll(vendor_lib: &str) {
    let src = std::path::PathBuf::from(vendor_lib).join(dll_name());
    let out_dir = match std::env::var("OUT_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => return,
    };
    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
    let profile_dir = match out_dir.ancestors().nth(3) {
        Some(d) => d,
        None => return,
    };
    let dest = profile_dir.join(dll_name());
    let copy_needed = match (std::fs::metadata(&src), std::fs::metadata(&dest)) {
        (Ok(s), Ok(d)) => s.len() != d.len(),
        (Ok(_), Err(_)) => true,
        _ => false,
    };
    if copy_needed {
        if let Err(e) = std::fs::copy(&src, &dest) {
            println!("cargo:warning=onnxruntime.dll staging skipped ({e})");
        }
    }
}

#[cfg(windows)]
fn dll_name() -> &'static str {
    "onnxruntime.dll"
}

#[cfg(not(windows))]
fn dll_name() -> &'static str {
    "libonnxruntime.so"
}
