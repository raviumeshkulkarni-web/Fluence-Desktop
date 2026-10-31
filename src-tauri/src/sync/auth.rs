// Fluence sync - Windows OAuth 2.0 authorization-code flow (spec §24).
//
// Loopback PKCE (S256): the browser is pointed at the provider's
// authorization endpoint with a code challenge; the provider redirects to
// `http://localhost:<port>/`, where an in-process listener captures the code.
// The refresh token is persisted by the caller (Credential Manager -
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

/// Hard byte cap on the loopback request head (audit finding S4). A browser
/// redirect request line plus headers is a few hundred bytes; 4096 covers it
/// with room to spare and keeps a stalled or hostile client from pinning the
/// listener. The cap is a ceiling, never grown past: reads are sized to the
/// room left in the buffer.
pub const REDIRECT_HEAD_MAX_BYTES: usize = 4096;

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
            AuthError::NoRefreshToken => write!(f, "no refresh token. Sign in again"),
            AuthError::Timeout => write!(f, "sign-in timed out. Please try again"),
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
        .append_pair("code_challenge_method", "S256")
        // `access_type=offline` is what makes Google issue a refresh token
        // when this client+user already holds a grant (audit finding M1).
        // Without it, `prompt=select_account` alone suppresses the forced
        // re-consent screen, so an already-authorized account comes back with
        // an access token only and sign-in dies on `NoRefreshToken` with no
        // recovery path. Kept alongside `prompt=select_account`, not instead
        // of it: the chooser still shows, the credential still gets persisted.
        .append_pair("access_type", "offline");
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

/// Bind the loopback redirect listener for `redirect_uri`'s port.
///
/// Split out of [`listen_for_redirect`] so the sign-in flow can bind *before*
/// opening the system browser. Previously the bind happened inside
/// `listen_for_redirect`, which ran after the browser was already spawned: a
/// fast redirect could arrive at an unbound port and be refused by the OS,
/// failing an otherwise-successful sign-in (audit finding S4).
pub async fn bind_redirect_listener(
    redirect_uri: &str,
) -> Result<tokio::net::TcpListener, AuthError> {
    let url = Url::parse(redirect_uri).map_err(|_| AuthError::BadRedirect)?;
    let port = url.port().ok_or(AuthError::BadRedirect)?;
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| AuthError::Network(e.to_string()))
}

/// Whether an accumulated request head contains the HTTP header terminator:
/// `\r\n\r\n` per RFC 7230, with a bare `\n\n` accepted as a fallback for a
/// client that sends LF-only line endings. Pure, offline-testable.
pub fn redirect_head_is_complete(head: &[u8]) -> bool {
    head.windows(4).any(|w| w == b"\r\n\r\n") || head.windows(2).any(|w| w == b"\n\n")
}

/// Feed one read chunk into the accumulating request head and report whether
/// the head is now complete (see [`redirect_head_is_complete`]).
///
/// Split out of [`accept_redirect`] so the accumulation is unit-testable
/// without a socket: the accept path used to do ONE `read()` into a 4096-byte
/// buffer and parse whatever the kernel handed over. A request line split
/// across TCP segments (which is entirely up to the kernel, not the peer) was
/// truncated mid-token, so `parse_redirect_request` failed and sign-in died
/// with `BadRedirect` - audit finding S4.
pub fn push_redirect_head(head: &mut Vec<u8>, chunk: &[u8]) -> bool {
    head.extend_from_slice(chunk);
    redirect_head_is_complete(head)
}

// Test seam for the accept loop's byte cap.
//
// `push_redirect_head` deliberately does not clip: it extends by exactly what it
// is given. That makes `take = room.min(chunk.len())` the ONLY thing enforcing
// `REDIRECT_HEAD_MAX_BYTES`, so a future edit that sizes a read without
// consulting `room` silently removes the ceiling - and no test driving the real
// loop could see it, because the observable outcome (BadRedirect on an over-long
// request) is identical either way.
//
// This seam records the largest head the loop accumulated, so the cap can be
// asserted directly. `cfg(test)` only: production neither observes nor pays for
// it, and it is thread-local because the harness runs tests in parallel.
#[cfg(test)]
thread_local! {
    static ACCEPT_HEAD_MAX_SEEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn note_accept_head_len(len: usize) {
    ACCEPT_HEAD_MAX_SEEN.with(|seen| {
        if len > seen.get() {
            seen.set(len);
        }
    });
}

#[cfg(test)]
pub(crate) fn accept_head_max_seen() -> usize {
    ACCEPT_HEAD_MAX_SEEN.with(|seen| seen.get())
}

#[cfg(test)]
pub(crate) fn reset_accept_head_max() {
    ACCEPT_HEAD_MAX_SEEN.with(|seen| seen.set(0));
}

/// Bytes still available under [`REDIRECT_HEAD_MAX_BYTES`]. Pure.
pub fn redirect_head_room(head: &[u8]) -> usize {
    REDIRECT_HEAD_MAX_BYTES.saturating_sub(head.len())
}

/// In-process loopback listener (spec §24): accept one HTTP request on an
/// already-bound listener, validate state, extract the code, answer with a
/// minimal HTML page, and return the code. Never binds an external address.
pub async fn listen_for_redirect(
    redirect_uri: &str,
    expected_state: &str,
) -> Result<String, AuthError> {
    let listener = bind_redirect_listener(redirect_uri).await?;
    accept_redirect(&listener, expected_state).await
}

/// Accept-side of [`listen_for_redirect`], kept separate so the listener can be
/// bound before the browser is opened (see [`bind_redirect_listener`]).
pub async fn accept_redirect(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, AuthError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // The whole accept+handshake is bounded: on timeout the listener and the
    // accepted stream drop here, releasing the loopback socket exactly as on
    // the success path.
    let handshake = async {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;

        // Read in a loop until the header terminator arrives (S4). Every read
        // is sized to the room left under the byte cap, so the buffer can
        // never grow past `REDIRECT_HEAD_MAX_BYTES`, and EOF (`n == 0`) ends
        // the loop without adding a new error variant.
        let mut head: Vec<u8> = Vec::with_capacity(1024);
        loop {
            let room = redirect_head_room(&head);
            if room == 0 {
                break;
            }
            let mut chunk = [0u8; 1024];
            let take = room.min(chunk.len());
            let n = stream
                .read(&mut chunk[..take])
                .await
                .map_err(|e| AuthError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            let complete = push_redirect_head(&mut head, &chunk[..n]);
            // Recorded before the completion break so the read that finishes the
            // request is inside the observed ceiling, not after it.
            #[cfg(test)]
            note_accept_head_len(head.len());
            if complete {
                break;
            }
        }
        let head = std::str::from_utf8(&head).map_err(|_| AuthError::BadRedirect)?;
        let request_line = head.lines().next().ok_or(AuthError::BadRedirect)?;
        let code = parse_redirect_request(request_line, expected_state)?;

        let body = "Fluence sync authorized. You can close this window.";
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

/// Longest provider-supplied `error=` value kept in an [`AuthError::AccessDenied`].
///
/// RFC 6749 §4.1.2.1 fixes the set to `invalid_request`, `unauthorized_client`,
/// `access_denied`, `unsupported_response_type`, `invalid_scope`,
/// `server_error`, `temporarily_unavailable` - the longest is 24 characters.
/// Anything materially longer is not a provider string, so anything longer is
/// clamped rather than rendered into the sync toast verbatim.
pub const OAUTH_ERROR_MAX_CHARS: usize = 32;

/// Normalize an attacker- or provider-supplied `error=` value.
///
/// The redirect arrives on a loopback listener bound to a FIXED port
/// ([`crate::sync::scheduler::SYNC_REDIRECT_PORT`]) that accepts exactly one
/// connection, so any local process can win that accept and put any bytes it
/// likes on this path. The value therefore gets an ALLOWLIST rather than a
/// denylist: RFC 6749 §4.1.2.1 defines `error` as
/// `1*VSCHAR`-derived but the real values are a fixed set of
/// `[A-Za-z0-9_-]` tokens, and anything else can only be there to spoof the
/// one-line sync toast (newline, bidi override, zero-width joiner, an
/// embedded URL, a megabyte of padding). A value that normalizes to nothing
/// becomes the one error Google actually sends, `access_denied`. This is
/// defense in depth - it matters far less once the state is validated first,
/// but the string still reaches a user-visible error path.
fn bounded_oauth_error(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(OAUTH_ERROR_MAX_CHARS)
        .collect();
    if cleaned.is_empty() {
        return "access_denied".to_string();
    }
    cleaned
}

/// Parse `GET /?code=..&state=.. HTTP/1.1` and validate the state.
///
/// **State is validated FIRST, before anything else in the query is looked
/// at** (audit finding C9). The loopback listener is single-accept on a fixed
/// port, so a local process can win the accept and inject a request of its own
/// choosing. The old order returned on `error=` before the state check, which
/// did two things an attacker controls: it surfaced an arbitrary
/// attacker-chosen string as the app's sign-in error, and because `accept`
/// is single-shot it consumed the connection and killed the real sign-in
/// (the user's browser then had nowhere to land). Validating state first makes
/// an injected request indistinguishable from any other rejected redirect.
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
    match params.get("state") {
        Some(state) if state == expected_state => {}
        // Missing OR mismatched: never distinguishable from each other to an
        // attacker, and both mean "not the request we started".
        _ => return Err(AuthError::InvalidState),
    }
    if let Some(error) = params.get("error") {
        return Err(AuthError::AccessDenied(bounded_oauth_error(error)));
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

    /// Load the refresh token from the OS credential store, distinguishing
    /// absence from store failure:
    /// - `Ok(Some(token))` - a usable refresh token is present.
    /// - `Ok(None)` - no credential is stored (signed out / never signed in).
    ///   Callers must treat this as authentication-required, never as a fatal
    ///   error: the previous implementation collapsed this case into `Err`,
    ///   which latched the scheduler off permanently.
    /// - `Err(msg)` - the credential store itself failed; `msg` describes the
    ///   failure only and never contains credential material.
    ///
    /// An empty or whitespace-only stored blob is treated as absence: it is
    /// not a usable token, and minting an empty token only guarantees a 400
    /// from the token endpoint with a more confusing error.
    pub fn load_refresh_token_checked(&mut self) -> Result<Option<String>, String> {
        self.refresh_token = match crate::credentials::read_sync_refresh_token_checked()
            .map_err(|e| e.to_string())?
        {
            crate::credentials::SyncRefreshTokenRead::Present(token) => {
                let trimmed = token.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            crate::credentials::SyncRefreshTokenRead::Missing => None,
            crate::credentials::SyncRefreshTokenRead::StoreFailed(msg) => return Err(msg),
        };
        Ok(self.refresh_token.clone())
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

    // --- Credential load classification (audit finding C3) -----------------

    #[test]
    fn load_refresh_token_checked_present() {
        crate::credentials::set_test_sync_refresh_token_read(Some(Ok(
            crate::credentials::SyncRefreshTokenRead::Present("refresh-token".into()),
        )));
        let mut session = AuthSession::new(test_config());
        assert_eq!(
            session.load_refresh_token_checked().unwrap(),
            Some("refresh-token".to_string())
        );
        assert_eq!(session.refresh_token.as_deref(), Some("refresh-token"));
        crate::credentials::set_test_sync_refresh_token_read(None);
    }

    #[test]
    fn load_refresh_token_checked_missing_is_absence_not_error() {
        // The signed-out state must be Ok(None) so the scheduler surfaces
        // AuthRequired (reconnect prompt), never Fatal (permanent latch).
        crate::credentials::set_test_sync_refresh_token_read(Some(Ok(
            crate::credentials::SyncRefreshTokenRead::Missing,
        )));
        let mut session = AuthSession::new(test_config());
        assert_eq!(session.load_refresh_token_checked().unwrap(), None);
        assert!(session.refresh_token.is_none());
        crate::credentials::set_test_sync_refresh_token_read(None);
    }

    #[test]
    fn load_refresh_token_checked_empty_blob_is_absence() {
        crate::credentials::set_test_sync_refresh_token_read(Some(Ok(
            crate::credentials::SyncRefreshTokenRead::Present("   ".into()),
        )));
        let mut session = AuthSession::new(test_config());
        assert_eq!(session.load_refresh_token_checked().unwrap(), None);
        assert!(session.refresh_token.is_none());
        crate::credentials::set_test_sync_refresh_token_read(None);
    }

    #[test]
    fn load_refresh_token_checked_store_failure_carries_no_secret() {
        // A store failure is Err (retryable upstream); the message must
        // describe the failure only.
        crate::credentials::set_test_sync_refresh_token_read(Some(Ok(
            crate::credentials::SyncRefreshTokenRead::StoreFailed(
                "CredReadW failed: The system cannot find the file specified.".into(),
            ),
        )));
        let mut session = AuthSession::new(test_config());
        let err = session.load_refresh_token_checked().unwrap_err();
        assert!(err.contains("CredReadW"), "diagnosable failure text");
        assert!(
            !err.contains("refresh-token") && !err.contains("token:"),
            "no credential material in the error"
        );
        assert!(session.refresh_token.is_none());
        crate::credentials::set_test_sync_refresh_token_read(None);
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
    fn authorization_url_requests_offline_access() {
        // M1: without `access_type=offline` Google hands an already-granted
        // account an access token only, and the user is stuck on
        // "Google did not return a refresh token" with no recovery path.
        let url = authorization_url(&test_config(), "state-1", "challenge-1");
        assert!(url.contains("access_type=offline"));
        // The account chooser stays: offline access asks for the credential,
        // it does not suppress the multi-account screen.
        assert!(url.contains("prompt=select_account"));
        let parsed = url::Url::parse(&url).expect("still a valid URL");
        assert_eq!(
            parsed.query_pairs().next().map(|(k, _)| k.to_string()),
            Some("response_type".to_string()),
            "adding the pair must not reorder the frozen query"
        );
        assert!(url.contains("code_challenge_method=S256"));
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

    // --- Audit finding C9: state before error ----------------------------

    #[test]
    fn parse_redirect_request_error_with_valid_state_is_access_denied() {
        // Ordering (a): the provider's own denial, on a request that really is
        // ours, must still be reported as a denial.
        let line = "GET /?error=access_denied&state=state-1 HTTP/1.1";
        match parse_redirect_request(line, "state-1") {
            Err(AuthError::AccessDenied(m)) => assert_eq!(m, "access_denied"),
            other => panic!("expected AccessDenied, got {other:?}"),
        }
    }

    #[test]
    fn parse_redirect_request_error_with_wrong_state_is_invalid_state() {
        // Ordering (b): the attack shape. A local process that won the
        // single-accept on the fixed loopback port sends this; it must NOT get
        // its own text back, and it must NOT consume the real sign-in as a
        // "denial" - an injected request is just another rejected redirect.
        for line in [
            "GET /?error=access_denied&state=attacker-state HTTP/1.1",
            "GET /?state=attacker-state&error=access_denied HTTP/1.1",
        ] {
            assert!(
                matches!(
                    parse_redirect_request(line, "state-1"),
                    Err(AuthError::InvalidState)
                ),
                "{line} must be rejected on state, not surfaced"
            );
        }
    }

    #[test]
    fn parse_redirect_request_error_with_missing_state_is_invalid_state() {
        // Same attack with the state parameter omitted entirely. Note the
        // error query string is fully attacker-controlled here, so this is
        // the exact injection the old `error`-first ordering handed to the UI.
        assert!(matches!(
            parse_redirect_request("GET /?error=access_denied HTTP/1.1", "state-1"),
            Err(AuthError::InvalidState)
        ));
    }

    #[test]
    fn attacker_controlled_error_text_never_reaches_the_error() {
        // Belt and braces: even with a VALID state (the provider's real
        // denial), the value is bounded - control characters stripped, length
        // clamped, empty replaced. It reaches a user-visible sync toast.
        let hostile =
            "denied\u{202E}\n\u{0}Click here to re-authorise your account at https://evil.example";
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("error", hostile)
            .append_pair("state", "s")
            .finish();
        match parse_redirect_request(&format!("GET /?{query} HTTP/1.1"), "s") {
            Err(AuthError::AccessDenied(m)) => {
                assert!(!m.contains('\n'), "no newline in a one-line toast");
                assert!(!m.contains('\u{0}'), "no NUL");
                assert!(!m.contains('\u{202E}'), "no bidi override");
                assert!(!m.contains("evil.example"), "the URL must not survive");
                assert!(
                    m.chars().count() <= OAUTH_ERROR_MAX_CHARS,
                    "clamped to {OAUTH_ERROR_MAX_CHARS} chars, got {}",
                    m.chars().count()
                );
            }
            other => panic!("expected AccessDenied, got {other:?}"),
        }
    }

    #[test]
    fn oauth_error_bounding_leaves_real_provider_values_untouched() {
        // RFC 6749 4.1.2.1 values must pass through verbatim - the bound only
        // bites on something no provider would send.
        for real in [
            "invalid_request",
            "unauthorized_client",
            "access_denied",
            "unsupported_response_type",
            "invalid_scope",
            "server_error",
            "temporarily_unavailable",
        ] {
            assert_eq!(bounded_oauth_error(real), real, "{real} must survive");
        }
        assert_eq!(bounded_oauth_error(""), "access_denied");
        assert_eq!(bounded_oauth_error("   "), "access_denied");
        assert_eq!(bounded_oauth_error("\n\t"), "access_denied");
        assert_eq!(bounded_oauth_error("\u{202E}\u{200B}"), "access_denied");
        assert_eq!(
            bounded_oauth_error("  access_denied  "),
            "access_denied",
            "surrounding whitespace stripped"
        );
        // The allowlist is positional, not just a prefix cut: a spoof that
        // looks legitimate at the front still loses its payload.
        assert_eq!(
            bounded_oauth_error("access_denied reauthorise at evil.example"),
            "access_deniedreauthoriseatevilexample"
                .chars()
                .take(OAUTH_ERROR_MAX_CHARS)
                .collect::<String>()
        );
        let clamped = bounded_oauth_error(&"x".repeat(10_000));
        assert_eq!(clamped.chars().count(), OAUTH_ERROR_MAX_CHARS);
    }

    #[test]
    fn success_path_still_works_and_state_is_still_required() {
        // The success path is untouched...
        assert_eq!(
            parse_redirect_request("GET /?code=auth-code-1&state=state-1 HTTP/1.1", "state-1")
                .unwrap(),
            "auth-code-1"
        );
        // ...and a code with no state at all can never be accepted.
        assert!(matches!(
            parse_redirect_request("GET /?code=auth-code-1 HTTP/1.1", "state-1"),
            Err(AuthError::InvalidState)
        ));
    }

    #[test]
    fn redirect_head_accumulates_until_the_terminator() {
        let mut head: Vec<u8> = Vec::new();
        assert!(
            !push_redirect_head(&mut head, b"GET /?code=auth-code-1&sta"),
            "half a request line is not a complete head"
        );
        assert!(!push_redirect_head(&mut head, b"te=state-1 HTTP/1.1\r\n"));
        assert!(!push_redirect_head(&mut head, b"Host: localhost\r\n"));
        assert!(
            push_redirect_head(&mut head, b"\r\n"),
            "the blank line terminates the head"
        );
        let head = std::str::from_utf8(&head).expect("ASCII request");
        assert_eq!(
            head.lines().next().unwrap(),
            "GET /?code=auth-code-1&state=state-1 HTTP/1.1"
        );
        // The reassembled line parses; the split-token head does not.
        assert_eq!(
            parse_redirect_request(head.lines().next().unwrap(), "state-1").unwrap(),
            "auth-code-1"
        );
        // CHANGED EXPECTATION (audit finding C9). This asserted `BadRedirect`,
        // which was the pre-fix result of the state lookup being the LAST thing
        // tried. It is now `InvalidState`, because a request whose `state`
        // parameter is absent (here, truncated mid-token) is exactly the shape
        // an injected single-accept connection produces, and it must be
        // rejected as a state mismatch - before any other query parameter is
        // read - rather than as a generic malformed redirect.
        assert!(matches!(
            parse_redirect_request("GET /?code=auth-code-1&sta", "state-1"),
            Err(AuthError::InvalidState)
        ));
    }

    #[test]
    fn redirect_head_accepts_an_lf_only_terminator() {
        assert!(!redirect_head_is_complete(
            b"GET /?code=1&state=s HTTP/1.1\n"
        ));
        assert!(
            redirect_head_is_complete(b"GET /?code=1&state=s HTTP/1.1\nHost: localhost\n\n"),
            "bare LF line endings are tolerated as a fallback"
        );
    }

    #[test]
    fn redirect_head_is_bounded_by_the_byte_cap() {
        // The cap is a ceiling, never grown past: the accept loop sizes every
        // read by `redirect_head_room`, so a stalled client cannot make the
        // buffer grow without bound.
        assert_eq!(REDIRECT_HEAD_MAX_BYTES, 4096);
        let mut head: Vec<u8> = Vec::new();
        assert_eq!(redirect_head_room(&head), REDIRECT_HEAD_MAX_BYTES);

        let chunk = [b'x'; 500];
        for _ in 0..8 {
            let room = redirect_head_room(&head);
            push_redirect_head(&mut head, &chunk[..room.min(chunk.len())]);
        }

        // 8 whole chunks = 4000 bytes; the 9th is clipped to the 96 bytes of
        // room left, so the head lands exactly on the cap and the loop has
        // nothing more to ask for. An unclipped 9th write would overshoot.
        let room = redirect_head_room(&head);
        assert_eq!(room, 96, "8 x 500 bytes leaves 96 bytes of room");
        push_redirect_head(&mut head, &chunk[..room]);
        assert_eq!(head.len(), REDIRECT_HEAD_MAX_BYTES);
        assert_eq!(redirect_head_room(&head), 0);
    }

    #[tokio::test]
    async fn accept_redirect_reads_a_request_split_across_reads() {
        // S4 end-to-end: the kernel may deliver a request line in pieces. Bind
        // a listener, drive a client that writes the line split inside the
        // `state` value, and require the code to still come back.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { accept_redirect(&listener, "state-1").await });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback");
        // Split inside the state token: parsing the first read alone would
        // look for a `state` param that is not there yet.
        client
            .write_all(b"GET /?code=auth-code-1&sta")
            .await
            .expect("write head");
        client.flush().await.expect("flush head");
        // Let the first segment reach the listener so the next read sees it
        // on its own.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        client
            .write_all(b"te=state-1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write tail");
        client.flush().await.expect("flush tail");

        let mut page = Vec::new();
        let _ = client.read_to_end(&mut page).await;

        let code = server.await.unwrap().expect("sign-in succeeds");
        assert_eq!(code, "auth-code-1");
    }

    /// Drive the real accept loop with explicit payload chunks. Uses the real
    /// loop rather than the helpers, so it exercises `take = room.min(..)`
    /// against whatever the client actually puts on the wire.
    async fn drive_accept(chunks: Vec<Vec<u8>>) -> Result<String, AuthError> {
        // Clear before every drive so the recorded ceiling belongs to this run.
        // Thread-local, so parallel tests cannot observe each other's reads.
        reset_accept_head_max();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { accept_redirect(&listener, "state-1").await });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback");
        for chunk in &chunks {
            client.write_all(chunk).await.expect("write chunk");
            client.flush().await.expect("flush chunk");
        }
        // Read to EOF so the serve is not left writing into a dead socket.
        let mut page = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut page),
        )
        .await;
        let outcome = server.await.expect("accept task must not panic");
        // The ceiling is the whole point of these cases: assert it on every
        // drive, not only where the outcome happens to differ.
        assert!(
            accept_head_max_seen() <= REDIRECT_HEAD_MAX_BYTES,
            "the accept head reached {} bytes, past the {} byte cap - the read \
             sizing no longer consults `room`",
            accept_head_max_seen(),
            REDIRECT_HEAD_MAX_BYTES
        );
        outcome
    }

    /// `n` filler bytes, so a caller can build a payload of an exact length.
    fn filler(n: usize) -> Vec<u8> {
        vec![b'a'; n]
    }

    /// A well-formed request line carrying `code`, padded with `x` inside the
    /// query string until it is at least `pad_to` bytes. Never truncated: the
    /// natural length is the floor, so a caller can pass 0 for "just the line".
    fn request_line(code: &str, pad_to: usize) -> Vec<u8> {
        let mut line =
            format!("GET /?code={code}&state=state-1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .into_bytes();
        if line.len() < pad_to {
            let need = pad_to - line.len();
            let at = line
                .windows(2)
                .position(|w| w == b"\r\n")
                .expect("request line has a terminator");
            let mut padded = Vec::with_capacity(pad_to);
            padded.extend_from_slice(&line[..at]);
            padded.extend(std::iter::repeat(b'x').take(need));
            padded.extend_from_slice(&line[at..]);
            line = padded;
        }
        assert!(
            line.len() >= pad_to,
            "request line must reach the requested size"
        );
        line
    }

    /// Below cap: a normal, well-formed request still succeeds through the loop.
    #[tokio::test]
    async fn accept_loop_below_cap_reads_a_normal_request() {
        let line = request_line("small-code-1", 0);
        assert!(
            line.len() < REDIRECT_HEAD_MAX_BYTES,
            "this case must be below cap"
        );
        let code = drive_accept(vec![line])
            .await
            .expect("a small request must succeed");
        assert_eq!(code, "small-code-1");
    }

    /// At cap: a request whose head is exactly `REDIRECT_HEAD_MAX_BYTES` must
    /// still be read and parsed. If the loop stopped one byte early, or refused
    /// to size its final read to the few bytes of room left, this loses the
    /// terminator and reports BadRedirect.
    #[tokio::test]
    async fn accept_loop_at_cap_still_completes_the_request() {
        let line = request_line("at-cap-1", REDIRECT_HEAD_MAX_BYTES);
        assert_eq!(line.len(), REDIRECT_HEAD_MAX_BYTES);
        let code = drive_accept(vec![line])
            .await
            .expect("an exactly-at-cap request must complete");
        assert_eq!(code, "at-cap-1");
    }

    /// Just under the cap, split into many chunks, so the loop performs several
    /// reads and `room` shrinks across them.
    #[tokio::test]
    async fn accept_loop_just_under_cap_split_across_reads() {
        let line = request_line("under-cap-1", REDIRECT_HEAD_MAX_BYTES - 1);
        let mut chunks = line.chunks(700).map(<[u8]>::to_vec).collect::<Vec<_>>();
        assert!(chunks.len() > 1, "the request must span several reads");
        let code = drive_accept(chunks)
            .await
            .expect("a just-under-cap request must complete");
        debug_assert_eq!(&code, "under-cap-1");
    }

    /// Above cap, no terminator: the hostile part is tested for *termination*,
    /// not for parsing. A loop that grew the buffer, or that asked for more
    /// room than remained, would hang here or exhaust memory.
    #[tokio::test]
    async fn accept_loop_above_cap_without_terminator_terminates() {
        // Far past the cap, and never sends the terminator. EOF via the client
        // dropping ends the loop; either way the accept must return, not spin.
        let outcome = drive_accept(vec![filler(REDIRECT_HEAD_MAX_BYTES * 4)]).await;
        match outcome {
            // EOF with no request line parsed: absent, not a panic and not a hang.
            Err(AuthError::BadRedirect) => {}
            other => panic!(
                "an over-cap client with no terminator must terminate without a code, got {other:?}"
            ),
        }
    }

    /// Above cap WITH a later terminator: the cap must clip the buffer, so the
    /// terminator that arrives after the cap is NOT part of the head. The read
    /// therefore cannot parse a request, and - critically - must not hang.
    /// This is the "hostile sibling" of the at-cap case: same intent, opposite
    /// expectation, and the one that catches an off-by-one in `take = room.min`.
    #[tokio::test]
    async fn accept_loop_above_cap_terminator_after_the_cap_is_clipped() {
        let outcome = drive_accept(vec![
            filler(REDIRECT_HEAD_MAX_BYTES),
            request_line("clipped-code", 128),
        ])
        .await;
        // The head is already full when the terminator arrives, so the request
        // line is 4096 'a' bytes with no code: a parse failure, never a hang,
        // and never the code the far end tried to inject after the cap.
        match outcome {
            Err(AuthError::BadRedirect) => {}
            other => panic!("a terminator sent after the cap must not be parsed, got {other:?}"),
        }
    }

    /// A single read may legitimately return fewer bytes than asked for, so the
    /// loop must not treat a short read as EOF-only. Dribbling one byte at a
    /// time still completes, which is what makes the byte accounting hold.
    #[tokio::test]
    async fn accept_loop_dribbled_single_bytes_still_completes() {
        let line = request_line("dribble-1", 72);
        let chunks = line.iter().map(|b| vec![*b]).collect::<Vec<_>>();
        let code = drive_accept(chunks)
            .await
            .expect("a dribbled request still parses");
        assert_eq!(code, "dribble-1");
    }

    /// Cap enforcement when the head does NOT land on a read boundary.
    ///
    /// The over-cap cases above send whole multiples of the 1024-byte read, so
    /// `head` always lands exactly on the cap and `room` is 0 before anything
    /// else can arrive. A peer that leaves an awkward remainder - 4000 bytes,
    /// then a pause, then more - is the shape that actually exercises `take =
    /// room.min(..)`: `head` stops at 4000 with `room` at 96, and more than 96
    /// bytes are still in the socket. Sizing that read to the chunk instead of
    /// the room is the bug this exists to catch.
    #[tokio::test]
    async fn accept_loop_awkward_remainder_does_not_overshoot_the_cap() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        reset_accept_head_max();
        let server = tokio::spawn(async move { accept_redirect(&listener, "state-1").await });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback");
        // 4000 bytes, so the accumulating head stops at 4000 (1024 x3 + 928):
        // `room` is 96, which is below the chunk size and therefore the case
        // `room.min(..)` exists for. The pause keeps the tail out of the read
        // that would otherwise be filled to a chunk boundary by coalescing.
        client.write_all(&[b'a'; 4000]).await.expect("write head");
        client.flush().await.expect("flush head");
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        // The server may legitimately have finished and closed once the cap was
        // reached, so a reset here is not a failure of the case.
        let _ = client.write_all(&[b'a'; 400]).await;
        let _ = client.flush().await;

        let mut page = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut page),
        )
        .await;
        let outcome = server.await.expect("accept task must not panic");

        assert!(
            accept_head_max_seen() <= REDIRECT_HEAD_MAX_BYTES,
            "the accept head reached {} bytes, past the {} byte cap - a read was \
             sized to the chunk instead of the room",
            accept_head_max_seen(),
            REDIRECT_HEAD_MAX_BYTES
        );
        assert!(
            matches!(outcome, Err(AuthError::BadRedirect)),
            "an all-filler request has no code, so it must fail to parse, got {outcome:?}"
        );
    }

    // MUTATION PROBE for the accept-loop tests below.
    //
    // The cap is enforced ONLY by `take = room.min(chunk.len())`, because
    // `push_redirect_head` extends by exactly what it is given and does not clip.
    // So the genuine way to break this is to size a read without consulting the
    // room:
    //
    //   in `accept_redirect`, change
    //       let take = room.min(chunk.len());
    //   to
    //       let take = chunk.len();
    //
    //   -> `accept_loop_awkward_remainder_does_not_overshoot_the_cap` must fail
    //      on "past the ... byte cap". It is the only shape that actually binds:
    //      the whole-multiple-of-1024 cases land `head` exactly on the cap, so
    //      `room` is already 0 before anything else can arrive.
    //
    // Executed before this comment existed; the test went red as expected. The
    // recorded ceiling is observable through `accept_head_max_seen()`, so a
    // future reviewer does not have to take the ceiling on trust.
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
        // authorizes the current pass - only a full sign-out (no tokens at
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
