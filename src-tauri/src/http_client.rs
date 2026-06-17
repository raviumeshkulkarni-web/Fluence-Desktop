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
