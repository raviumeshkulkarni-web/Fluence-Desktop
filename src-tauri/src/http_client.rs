// Fluence Windows - Shared HTTP Client
// Single reqwest Client pooled and shared across backend API calls.

use once_cell::sync::Lazy;

/// Maximum redirect hops the shared clients follow before handing the 3xx
/// back to the caller. `Policy::custom` does NOT inherit reqwest's default
/// loop guard (`Policy::limited(10)` also errors on a loop), so the cap has to
/// live here or a redirect loop would spin forever.
pub const MAX_REDIRECT_HOPS: usize = 10;

/// Whether one redirect hop may be followed by a shared client (audit finding
/// S3). Pure so the rule is unit-testable without a network.
///
/// reqwest only strips `Authorization`/`Cookie` when a redirect crosses HOSTS
/// (`redirect::remove_sensitive_headers` in reqwest 0.12.28). A SAME-HOST
/// `https` -> `http` redirect therefore forwards the bearer token to an
/// insecure endpoint, in the clear, to whatever hostname the server named.
/// That is the leak this predicate closes.
///
/// Rule: a hop is allowed only when it never downgrades the chain off TLS.
/// `previous` is the chain so far with the initial request URL first, so an
/// `https` anywhere in it means every later hop must be `https` too. A chain
/// that never left `http` (the localhost development servers
/// [`validate_api_url`] permits) keeps following `http` hops. Any non-web
/// scheme is refused.
pub fn redirect_is_allowed(previous: &[url::Url], next: &url::Url) -> bool {
    // Loop guard: `previous.len()` is (hops already followed) + 1, because the
    // initial request URL is part of the chain.
    if previous.len().saturating_sub(1) >= MAX_REDIRECT_HOPS {
        return false;
    }
    if next.scheme() == "https" {
        return true;
    }
    if next.scheme() != "http" {
        return false;
    }
    !previous.iter().any(|url| url.scheme() == "https")
}

/// The explicit redirect policy for the shared clients.
///
/// Chosen over `Policy::default()` (`limited(10)`, and reqwest 0.12.28 leaves
/// `https_only = false`), which follows a same-host `https` -> `http`
/// redirect while still carrying `Authorization`.
///
/// A refused hop uses `stop()`, NOT `error()`: the 3xx is handed back as the
/// response, so every caller's existing status classification sees it (the
/// Drive layer maps 3xx to a permanent `Rejected`) instead of an opaque
/// transport error the Drive layer would classify `Retryable` and back off on
/// forever.
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if redirect_is_allowed(attempt.previous(), attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

pub static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        // Fail fast (8s) if the server is unreachable - distinct from the 30s
        // response timeout which covers upload + inference time for large recordings.
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(30))
        // S3: never hand the bearer token to a plaintext endpoint on a
        // redirect. Same policy as the Drive blocking client.
        .redirect(redirect_policy())
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

    // --- Redirect policy (audit finding S3) --------------------------------

    fn url_of(raw: &str) -> url::Url {
        url::Url::parse(raw).unwrap()
    }

    #[test]
    fn redirect_policy_allows_tls_hops_and_local_http() {
        // Local http development servers (allowed by `validate_api_url`) keep
        // following their http hops.
        assert!(redirect_is_allowed(&[], &url_of("http://localhost:1430/")));
        assert!(redirect_is_allowed(
            &[url_of("http://localhost:1430/")],
            &url_of("http://localhost:1430/v1/audio"),
        ));
        // https -> https is never a downgrade.
        assert!(redirect_is_allowed(
            &[url_of("https://api.groq.com/openai/v1")],
            &url_of("https://api.groq.com/openai/v1/models"),
        ));
        // http -> https is an upgrade, not a downgrade.
        assert!(redirect_is_allowed(
            &[url_of("http://localhost:1430/")],
            &url_of("https://localhost:1430/secure"),
        ));
    }

    #[test]
    fn redirect_policy_refuses_a_tls_downgrade() {
        // The leak: same host, https -> http, `Authorization` still attached
        // because reqwest only strips it when the HOST changes.
        assert!(!redirect_is_allowed(
            &[url_of("https://www.googleapis.com/drive/v3/files")],
            &url_of("http://www.googleapis.com/drive/v3/files")
        ));
        assert!(!redirect_is_allowed(
            &[
                url_of("https://api.groq.com/a"),
                url_of("https://api.groq.com/b")
            ],
            &url_of("http://api.groq.com/c"),
        ));
        // A cross-host hop off an https chain is refused as well.
        assert!(!redirect_is_allowed(
            &[url_of("https://api.groq.com/a")],
            &url_of("http://evil.example/a"),
        ));
    }

    #[test]
    fn redirect_policy_refuses_non_web_schemes() {
        assert!(!redirect_is_allowed(
            &[url_of("https://a.test/")],
            &url_of("ftp://a.test/f")
        ));
        assert!(!redirect_is_allowed(
            &[url_of("http://a.test/")],
            &url_of("file:///etc/passwd")
        ));
    }

    #[test]
    fn redirect_policy_caps_the_hop_chain() {
        // `Policy::custom` does not inherit reqwest's loop guard, so the cap
        // is ours: `previous` includes the initial URL, so length == hops + 1.
        let chain: Vec<url::Url> = (0..=MAX_REDIRECT_HOPS)
            .map(|i| url_of(&format!("https://a.test/{i}")))
            .collect();
        assert!(redirect_is_allowed(
            &chain[..MAX_REDIRECT_HOPS],
            &url_of("https://a.test/next")
        ));
        assert!(
            !redirect_is_allowed(&chain, &url_of("https://a.test/next")),
            "one hop past the cap stops the chain"
        );
    }

    #[tokio::test]
    async fn shared_client_follows_a_plaintext_to_plaintext_hop() {
        // Behavior preservation for S3: an http -> http hop (the localhost
        // development case) is still followed, so installing a custom policy
        // did not silently switch the client to `Policy::none()`.
        let target = one_shot("200 OK", "", "followed").await;
        let origin = one_shot(
            "302 Found",
            &format!("Location: http://{target}/next\r\n"),
            "",
        )
        .await;

        let client = reqwest::Client::builder()
            .redirect(redirect_policy())
            .build()
            .unwrap();
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client.get(format!("http://{origin}/start")).send(),
        )
        .await
        .expect("the hop must not hang")
        .expect("the redirect is followed");

        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.text().await.unwrap(), "followed");
    }

    /// Answers exactly one request with `status`, `extra_headers` and `body`,
    /// then closes. Returns the bound loopback address.
    async fn one_shot(status: &str, extra_headers: &str, body: &str) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Owned so the spawned responder is `'static`.
        let status = status.to_string();
        let extra_headers = extra_headers.to_string();
        let body = body.to_string();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
                    len = body.len(),
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        addr
    }
}
