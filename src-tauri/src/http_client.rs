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

/// Validate that a base URL is suitable for API calls.
/// - Must be a well-formed URL
/// - Must use HTTPS in production
/// - HTTP is allowed only for localhost development servers
pub fn validate_api_url(base_url: &str) -> Result<(), String> {
    let url = url::Url::parse(base_url.trim())
        .map_err(|e| format!("Invalid URL '{}': {}", base_url, e))?;

    match url.scheme() {
        "https" => {}
        "http" => {
            let host = url.host_str().unwrap_or("");
            if host != "localhost" && host != "127.0.0.1" && host != "::1" && host != "[::1]" {
                return Err(format!(
                    "HTTP is only allowed for localhost development (got {}). Use HTTPS for remote servers.",
                    host
                ));
            }
        }
        other => {
            return Err(format!(
                "Unsupported URL scheme '{}'. Use https:// (or http://localhost for development).",
                other
            ));
        }
    }

    if url.username() != "" || url.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".into());
    }

    if url.host_str().is_none() || url.host_str() == Some("") {
        return Err("URL must have a valid hostname".into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_https_url() {
        assert!(validate_api_url("https://api.groq.com/openai").is_ok());
    }

    #[test]
    fn valid_https_trailing_slash() {
        assert!(validate_api_url("https://api.openai.com/v1/").is_ok());
    }

    #[test]
    fn valid_localhost_http() {
        assert!(validate_api_url("http://localhost:1430").is_ok());
        assert!(validate_api_url("http://127.0.0.1:1430").is_ok());
        assert!(validate_api_url("http://[::1]:1430").is_ok());
    }

    #[test]
    fn reject_http_non_localhost() {
        assert!(validate_api_url("http://api.groq.com/openai").is_err());
        assert!(validate_api_url("http://10.0.0.1:8080").is_err());
        assert!(validate_api_url("http://192.168.1.1").is_err());
    }

    #[test]
    fn reject_ftp_scheme() {
        assert!(validate_api_url("ftp://example.com/file").is_err());
        assert!(validate_api_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn reject_credentials_in_url() {
        assert!(validate_api_url("https://user:pass@api.example.com").is_err());
        assert!(validate_api_url("https://user@api.example.com").is_err());
    }

    #[test]
    fn reject_empty_url() {
        assert!(validate_api_url("").is_err());
    }

    #[test]
    fn reject_no_host() {
        assert!(validate_api_url("https://").is_err());
    }

    #[test]
    fn reject_garbage() {
        assert!(validate_api_url("not-a-url").is_err());
    }

    #[test]
    fn build_api_url_appends_v1() {
        let url = build_api_url("https://api.groq.com/openai", "audio/transcriptions");
        assert_eq!(url, "https://api.groq.com/openai/v1/audio/transcriptions");
    }

    #[test]
    fn build_api_url_no_double_v1() {
        let url = build_api_url("https://api.groq.com/openai/v1", "audio/transcriptions");
        assert_eq!(url, "https://api.groq.com/openai/v1/audio/transcriptions");
    }

    #[test]
    fn build_api_url_strips_trailing_slash() {
        let url = build_api_url("https://api.groq.com/openai/v1/", "audio/transcriptions");
        assert_eq!(url, "https://api.groq.com/openai/v1/audio/transcriptions");
    }
}
