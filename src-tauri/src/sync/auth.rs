// Fluence sync — Windows OAuth 2.0 authorization-code flow (spec §24).
//
// Loopback PKCE (S256): the browser is pointed at the provider's
// authorization endpoint with a code challenge; the provider redirects to
// `http://localhost:<port>/`, where an in-process listener captures the code.
// The refresh token is persisted by the caller (Credential Manager —
// `credentials::SYNC_REFRESH_TOKEN_TARGET`); the access token is memory-only.
//
// Everything except the HTTP exchange and the loopback listener is a pure
// function so the PKCE and redirect logic is fully unit-testable offline.

use base64::Engine;
use sha2::{Digest, Sha256};
use url::Url;

/// How long `listen_for_redirect` waits for the browser redirect before
/// giving up. Long enough for a slow account chooser; short enough that an
/// abandoned browser tab cannot wedge the sign-in button until app restart.
pub const SIGN_IN_TIMEOUT_SECS: u64 = 300;

/// OAuth client configuration. `OAuthConfig::google(client_id)` supplies the
/// Google defaults; real client credentials come from the app's OAuth setup
/// (spec §29 Experiment 1), never hardcoded in the repo.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    /// Public clients (installed apps) usually omit the secret.
    pub client_secret: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub redirect_uri: String,
    pub scope: String,
}

impl OAuthConfig {
    pub fn google(client_id: String, redirect_port: u16) -> Self {
        Self {
            client_id,
            client_secret: None,
            authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_endpoint: "https://oauth2.googleapis.com/token".to_string(),
            redirect_uri: format!("http://localhost:{redirect_port}/"),
            scope: "https://www.googleapis.com/auth/drive.appdata".to_string(),
        }
    }
}

/// Errors surfaced by the auth layer. `Http` carries the provider's error
/// body when one exists.
#[derive(Debug)]
pub enum AuthError {
    Network(String),
    Http {
        status: u16,
        body: String,
    },
    BadRedirect,
    BadResponse,
    InvalidState,
    AccessDenied(String),
    NoRefreshToken,
    /// The loopback redirect never arrived within `SIGN_IN_TIMEOUT_SECS`.
    Timeout,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Network(m) => write!(f, "network error: {m}"),
            AuthError::Http { status, body } => write!(f, "token endpoint HTTP {status}: {body}"),
            AuthError::BadRedirect => write!(f, "malformed OAuth redirect"),
            AuthError::BadResponse => write!(f, "malformed token response"),
            AuthError::InvalidState => write!(f, "OAuth state mismatch"),
            AuthError::AccessDenied(m) => write!(f, "authorization denied: {m}"),
            AuthError::NoRefreshToken => write!(f, "no refresh token — sign in again"),
            AuthError::Timeout => write!(f, "sign-in timed out — please try again"),
        }
    }
}

impl std::error::Error for AuthError {}

/// PKCE code verifier: 43 characters from the unreserved alphabet, generated
/// from two random UUIDs (no extra rand dependency). RFC 7636: 43–128 chars.
pub fn pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let mut verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    verifier.truncate(43);
    verifier
}

/// RFC 7636 §4.2 S256 challenge: `BASE64URL-ENCODE(SHA256(ASCII(verifier)))`.
/// Tested against the RFC's own vector.
pub fn pkce_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// The authorization URL to open in the system browser. `prompt=select_account`
/// forces the account chooser so users with multiple Google accounts pick the
/// one they want instead of silently reusing the signed-in session.
pub fn authorization_url(config: &OAuthConfig, state: &str, challenge: &str) -> String {
    let mut url = Url::parse(&config.authorization_endpoint).expect("valid endpoint");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("prompt", "select_account")
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    url.to_string()
}

/// The outcome of a successful token exchange.
#[derive(Debug, Clone)]
pub struct TokenResponse {
    pub access_token: String,
    /// `None` when the endpoint does not rotate the refresh token.
    pub refresh_token: Option<String>,
    pub expires_in_secs: u64,
}

/// Form body for the authorization-code exchange (RFC 6749 §4.1.3).
pub fn token_request_body(
    config: &OAuthConfig,
    code: &str,
    verifier: &str,
) -> Vec<(String, String)> {
    let mut body = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), config.redirect_uri.clone()),
        ("client_id".to_string(), config.client_id.clone()),
        ("code_verifier".to_string(), verifier.to_string()),
    ];
    if let Some(secret) = &config.client_secret {
        body.push(("client_secret".to_string(), secret.clone()));
    }
    body
}

/// Form body for the refresh grant (RFC 6749 §6).
pub fn refresh_request_body(config: &OAuthConfig, refresh_token: &str) -> Vec<(String, String)> {
    let mut body = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token.to_string()),
        ("client_id".to_string(), config.client_id.clone()),
    ];
    if let Some(secret) = &config.client_secret {
        body.push(("client_secret".to_string(), secret.clone()));
    }
    body
}

/// Parse the provider's token JSON. Google returns `access_token`,
/// optional `refresh_token`, and `expires_in`.
pub fn parse_token_response(json: &str) -> Result<TokenResponse, AuthError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| AuthError::BadResponse)?;
    let access = value
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or(AuthError::BadResponse)?;
    let refresh = value.get("refresh_token").and_then(|v| v.as_str());
    let expires = value
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    Ok(TokenResponse {
        access_token: access.to_string(),
        refresh_token: refresh.map(str::to_string),
        expires_in_secs: expires,
    })
}

/// In-process loopback listener (spec §24): bind the redirect URI's port,
/// accept one HTTP request, validate state, extract the code, answer with a
/// minimal HTML page, and return the code. Never binds an external address.
pub async fn listen_for_redirect(
    redirect_uri: &str,
    expected_state: &str,
) -> Result<String, AuthError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let url = Url::parse(redirect_uri).map_err(|_| AuthError::BadRedirect)?;
    let port = url.port().ok_or(AuthError::BadRedirect)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| AuthError::Network(e.to_string()))?;

    // The whole accept+handshake is bounded: on timeout the listener and the
    // accepted stream drop here, releasing the loopback socket exactly as on
    // the success path.
    let handshake = async {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;

        let mut buf = [0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        let head = std::str::from_utf8(&buf[..n]).map_err(|_| AuthError::BadRedirect)?;
        let request_line = head.lines().next().ok_or(AuthError::BadRedirect)?;
        let code = parse_redirect_request(request_line, expected_state)?;

        let body = "Fluence sync authorized — you can close this window.";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        Ok(code)
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(SIGN_IN_TIMEOUT_SECS),
        handshake,
    )
    .await
    .map_err(|_| AuthError::Timeout)?
}

/// Parse `GET /?code=..&state=.. HTTP/1.1` and validate the state.
pub fn parse_redirect_request(
    request_line: &str,
    expected_state: &str,
) -> Result<String, AuthError> {
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        return Err(AuthError::BadRedirect);
    }
    let target = parts.next().ok_or(AuthError::BadRedirect)?;
    let (path, query) = target.split_once('?').ok_or(AuthError::BadRedirect)?;
    if path != "/" && !path.is_empty() {
        return Err(AuthError::BadRedirect);
    }
    let params: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
    if let Some(error) = params.get("error") {
        return Err(AuthError::AccessDenied(error.clone()));
    }
    let state = params.get("state").ok_or(AuthError::BadRedirect)?;
    if state != expected_state {
        return Err(AuthError::InvalidState);
    }
    params.get("code").cloned().ok_or(AuthError::BadRedirect)
}

/// Exchange the authorization code for tokens. `client` is the shared
/// reqwest client (`crate::http_client::CLIENT`).
pub async fn exchange_code(
    config: &OAuthConfig,
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, AuthError> {
    let body = token_request_body(config, code, verifier);
    post_token_request(config, client, &body).await
}

/// Refresh the access token with a stored refresh token.
pub async fn refresh_access_token(
    config: &OAuthConfig,
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<TokenResponse, AuthError> {
    let body = refresh_request_body(config, refresh_token);
    post_token_request(config, client, &body).await
}

async fn post_token_request(
    config: &OAuthConfig,
    client: &reqwest::Client,
    body: &[(String, String)],
) -> Result<TokenResponse, AuthError> {
    let response = client
        .post(&config.token_endpoint)
        .form(body)
        .send()
        .await
        .map_err(|e| AuthError::Network(e.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| AuthError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(AuthError::Http {
            status: status.as_u16(),
            body: text,
        });
    }
    parse_token_response(&text)
}

// ---------------------------------------------------------------------------
// Auth session (spec §24): access token memory-only, refresh token in the
// OS credential store via `credentials`.
// ---------------------------------------------------------------------------

use std::time::Instant;

/// In-memory OAuth session. The access token lives here and nowhere else;
/// the refresh token is mirrored to the OS credential store through
/// `persist_refresh_token`/`forget_refresh_token`.
#[derive(Debug)]
pub struct AuthSession {
    pub config: OAuthConfig,
    access_token: Option<String>,
    expires_at: Option<Instant>,
    pub refresh_token: Option<String>,
}

impl AuthSession {
    pub fn new(config: OAuthConfig) -> Self {
        Self {
            config,
            access_token: None,
            expires_at: None,
            refresh_token: None,
        }
    }

    /// Mirror the refresh token to the OS credential store (§24).
    pub fn persist_refresh_token(&self) -> Result<(), String> {
        let Some(token) = &self.refresh_token else {
            return Ok(());
        };
        crate::credentials::store_sync_refresh_token(token).map_err(|e| e.to_string())
    }

    /// Load the refresh token from the OS credential store.
    pub fn load_refresh_token(&mut self) -> Result<(), String> {
        self.refresh_token =
            Some(crate::credentials::read_sync_refresh_token().map_err(|e| e.to_string())?);
        Ok(())
    }

    /// Forget the refresh token everywhere (sign-out).
    pub fn forget_refresh_token(&mut self) -> Result<(), String> {
        self.refresh_token = None;
        crate::credentials::delete_sync_refresh_token().map_err(|e| e.to_string())
    }

    /// Adopt exchange results; a rotated refresh token replaces the old one.
    pub fn store_tokens(&mut self, response: &TokenResponse) {
        self.access_token = Some(response.access_token.clone());
        self.expires_at = Some(
            Instant::now()
                + std::time::Duration::from_secs(response.expires_in_secs.saturating_sub(60)),
        );
        if let Some(refresh) = &response.refresh_token {
            self.refresh_token = Some(refresh.clone());
        }
    }

    pub fn has_valid_access_token(&self) -> bool {
        match (&self.access_token, &self.expires_at) {
            (Some(_), Some(expires)) => Instant::now() < *expires,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// The current access token; `None` when expired or absent.
    pub fn access_token(&self) -> Option<&str> {
        if self.has_valid_access_token() {
            self.access_token.as_deref()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> OAuthConfig {
        OAuthConfig::google("test-client".to_string(), 17251)
    }

    #[test]
    fn pkce_s256_matches_rfc_7636_vector() {
        // RFC 7636 §A.4: the challenge for this verifier is `E9Mel...`.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_s256(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_verifier_is_43_unreserved_chars() {
        let v = pkce_verifier();
        assert_eq!(v.len(), 43);
        assert!(
            v.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "verifier must be unreserved: {v}"
        );
        assert_ne!(pkce_verifier(), v, "verifiers are random");
    }

    #[test]
    fn authorization_url_includes_pkce_params() {
        let url = authorization_url(&test_config(), "state-1", "challenge-1");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=test-client"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A17251%2F"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.appdata"));
        assert!(url.contains("state=state-1"));
        assert!(url.contains("code_challenge=challenge-1"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("prompt=select_account"));
    }

    #[test]
    fn parse_redirect_request_valid() {
        let line = "GET /?code=auth-code-1&state=state-1 HTTP/1.1";
        assert_eq!(
            parse_redirect_request(line, "state-1").unwrap(),
            "auth-code-1"
        );
    }

    #[test]
    fn parse_redirect_request_state_mismatch_rejected() {
        let line = "GET /?code=auth-code-1&state=state-2 HTTP/1.1";
        assert!(matches!(
            parse_redirect_request(line, "state-1"),
            Err(AuthError::InvalidState)
        ));
    }

    #[test]
    fn parse_redirect_request_error_rejected() {
        let line = "GET /?error=access_denied&state=state-1 HTTP/1.1";
        assert!(matches!(
            parse_redirect_request(line, "state-1"),
            Err(AuthError::AccessDenied(_))
        ));
    }

    #[test]
    fn parse_redirect_request_malformed_rejected() {
        assert!(matches!(
            parse_redirect_request("POST / HTTP/1.1", "s"),
            Err(AuthError::BadRedirect)
        ));
        assert!(matches!(
            parse_redirect_request("GET /no-query HTTP/1.1", "s"),
            Err(AuthError::BadRedirect)
        ));
        assert!(matches!(
            parse_redirect_request("GET /?state=s HTTP/1.1", "s"),
            Err(AuthError::BadRedirect)
        )); // code missing
    }

    #[test]
    fn token_request_body_has_verifier_and_no_secret_for_public_client() {
        let body = token_request_body(&test_config(), "code-1", "verifier-1");
        assert!(body.contains(&("grant_type".to_string(), "authorization_code".to_string())));
        assert!(body.contains(&("code".to_string(), "code-1".to_string())));
        assert!(body.contains(&("code_verifier".to_string(), "verifier-1".to_string())));
        assert!(body.contains(&(
            "redirect_uri".to_string(),
            "http://localhost:17251/".to_string()
        )));
        assert!(!body.iter().any(|(k, _)| k == "client_secret"));
    }

    #[test]
    fn refresh_request_body_omits_verifier() {
        let body = refresh_request_body(&test_config(), "rt-1");
        assert!(body.contains(&("grant_type".to_string(), "refresh_token".to_string())));
        assert!(body.contains(&("refresh_token".to_string(), "rt-1".to_string())));
        assert!(!body.iter().any(|(k, _)| k == "code_verifier"));
        assert!(!body.iter().any(|(k, _)| k == "client_secret"));
    }

    #[test]
    fn parse_token_response_extracts_tokens() {
        let t = parse_token_response(
            r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3599}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "at-1");
        assert_eq!(t.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(t.expires_in_secs, 3599);

        let t2 = parse_token_response(r#"{"access_token":"at-2","expires_in":3600}"#).unwrap();
        assert_eq!(t2.refresh_token, None, "refresh rotation is optional");

        assert!(parse_token_response(r#"{"error":"invalid_grant"}"#).is_err());
        assert!(parse_token_response("not json").is_err());
    }

    #[test]
    fn google_config_uses_drive_file_scope() {
        let c = test_config();
        assert_eq!(c.scope, "https://www.googleapis.com/auth/drive.appdata");
        assert_eq!(
            c.authorization_endpoint,
            "https://accounts.google.com/o/oauth2/v2/auth"
        );
        assert_eq!(c.token_endpoint, "https://oauth2.googleapis.com/token");
        assert_eq!(c.redirect_uri, "http://localhost:17251/");
        assert!(c.client_secret.is_none(), "public client has no secret");
    }

    #[test]
    fn session_token_lifecycle_and_expiry() {
        let mut session = AuthSession::new(test_config());
        assert!(!session.has_valid_access_token());
        assert!(!session.has_valid_access_token(), "no tokens at all");

        session.store_tokens(&TokenResponse {
            access_token: "at-1".to_string(),
            refresh_token: Some("rt-1".to_string()),
            expires_in_secs: 3600,
        });
        assert!(session.has_valid_access_token());
        assert_eq!(session.access_token(), Some("at-1"));
        assert_eq!(session.refresh_token.as_deref(), Some("rt-1"));

        // A refresh that does not rotate the token keeps the old refresh token.
        session.store_tokens(&TokenResponse {
            access_token: "at-2".to_string(),
            refresh_token: None,
            expires_in_secs: 3600,
        });
        assert_eq!(session.access_token(), Some("at-2"));
        assert_eq!(session.refresh_token.as_deref(), Some("rt-1"));

        // Sign-out clears the refresh token; the in-memory access token still
        // authorizes the current pass — only a full sign-out (no tokens at
        // all) skips it. The Credential Manager round-trip is OS integration,
        // not unit-testable here.
        session.refresh_token = None;
        assert!(
            session.has_valid_access_token(),
            "valid access token still authorizes"
        );
        session.access_token = None;
        session.expires_at = None;
        assert!(!session.has_valid_access_token(), "no tokens at all");
    }

    #[test]
    fn session_expiry_respects_margin() {
        let mut session = AuthSession::new(test_config());
        session.store_tokens(&TokenResponse {
            access_token: "at-1".to_string(),
            refresh_token: None,
            expires_in_secs: 3600,
        });
        assert!(session.has_valid_access_token());
        session.expires_at = Some(Instant::now() - std::time::Duration::from_millis(1));
        assert!(!session.has_valid_access_token(), "token expired");
        assert!(session.access_token().is_none());
    }
}
