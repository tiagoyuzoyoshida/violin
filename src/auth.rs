use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpListener;

pub const DEFAULT_SPOTIFY_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const DEFAULT_REDIRECT_PORT: u16 = 8888;
pub const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
pub const DEFAULT_REDIRECT_PATH: &str = "/login";
pub const CUSTOM_REDIRECT_PATH: &str = "/callback";
pub const SPOTIFY_SCOPES: &[&str] = &["user-library-read", "streaming"];
const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const TOKEN_EXPIRY_SKEW_SECS: u64 = 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const PKCE_VERIFIER_LEN: usize = 64;
const OAUTH_STATE_LEN: usize = 32;
const PKCE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const TOKEN_FILE_MODE: u32 = 0o600;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("token store I/O error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("token store JSON error at {}: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("OAuth HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("OAuth callback server failed on {addr}: {source}")]
    CallbackBind {
        addr: String,
        source: std::io::Error,
    },
    #[error("OAuth callback failed: {0}")]
    Callback(String),
    #[error("Spotify token endpoint rejected request (HTTP {status}): {message}")]
    TokenEndpoint { status: u16, message: String },
    #[error("stored OAuth token is expired and has no refresh token")]
    MissingRefreshToken,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix_secs: u64,
    pub scope: Option<String>,
}

impl fmt::Debug for StoredTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let refresh_display = if self.refresh_token.is_some() {
            "[REDACTED]"
        } else {
            "None"
        };
        f.debug_struct("StoredTokens")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &refresh_display)
            .field("expires_at_unix_secs", &self.expires_at_unix_secs)
            .field("scope", &self.scope)
            .finish()
    }
}

impl StoredTokens {
    /// Returns `true` if the token is expired or within `TOKEN_EXPIRY_SKEW_SECS` of expiry.
    pub fn is_expired(&self, now_unix_secs: u64) -> bool {
        now_unix_secs.saturating_add(TOKEN_EXPIRY_SKEW_SECS) >= self.expires_at_unix_secs
    }
}

fn random_url_safe_string(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| {
            let idx = rng.random_range(0..PKCE_ALPHABET.len());
            PKCE_ALPHABET[idx] as char
        })
        .collect()
}

/// Generates a 64-character URL-safe PKCE code verifier.
pub fn generate_code_verifier() -> String {
    random_url_safe_string(PKCE_VERIFIER_LEN)
}

/// Computes the RFC 7636 S256 PKCE code challenge for `verifier`.
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Builds the Spotify `/authorize` URL with PKCE S256 parameters.
pub fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    scopes: &[&str],
) -> String {
    let scope_joined = scopes.join(" ");
    let url = reqwest::Url::parse_with_params(
        SPOTIFY_AUTHORIZE_URL,
        &[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge),
            ("state", state),
            ("scope", &scope_joined),
        ],
    )
    .expect("SPOTIFY_AUTHORIZE_URL is a valid URL");
    url.into()
}

fn redirect_listener_config(client_id: &str) -> (u16, &'static str) {
    if client_id == DEFAULT_SPOTIFY_CLIENT_ID {
        (0, DEFAULT_REDIRECT_PATH)
    } else {
        (DEFAULT_REDIRECT_PORT, CUSTOM_REDIRECT_PATH)
    }
}

fn extract_code_from_url(url: &reqwest::Url, expected_state: &str) -> Result<String, AuthError> {
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut oauth_error: Option<String> = None;

    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => oauth_error = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(err) = oauth_error {
        return Err(AuthError::Callback(format!(
            "Spotify returned error: {err}"
        )));
    }

    match state.as_deref() {
        Some(actual) if actual == expected_state => {}
        Some(_) => return Err(AuthError::Callback("OAuth state mismatch".to_string())),
        None => {
            return Err(AuthError::Callback(
                "missing OAuth state parameter".to_string(),
            ));
        }
    }

    match code {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(AuthError::Callback(
            "missing authorization code in callback".to_string(),
        )),
    }
}

/// Parses an HTTP request line from the loopback OAuth callback and validates `state` and `code`.
pub fn parse_callback_request(
    request_line: &str,
    expected_state: &str,
) -> Result<String, AuthError> {
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| AuthError::Callback("empty HTTP request line".to_string()))?;
    let target = parts
        .next()
        .ok_or_else(|| AuthError::Callback("missing request target".to_string()))?;

    if method != "GET" {
        return Err(AuthError::Callback(format!(
            "unexpected HTTP method {method}"
        )));
    }

    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|err| AuthError::Callback(format!("invalid callback URL: {err}")))?;

    extract_code_from_url(&url, expected_state)
}

/// Parses a full redirected callback URL pasted by the user and validates `state` and `code`.
pub fn parse_callback_url(input: &str, expected_state: &str) -> Result<String, AuthError> {
    let trimmed = input.trim();
    let url = reqwest::Url::parse(trimmed)
        .map_err(|err| AuthError::Callback(format!("invalid callback URL: {err}")))?;

    extract_code_from_url(&url, expected_state)
}

pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// Creates a token store backed by `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Loads cached tokens from disk, returning `Ok(None)` if the file does not exist.
    pub fn load(&self) -> Result<Option<StoredTokens>, AuthError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(AuthError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };

        let tokens = serde_json::from_str(&raw).map_err(|source| AuthError::Json {
            path: self.path.clone(),
            source,
        })?;
        Ok(Some(tokens))
    }

    /// Saves tokens to disk, creating parent directories and enforcing `0600` Unix permissions.
    pub fn save(&self, tokens: &StoredTokens) -> Result<(), AuthError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| AuthError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let payload = serde_json::to_vec_pretty(tokens).map_err(|source| AuthError::Json {
            path: self.path.clone(),
            source,
        })?;

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(TOKEN_FILE_MODE)
            .open(&self.path)
            .map_err(|source| AuthError::Io {
                path: self.path.clone(),
                source,
            })?;

        file.write_all(&payload).map_err(|source| AuthError::Io {
            path: self.path.clone(),
            source,
        })?;

        // Enforce 0600 even if the file already existed with broader permissions.
        fs::set_permissions(&self.path, Permissions::from_mode(TOKEN_FILE_MODE)).map_err(
            |source| AuthError::Io {
                path: self.path.clone(),
                source,
            },
        )?;

        Ok(())
    }

    /// Deletes the cached token file if it exists. Returns `Ok(true)` if removed, `Ok(false)` if absent.
    pub fn clear(&self) -> Result<bool, AuthError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
            Err(source) => Err(AuthError::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct AuthManager {
    client_id: String,
    token_store: TokenStore,
    http: reqwest::Client,
}

impl AuthManager {
    /// Creates an `AuthManager` with a shared `reqwest::Client`.
    pub fn new(client_id: String, token_store: TokenStore) -> Result<Self, AuthError> {
        let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
        Ok(Self {
            client_id,
            token_store,
            http,
        })
    }

    /// Returns a valid access token from cache, refresh, or interactive PKCE login.
    pub async fn ensure_valid_token(&self) -> Result<String, AuthError> {
        if let Some(tokens) = self.token_store.load()? {
            if !tokens.is_expired(now_unix_secs()) {
                return Ok(tokens.access_token);
            }
            if tokens.refresh_token.is_some() {
                let refreshed = self.refresh_tokens(&tokens).await?;
                self.token_store.save(&refreshed)?;
                return Ok(refreshed.access_token);
            }
        }

        let fresh = self.login_interactive().await?;
        self.token_store.save(&fresh)?;
        Ok(fresh.access_token)
    }

    /// Runs interactive browser PKCE login regardless of cached tokens, saves the new tokens, and returns the access token.
    pub async fn login(&self) -> Result<String, AuthError> {
        let fresh = self.login_interactive().await?;
        self.token_store.save(&fresh)?;
        Ok(fresh.access_token)
    }

    /// Reloads cached tokens and forces a refresh via `refresh_token`.
    pub async fn force_refresh(&self) -> Result<String, AuthError> {
        let tokens = self
            .token_store
            .load()?
            .ok_or(AuthError::MissingRefreshToken)?;
        let refreshed = self.refresh_tokens(&tokens).await?;
        self.token_store.save(&refreshed)?;
        Ok(refreshed.access_token)
    }

    /// Returns a valid access token from cache or refresh token without starting interactive login.
    /// Intended for background workers running while the TUI owns the terminal.
    pub async fn valid_token_non_interactive(&self) -> Result<String, AuthError> {
        let tokens = self
            .token_store
            .load()?
            .ok_or(AuthError::MissingRefreshToken)?;
        if !tokens.is_expired(now_unix_secs()) {
            return Ok(tokens.access_token);
        }
        let refreshed = self.refresh_tokens(&tokens).await?;
        self.token_store.save(&refreshed)?;
        Ok(refreshed.access_token)
    }

    async fn refresh_tokens(&self, current: &StoredTokens) -> Result<StoredTokens, AuthError> {
        let refresh_token = current
            .refresh_token
            .as_deref()
            .ok_or(AuthError::MissingRefreshToken)?;

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ];
        self.post_token_request(&params, current.refresh_token.clone())
            .await
    }

    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<StoredTokens, AuthError> {
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", verifier),
        ];
        self.post_token_request(&params, None).await
    }

    async fn post_token_request(
        &self,
        params: &[(&str, &str)],
        fallback_refresh_token: Option<String>,
    ) -> Result<StoredTokens, AuthError> {
        let response = self
            .http
            .post(SPOTIFY_TOKEN_URL)
            .form(params)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let message = serde_json::from_str::<TokenErrorResponse>(&body)
                .ok()
                .and_then(|err| err.error_description.or(err.error))
                .unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("token request failed")
                        .to_string()
                });
            return Err(AuthError::TokenEndpoint {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: TokenResponse = response.json().await?;
        Ok(StoredTokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token.or(fallback_refresh_token),
            expires_at_unix_secs: now_unix_secs().saturating_add(parsed.expires_in),
            scope: parsed.scope,
        })
    }

    async fn login_interactive(&self) -> Result<StoredTokens, AuthError> {
        let verifier = generate_code_verifier();
        let challenge = code_challenge(&verifier);
        let state = random_url_safe_string(OAUTH_STATE_LEN);
        let (bind_port, redirect_path) = redirect_listener_config(&self.client_id);
        let bind_addr = format!("127.0.0.1:{bind_port}");

        let listener =
            TcpListener::bind(&bind_addr)
                .await
                .map_err(|source| AuthError::CallbackBind {
                    addr: bind_addr.clone(),
                    source,
                })?;

        let actual_port = listener
            .local_addr()
            .map_err(|source| AuthError::CallbackBind {
                addr: bind_addr.clone(),
                source,
            })?
            .port();
        let actual_bind_addr = format!("127.0.0.1:{actual_port}");
        let redirect_uri = format!("http://127.0.0.1:{actual_port}{redirect_path}");

        let authorize_url = build_authorize_url(
            &self.client_id,
            &redirect_uri,
            &state,
            &challenge,
            SPOTIFY_SCOPES,
        );

        eprintln!(
            "Log in to Spotify by opening this URL in your browser:\n\n{authorize_url}\n\nWaiting for browser login (or paste the redirected URL here and press Enter)..."
        );

        let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();
        let mut stdin_open = true;
        let loopback_fut =
            accept_loopback_callback(&listener, &actual_bind_addr, redirect_path, &state);
        tokio::pin!(loopback_fut);

        let code = loop {
            tokio::select! {
                res = &mut loopback_fut => break res?,
                line_res = stdin_lines.next_line(), if stdin_open => {
                    match line_res {
                        Ok(Some(line)) => {
                            if line.trim().is_empty() {
                                continue;
                            }
                            match parse_callback_url(&line, &state) {
                                Ok(code) => break code,
                                Err(err) => {
                                    eprintln!("Invalid callback URL ({err}). Paste the full redirected URL or finish in your browser...");
                                }
                            }
                        }
                        Ok(None) | Err(_) => {
                            stdin_open = false;
                        }
                    }
                }
            }
        };

        let tokens = self.exchange_code(&code, &verifier, &redirect_uri).await?;
        eprintln!("Spotify login successful.");
        Ok(tokens)
    }
}

async fn accept_loopback_callback(
    listener: &TcpListener,
    bind_addr: &str,
    expected_path: &str,
    expected_state: &str,
) -> Result<String, AuthError> {
    loop {
        let (mut stream, _) =
            listener
                .accept()
                .await
                .map_err(|source| AuthError::CallbackBind {
                    addr: bind_addr.to_string(),
                    source,
                })?;

        let mut reader = BufReader::new(&mut stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).await.map_err(|err| {
            AuthError::Callback(format!("failed to read callback request: {err}"))
        })?;

        let trimmed_line = request_line.trim_end();
        let target = trimmed_line.split_whitespace().nth(1).unwrap_or("");
        let target_path = target.split('?').next().unwrap_or("");

        if target_path != expected_path {
            let response =
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
            continue;
        }

        match parse_callback_request(trimmed_line, expected_state) {
            Ok(code) => {
                let html = "<!doctype html><html><body><p>violin login complete. You may close this tab.</p></body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
                return Ok(code);
            }
            Err(err) => {
                let html = "<!doctype html><html><body><p>violin login failed. Return to your terminal for details.</p></body></html>";
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
                return Err(err);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_tokens_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "violin-test-{name}-{}-{nanos}/tokens.json",
            std::process::id()
        ))
    }

    #[test]
    fn code_challenge_matches_rfc_7636_test_vector() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(code_challenge(verifier), expected);
    }

    #[test]
    fn generate_code_verifier_has_expected_length_and_charset() {
        let verifier = generate_code_verifier();
        assert_eq!(verifier.len(), 64);
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }

    #[test]
    fn build_authorize_url_encodes_all_pkce_parameters() {
        let url = build_authorize_url(
            "client_123",
            REDIRECT_URI,
            "state_xyz",
            "challenge_abc",
            SPOTIFY_SCOPES,
        );
        let parsed = reqwest::Url::parse(&url).unwrap();
        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("accounts.spotify.com"));
        assert_eq!(parsed.path(), "/authorize");

        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("response_type").map(String::as_str),
            Some("code")
        );
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some("client_123")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some(REDIRECT_URI)
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some("challenge_abc")
        );
        assert_eq!(params.get("state").map(String::as_str), Some("state_xyz"));
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some("user-library-read streaming")
        );
    }

    #[test]
    fn parse_callback_request_handles_success_and_error_cases() {
        let code = parse_callback_request(
            "GET /callback?code=auth_code_1&state=expected_state HTTP/1.1",
            "expected_state",
        )
        .unwrap();
        assert_eq!(code, "auth_code_1");

        let mismatch = parse_callback_request(
            "GET /callback?code=auth_code_1&state=wrong HTTP/1.1",
            "expected_state",
        )
        .unwrap_err();
        assert!(matches!(mismatch, AuthError::Callback(_)));

        let oauth_err = parse_callback_request(
            "GET /callback?error=access_denied&state=expected_state HTTP/1.1",
            "expected_state",
        )
        .unwrap_err();
        assert!(matches!(oauth_err, AuthError::Callback(_)));

        let missing_code = parse_callback_request(
            "GET /callback?state=expected_state HTTP/1.1",
            "expected_state",
        )
        .unwrap_err();
        assert!(matches!(missing_code, AuthError::Callback(_)));
    }

    #[test]
    fn stored_tokens_is_expired_respects_skew() {
        let tokens = StoredTokens {
            access_token: "secret-access".to_string(),
            refresh_token: Some("secret-refresh".to_string()),
            expires_at_unix_secs: 1_000,
            scope: Some("streaming".to_string()),
        };
        assert!(!tokens.is_expired(939));
        assert!(tokens.is_expired(940));
        assert!(tokens.is_expired(1_000));
    }

    #[test]
    fn stored_tokens_debug_redacts_secrets() {
        let tokens = StoredTokens {
            access_token: "super-secret-access-token".to_string(),
            refresh_token: Some("super-secret-refresh-token".to_string()),
            expires_at_unix_secs: 1_700_000_000,
            scope: Some("user-library-read".to_string()),
        };
        let formatted = format!("{tokens:?}");
        assert!(!formatted.contains("super-secret-access-token"));
        assert!(!formatted.contains("super-secret-refresh-token"));
        assert!(formatted.contains("[REDACTED]"));
    }

    #[test]
    fn token_store_round_trip_enforces_0600_permissions() {
        let path = temp_tokens_path("round-trip");
        let store = TokenStore::new(path.clone());
        assert_eq!(store.load().unwrap(), None);

        let tokens = StoredTokens {
            access_token: "tok_a".to_string(),
            refresh_token: Some("tok_r".to_string()),
            expires_at_unix_secs: 2_000_000_000,
            scope: Some("user-library-read streaming".to_string()),
        };
        store.save(&tokens).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(store.load().unwrap(), Some(tokens));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn valid_token_non_interactive_returns_cached_token_when_fresh() {
        let path = temp_tokens_path("non-interactive-fresh");
        let store = TokenStore::new(path.clone());
        let tokens = StoredTokens {
            access_token: "fresh-access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at_unix_secs: now_unix_secs() + 3600,
            scope: Some("user-library-read streaming".to_string()),
        };
        store.save(&tokens).unwrap();

        let auth = AuthManager::new("client_id".to_string(), store).unwrap();
        let token = auth.valid_token_non_interactive().await.unwrap();
        assert_eq!(token, "fresh-access-token");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn valid_token_non_interactive_errors_when_no_token_file_exists() {
        let path = temp_tokens_path("non-interactive-missing");
        let store = TokenStore::new(path);
        let auth = AuthManager::new("client_id".to_string(), store).unwrap();

        let err = auth.valid_token_non_interactive().await.unwrap_err();
        assert!(matches!(err, AuthError::MissingRefreshToken));
    }

    #[test]
    fn redirect_listener_config_selects_ephemeral_login_for_default_and_8888_callback_for_custom() {
        assert_eq!(
            redirect_listener_config(DEFAULT_SPOTIFY_CLIENT_ID),
            (0, "/login")
        );
        assert_eq!(
            redirect_listener_config("custom_client_id"),
            (8888, "/callback")
        );
    }

    #[test]
    fn parse_callback_url_handles_success_and_error_cases() {
        let code = parse_callback_url(
            "  http://127.0.0.1:41234/login?code=pasted_code&state=expected_state \n",
            "expected_state",
        )
        .unwrap();
        assert_eq!(code, "pasted_code");

        let mismatch = parse_callback_url(
            "http://127.0.0.1:41234/login?code=pasted_code&state=wrong",
            "expected_state",
        )
        .unwrap_err();
        assert!(matches!(mismatch, AuthError::Callback(_)));

        let oauth_err = parse_callback_url(
            "http://127.0.0.1:41234/login?error=access_denied&state=expected_state",
            "expected_state",
        )
        .unwrap_err();
        assert!(matches!(oauth_err, AuthError::Callback(_)));

        let missing_code = parse_callback_url(
            "http://127.0.0.1:41234/login?state=expected_state",
            "expected_state",
        )
        .unwrap_err();
        assert!(matches!(missing_code, AuthError::Callback(_)));

        let malformed = parse_callback_url("not a url", "expected_state").unwrap_err();
        assert!(matches!(malformed, AuthError::Callback(_)));
    }

    #[test]
    fn token_store_clear_removes_existing_file_and_returns_false_when_absent() {
        let path = temp_tokens_path("clear");
        let store = TokenStore::new(path.clone());
        assert!(!store.clear().unwrap());

        let tokens = StoredTokens {
            access_token: "tok_a".to_string(),
            refresh_token: Some("tok_r".to_string()),
            expires_at_unix_secs: 2_000_000_000,
            scope: None,
        };
        store.save(&tokens).unwrap();
        assert!(path.exists());

        assert!(store.clear().unwrap());
        assert!(!path.exists());
        assert!(!store.clear().unwrap());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn accept_loopback_callback_ignores_favicon_and_accepts_matching_path() {
        use tokio::io::AsyncReadExt as _;
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bind_addr = addr.to_string();

        let server = tokio::spawn(async move {
            accept_loopback_callback(&listener, &bind_addr, "/login", "expected").await
        });

        let mut favicon_stream = TcpStream::connect(addr).await.unwrap();
        favicon_stream
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut favicon_resp = String::new();
        favicon_stream
            .read_to_string(&mut favicon_resp)
            .await
            .unwrap();
        assert!(favicon_resp.starts_with("HTTP/1.1 404 Not Found"));

        let mut login_stream = TcpStream::connect(addr).await.unwrap();
        login_stream
            .write_all(
                b"GET /login?code=ok_code&state=expected HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();
        let mut login_resp = String::new();
        login_stream.read_to_string(&mut login_resp).await.unwrap();
        assert!(login_resp.starts_with("HTTP/1.1 200 OK"));

        let code = server.await.unwrap().unwrap();
        assert_eq!(code, "ok_code");
    }
}
