// Links the official prebuilt Moonshine v2 C++ core + ONNX Runtime.
// The vendor tree (headers + .lib + onnxruntime.dll) lives in
// <repo>/.vendor/moonshine (gitignored, provisioned per-machine from the
// official moonshine-ai/moonshine release). MOONSHINE_V2_LIB_DIR overrides
// the search path when set.

#[cfg(target_os = "windows")]
fn vendor_subdir() -> &'static str {
    "moonshine-voice-windows-x86_64"
}

#[cfg(not(target_os = "windows"))]
fn vendor_subdir() -> &'static str {
    "moonshine-voice-linux-x86_64"
}

fn main() {
    let vendor_lib = std::env::var("MOONSHINE_V2_LIB_DIR").unwrap_or_else(|_| {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        format!(
            "{manifest_dir}/../../.vendor/moonshine/{}/lib",
            vendor_subdir()
        )
    });
    println!("cargo:rustc-link-search=native={vendor_lib}");
    link_vendor_libs(&vendor_lib);
    println!("cargo:rerun-if-env-changed=MOONSHINE_V2_LIB_DIR");

    #[cfg(not(target_os = "windows"))]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");

    stage_runtime_libs(&vendor_lib);
}

#[cfg(target_os = "windows")]
fn link_vendor_libs(_vendor_lib: &str) {
    // Dependents first for MSVC static linking.
    println!("cargo:rustc-link-lib=static=moonshine");
    println!("cargo:rustc-link-lib=static=moonshine-utils");
    println!("cargo:rustc-link-lib=static=bin-tokenizer");
    println!("cargo:rustc-link-lib=static=ort-utils");
    println!("cargo:rustc-link-lib=onnxruntime");
}

#[cfg(not(target_os = "windows"))]
fn link_vendor_libs(vendor_lib: &str) {
    let dir = std::path::Path::new(vendor_lib);
    let moonshine = dir.join("libmoonshine.so");
    let ort = dir.join("libonnxruntime.so.1");
    for required in [&moonshine, &ort] {
        if !required.is_file() {
            panic!(
                "moonshine-v2-server: missing vendor file {}. Provision the Linux vendor bundle first: \
                 see src-tauri/moonshine-v2-server/VENDOR.linux.json and CONTRIBUTING.md (Linux section). \
                 Searched lib dir: {vendor_lib}",
                required.display()
            );
        }
    }
    println!("cargo:rustc-link-lib=moonshine");
    println!("cargo:rustc-link-arg=-l:libonnxruntime.so.1");
}

fn stage_one_file(vendor_lib: &str, file: &str, warn_label: &str) {
    let src = std::path::PathBuf::from(vendor_lib).join(file);
    let out_dir = match std::env::var("OUT_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => return,
    };
    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
    let profile_dir = match out_dir.ancestors().nth(3) {
        Some(d) => d,
        None => return,
    };
    let dest = profile_dir.join(file);
    let copy_needed = match (std::fs::metadata(&src), std::fs::metadata(&dest)) {
        (Ok(s), Ok(d)) => s.len() != d.len(),
        (Ok(_), Err(_)) => true,
        _ => false,
    };
    if copy_needed {
        if let Err(e) = std::fs::copy(&src, &dest) {
            println!("cargo:warning={warn_label} staging skipped ({e})");
        }
    }
}

#[cfg(target_os = "windows")]
fn stage_runtime_libs(vendor_lib: &str) {
    stage_one_file(vendor_lib, "onnxruntime.dll", "onnxruntime.dll");
}

#[cfg(not(target_os = "windows"))]
fn stage_runtime_libs(vendor_lib: &str) {
    stage_one_file(vendor_lib, "libonnxruntime.so.1", "libonnxruntime.so.1");
    stage_one_file(vendor_lib, "libmoonshine.so", "libmoonshine.so");
}
