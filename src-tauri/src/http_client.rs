// Fluence Windows — Shared HTTP Client
// Single reqwest Client pooled and shared across backend API calls.

use once_cell::sync::Lazy;

pub static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
});
