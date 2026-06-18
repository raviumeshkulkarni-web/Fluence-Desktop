// Fluence Windows — Shared HTTP Client
// Single reqwest Client pooled and shared across backend API calls.

use once_cell::sync::Lazy;

pub static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        // Fail fast (8s) if the server is unreachable — distinct from the 30s
        // response timeout which covers upload + inference time for large recordings.
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
});

/// Builds a full API endpoint URL from a base URL and path suffix.
/// Handles trailing slashes and whether or not the base already includes `/v1`.
///
/// # Examples
/// ```
/// // "https://api.groq.com/openai"      → "https://api.groq.com/openai/v1/audio/transcriptions"
/// // "https://api.groq.com/openai/v1"   → "https://api.groq.com/openai/v1/audio/transcriptions"
/// // "https://api.groq.com/openai/v1/"  → "https://api.groq.com/openai/v1/audio/transcriptions"
/// ```
pub fn build_api_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.to_lowercase().ends_with("/v1") {
        format!("{}/{}", base, path.trim_start_matches('/'))
    } else {
        format!("{}/v1/{}", base, path.trim_start_matches('/'))
    }
}
