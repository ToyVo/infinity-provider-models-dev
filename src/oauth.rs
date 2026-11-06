//! Claude Pro/Max subscription auth (`OAuth`) for the `Anthropic` backend.
//!
//! Implements the same subscription sign-in the community bridges use
//! ([`pi-claude-bridge`](https://github.com/elidickinson/pi-claude-bridge),
//! [`anthropic-auth`](https://github.com/cortexkit/anthropic-auth)): a PKCE
//! authorize URL the user opens in a browser, a pasted-back code exchanged
//! for tokens, and transparent refreshes afterwards. Inference requests then
//! carry `Bearer` tokens with the `oauth-2025-04-20` beta instead of an API
//! key, so usage bills to the subscription instead of API credits.
//!
//! > **At your own risk.** This project is not affiliated with or endorsed
//! > by `Anthropic`. Using a subscription outside `Anthropic`'s official
//! > clients may violate their terms and can lead to account restrictions or
//! > suspension. The preset stays dormant unless explicitly opted in.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

/// `OAuth` client id embedded in `Claude Code` (public, shared by bridges).
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Authorize endpoints by account type.
pub const AUTHORIZE_URL_CONSOLE: &str = "https://platform.claude.com/oauth/authorize";
/// Authorize endpoints by account type.
pub const AUTHORIZE_URL_MAX: &str = "https://claude.com/cai/oauth/authorize";

/// Callback the browser flow redirects to (code is pasted back manually).
pub const CODE_CALLBACK_URL: &str = "https://platform.claude.com/oauth/code/callback";

/// Token endpoint for both code exchange and refresh.
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// Scopes requested at sign-in.
pub const OAUTH_SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Narrower scopes sent on refresh.
pub const REFRESH_SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// Beta flag billing inference to the subscription.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// `Anthropic` API version header.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// `User-Agent` used for token endpoint calls.
pub const TOKEN_USER_AGENT: &str = "axios/1.15.2";
/// `User-Agent` identifying requests as `Claude Code`, mirroring the bridges.
pub const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/2.1.302 (external, cli)";

/// Refresh this far ahead of expiry (5 minutes).
pub const REFRESH_SKEW_MS: i64 = 300_000;
/// Network timeout for token endpoint calls.
pub const TOKEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Explicit opt-in env var enabling the subscription preset.
pub const OPT_IN_ENV: &str = "INFINITY_CLAUDE_OAUTH";
/// Static bearer token (no refresh); implies opt-in.
pub const STATIC_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
/// Base URL override for subscription inference.
pub const BASE_URL_ENV: &str = "CLAUDE_OAUTH_BASE_URL";

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Failure talking to the token endpoint.
#[derive(Debug)]
pub struct OAuthError {
    /// Human-readable failure.
    pub message: String,
    /// HTTP status when the endpoint answered.
    pub status: Option<u16>,
    /// Whether retrying later might succeed (network/5xx).
    pub retryable: bool,
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for OAuthError {}

/// Freshly issued tokens.
#[derive(Debug)]
pub struct IssuedTokens {
    /// Bearer token for inference.
    pub access_token: String,
    /// Replacement refresh token, when the server rotated it.
    pub refresh_token: Option<String>,
    /// Lifetime in seconds.
    pub expires_in_secs: i64,
}

/// Credential set backing one subscription session.
#[derive(Debug)]
pub struct OAuthSession {
    /// Current bearer token.
    pub access_token: String,
    /// Long-lived token used to mint new bearer tokens; empty for static
    /// (`env`) tokens, which are never refreshed.
    pub refresh_token: String,
    /// Bearer expiry as unix millis; `i64::MAX` for non-expiring static tokens.
    pub expires_at_ms: i64,
    /// Our credentials file to rewrite after refresh; `None` means never
    /// persist (env tokens and `Claude Code`'s own file).
    pub persist_path: Option<PathBuf>,
}

/// Shareable handle for concurrent completions.
pub type OAuthSessionHandle = Arc<RwLock<OAuthSession>>;

/// Current time as unix millis; `0` when the clock is unavailable.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .ok()
        .and_then(|millis| i64::try_from(millis).ok())
        .unwrap_or(0)
}

/// Whether the bearer token should be refreshed before use.
#[must_use]
pub const fn needs_refresh(expires_at_ms: i64, now: i64) -> bool {
    expires_at_ms.saturating_sub(now) < REFRESH_SKEW_MS
}

/// Read a poisoned lock's contents instead of failing.
fn unlocked<T>(result: Result<T, PoisonError<T>>) -> T {
    result.unwrap_or_else(PoisonError::into_inner)
}

/// Tokens as stored in our own credentials file.
#[derive(Debug, Serialize, Deserialize)]
struct StoredCredentials {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    expires_at_ms: i64,
    #[serde(default)]
    mode: Option<String>,
}

/// `Claude Code`'s `~/.claude/.credentials.json` (tolerant subset).
#[derive(Debug, Deserialize)]
struct ClaudeCodeFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<ClaudeCodeOAuth>,
}

/// `claudeAiOauth` object inside `Claude Code`'s credentials file.
#[derive(Debug, Deserialize)]
struct ClaudeCodeOAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at_ms: Option<i64>,
}

/// Loaded credentials plus where to persist refreshed tokens (`None` = don't).
#[derive(Debug)]
pub struct LoadedCredentials {
    /// Bearer token.
    pub access_token: String,
    /// Refresh token (empty when unavailable).
    pub refresh_token: String,
    /// Bearer expiry as unix millis.
    pub expires_at_ms: i64,
    /// File to rewrite after refresh.
    pub persist_path: Option<PathBuf>,
}

/// Path of our own credentials file (`~/.infinity/claude-oauth.json`).
#[must_use]
pub fn own_credentials_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".infinity").join("claude-oauth.json"))
}

/// Path of `Claude Code`'s credentials file (`claude auth login` target).
#[must_use]
pub fn claude_code_credentials_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join(".credentials.json"));
        }
    }
    dirs::home_dir().map(|home| home.join(".claude").join(".credentials.json"))
}

/// Load subscription credentials: our file first, then `Claude Code`'s login.
#[must_use]
pub fn load_credentials() -> Option<LoadedCredentials> {
    if let Some(path) = own_credentials_path() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(stored) = serde_json::from_str::<StoredCredentials>(&contents) {
                if !stored.access_token.trim().is_empty() {
                    return Some(LoadedCredentials {
                        access_token: stored.access_token,
                        refresh_token: stored.refresh_token,
                        expires_at_ms: stored.expires_at_ms,
                        persist_path: Some(path),
                    });
                }
            }
        }
    }
    if let Some(path) = claude_code_credentials_path() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(file) = serde_json::from_str::<ClaudeCodeFile>(&contents) {
                if let Some(oauth) = file.oauth {
                    if let Some(access) = oauth.access_token {
                        if !access.trim().is_empty() {
                            return Some(LoadedCredentials {
                                access_token: access,
                                refresh_token: oauth.refresh_token.unwrap_or_default(),
                                expires_at_ms: oauth.expires_at_ms.unwrap_or(0),
                                persist_path: None,
                            });
                        }
                    }
                }
            }
        }
    }
    None
}

/// Write our credentials file with owner-only permissions.
fn write_credentials_file(path: &PathBuf, creds: &StoredCredentials) -> Result<(), BoxError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(creds)?;
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Persist freshly exchanged login tokens; returns the file path.
///
/// # Errors
///
/// Returns an error when the home directory is unknown or the file cannot
/// be written.
pub fn store_login(
    mode: AccountMode,
    issued: &IssuedTokens,
    now: i64,
) -> Result<PathBuf, BoxError> {
    let Some(path) = own_credentials_path() else {
        return Err("could not determine home directory".into());
    };
    let mode_name = match mode {
        AccountMode::Max => "max".to_owned(),
        AccountMode::Console => "console".to_owned(),
    };
    let creds = StoredCredentials {
        access_token: issued.access_token.clone(),
        refresh_token: issued.refresh_token.clone().unwrap_or_default(),
        expires_at_ms: now.saturating_add(issued.expires_in_secs.saturating_mul(1000)),
        mode: Some(mode_name),
    };
    write_credentials_file(&path, &creds)?;
    Ok(path)
}

/// Persist refreshed tokens to our credentials file when configured.
fn persist(handle: &OAuthSession, now: i64, expires_in_secs: i64) {
    let Some(path) = handle.persist_path.clone() else {
        return;
    };
    let creds = StoredCredentials {
        access_token: handle.access_token.clone(),
        refresh_token: handle.refresh_token.clone(),
        expires_at_ms: now.saturating_add(expires_in_secs.saturating_mul(1000)),
        mode: None,
    };
    if let Err(e) = write_credentials_file(&path, &creds) {
        tracing::warn!("oauth: failed to persist refreshed tokens: {e}");
    }
}

/// Exchange a refresh token for new tokens.
async fn refresh_tokens(client: &Client, refresh_token: &str) -> Result<IssuedTokens, OAuthError> {
    #[derive(Serialize)]
    struct RefreshRequest<'a> {
        grant_type: &'a str,
        refresh_token: &'a str,
        client_id: &'a str,
        scope: &'a str,
    }
    #[derive(Deserialize)]
    struct RefreshResponse {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        expires_in: i64,
    }
    let resp = client
        .post(TOKEN_URL)
        .timeout(TOKEN_TIMEOUT)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/plain, */*")
        .header("User-Agent", TOKEN_USER_AGENT)
        .json(&RefreshRequest {
            grant_type: "refresh_token",
            refresh_token,
            client_id: CLIENT_ID,
            scope: REFRESH_SCOPE,
        })
        .send()
        .await
        .map_err(|e| OAuthError {
            message: format!("oauth refresh request failed: {e}"),
            status: None,
            retryable: true,
        })?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(OAuthError {
            message: format!("oauth refresh failed: HTTP {status}: {}", truncate(&text)),
            status: Some(status),
            retryable: status >= 500,
        });
    }
    let body: RefreshResponse = resp.json().await.map_err(|e| OAuthError {
        message: format!("oauth refresh returned invalid JSON: {e}"),
        status: None,
        retryable: true,
    })?;
    Ok(IssuedTokens {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        expires_in_secs: body.expires_in,
    })
}

fn truncate(text: &str) -> String {
    text.trim().chars().take(300).collect()
}

/// Ensure the session holds a fresh bearer token, refreshing when needed.
///
/// Static (refresh-less) sessions return their token as-is.
///
/// # Errors
///
/// Returns [`OAuthError`] when the session has no usable token or the
/// refresh endpoint rejects the request.
pub async fn ensure_fresh_token(
    client: &Client,
    handle: &OAuthSessionHandle,
) -> Result<String, OAuthError> {
    let refresh_token = {
        let session = unlocked(handle.read());
        if session.access_token.trim().is_empty() {
            return Err(OAuthError {
                message: "oauth session has no access token; sign in again".to_owned(),
                status: None,
                retryable: false,
            });
        }
        if session.refresh_token.trim().is_empty()
            || !needs_refresh(session.expires_at_ms, now_ms())
        {
            return Ok(session.access_token.clone());
        }
        session.refresh_token.clone()
    };
    store_refreshed(client, handle, &refresh_token).await
}

/// Refresh unconditionally and store the result (used after a `401`).
///
/// # Errors
///
/// Returns [`OAuthError`] when no refresh token is available or the refresh
/// endpoint rejects the request.
pub async fn force_refresh(
    client: &Client,
    handle: &OAuthSessionHandle,
) -> Result<String, OAuthError> {
    let refresh_token = {
        let session = unlocked(handle.read());
        if session.refresh_token.trim().is_empty() {
            return Err(OAuthError {
                message: "oauth token rejected and no refresh token available".to_owned(),
                status: None,
                retryable: false,
            });
        }
        session.refresh_token.clone()
    };
    store_refreshed(client, handle, &refresh_token).await
}

async fn store_refreshed(
    client: &Client,
    handle: &OAuthSessionHandle,
    refresh_token: &str,
) -> Result<String, OAuthError> {
    let issued = refresh_tokens(client, refresh_token).await?;
    let now = now_ms();
    let mut session = unlocked(handle.write());
    // Another task may have refreshed while the network call was in flight.
    if !needs_refresh(session.expires_at_ms, now) && !session.access_token.trim().is_empty() {
        return Ok(session.access_token.clone());
    }
    session.access_token.clone_from(&issued.access_token);
    if let Some(rotated) = issued.refresh_token {
        session.refresh_token = rotated;
    }
    session.expires_at_ms = now.saturating_add(issued.expires_in_secs.saturating_mul(1000));
    persist(&session, now, issued.expires_in_secs);
    Ok(session.access_token.clone())
}

/// PKCE pair for the authorize redirect.
#[derive(Debug)]
pub struct Pkce {
    /// Secret sent at code exchange.
    pub verifier: String,
    /// `SHA-256` challenge sent in the authorize URL.
    pub challenge: String,
}

/// Generate a PKCE pair (64 random bytes, `S256`).
fn generate_pkce() -> Result<Pkce, BoxError> {
    let mut random = [0_u8; 64];
    getrandom::fill(&mut random).map_err(|e| format!("oauth: randomness unavailable: {e}"))?;
    let verifier = URL_SAFE_NO_PAD.encode(random);
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    Ok(Pkce {
        verifier,
        challenge: URL_SAFE_NO_PAD.encode(digest),
    })
}

/// Random `state` (32 hex chars) binding the callback to this sign-in.
fn generate_state() -> Result<String, BoxError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|e| format!("oauth: randomness unavailable: {e}"))?;
    Ok(random
        .iter()
        .fold(String::with_capacity(32), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        }))
}

/// Authorize redirect for one sign-in attempt.
#[derive(Debug)]
pub struct Authorization {
    /// URL to open in a browser.
    pub url: String,
    /// Expected `state` in the pasted callback.
    pub state: String,
    /// Secret for the code exchange.
    pub verifier: String,
}

/// Subscription account type selecting the authorize endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountMode {
    /// `Max` / `Pro` subscription via `claude.com`.
    Max,
    /// `Console` billing via `platform.claude.com`.
    Console,
}

impl AccountMode {
    #[must_use]
    pub const fn authorize_url(&self) -> &'static str {
        match self {
            Self::Max => AUTHORIZE_URL_MAX,
            Self::Console => AUTHORIZE_URL_CONSOLE,
        }
    }
}

impl std::str::FromStr for AccountMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "max" => Ok(Self::Max),
            "console" => Ok(Self::Console),
            other => Err(format!(
                "unknown account mode: {other} (expected max|console)"
            )),
        }
    }
}

/// Build the authorize URL, PKCE pair, and `state` for a sign-in attempt.
///
/// # Errors
///
/// Returns an error when OS randomness is unavailable for PKCE/`state`.
pub fn authorize(mode: AccountMode) -> Result<Authorization, BoxError> {
    let pkce = generate_pkce()?;
    let state = generate_state()?;
    let scopes = OAUTH_SCOPES.replace(' ', "%20");
    let url = format!(
        "{}?code=true&client_id={CLIENT_ID}&response_type=code&redirect_uri={CODE_CALLBACK_URL}\
         &scope={scopes}&code_challenge={}&code_challenge_method=S256&state={state}",
        mode.authorize_url(),
        pkce.challenge,
    );
    Ok(Authorization {
        url,
        state,
        verifier: pkce.verifier,
    })
}

/// Pasted callback: full redirect URL, `code#state`, or `code=&state=` pairs.
#[derive(Debug, PartialEq, Eq)]
pub struct Callback {
    /// Authorization code.
    pub code: String,
    /// Round-tripped `state`.
    pub state: String,
}

/// Parse pasted callback input, rejecting blank fields.
pub fn parse_callback(input: &str) -> Option<Callback> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Full redirect URL: take the query string.
    let query = trimmed.split_once('?').map_or(trimmed, |(_, query)| {
        query.split('#').next().unwrap_or(query)
    });
    if let Some(found) = from_pairs(query) {
        return Some(found);
    }
    // Legacy `code#state` shape.
    if let Some((code, state)) = trimmed.split_once('#') {
        let code = code.trim().to_owned();
        let state = state.trim().to_owned();
        if !code.is_empty() && !state.is_empty() && !code.contains(char::is_whitespace) {
            return Some(Callback { code, state });
        }
    }
    None
}

fn from_pairs(query: &str) -> Option<Callback> {
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key.trim() {
            "code" => code = Some(value.trim().to_owned()),
            "state" => state = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    match (code, state) {
        (Some(code), Some(state)) if !code.is_empty() && !state.is_empty() => {
            Some(Callback { code, state })
        }
        _ => None,
    }
}

/// Exchange a verified callback for tokens.
///
/// Returns `None` when the endpoint rejects the code.
///
/// # Errors
///
/// Returns [`OAuthError`] when the token endpoint is unreachable or answers
/// with undecodable JSON.
pub async fn exchange_code(
    client: &Client,
    callback: &Callback,
    verifier: &str,
) -> Result<Option<IssuedTokens>, OAuthError> {
    #[derive(Serialize)]
    struct ExchangeRequest<'a> {
        code: &'a str,
        state: &'a str,
        grant_type: &'a str,
        client_id: &'a str,
        redirect_uri: &'a str,
        code_verifier: &'a str,
    }
    #[derive(Deserialize)]
    struct ExchangeResponse {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        expires_in: i64,
    }
    let resp = client
        .post(TOKEN_URL)
        .timeout(TOKEN_TIMEOUT)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/plain, */*")
        .header("User-Agent", TOKEN_USER_AGENT)
        .json(&ExchangeRequest {
            code: &callback.code,
            state: &callback.state,
            grant_type: "authorization_code",
            client_id: CLIENT_ID,
            redirect_uri: CODE_CALLBACK_URL,
            code_verifier: verifier,
        })
        .send()
        .await
        .map_err(|e| OAuthError {
            message: format!("oauth exchange request failed: {e}"),
            status: None,
            retryable: true,
        })?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status >= 500 {
            return Err(OAuthError {
                message: format!("oauth exchange failed: HTTP {status}: {}", truncate(&text)),
                status: Some(status),
                retryable: true,
            });
        }
        return Ok(None);
    }
    let body: ExchangeResponse = resp.json().await.map_err(|e| OAuthError {
        message: format!("oauth exchange returned invalid JSON: {e}"),
        status: None,
        retryable: true,
    })?;
    Ok(Some(IssuedTokens {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        expires_in_secs: body.expires_in,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_callback_url() {
        let callback = parse_callback(
            "https://platform.claude.com/oauth/code/callback?code=abc123&state=xyz789",
        );
        assert_eq!(
            callback,
            Some(Callback {
                code: "abc123".to_owned(),
                state: "xyz789".to_owned(),
            })
        );
    }

    #[test]
    fn parses_query_pairs_without_url() {
        let callback = parse_callback("code=abc123&state=xyz789");
        assert_eq!(
            callback,
            Some(Callback {
                code: "abc123".to_owned(),
                state: "xyz789".to_owned(),
            })
        );
    }

    #[test]
    fn parses_legacy_hash_shape() {
        let callback = parse_callback("abc123#xyz789");
        assert_eq!(
            callback,
            Some(Callback {
                code: "abc123".to_owned(),
                state: "xyz789".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_blank_and_partial_input() {
        assert!(parse_callback("").is_none());
        assert!(parse_callback("   ").is_none());
        assert!(parse_callback("code=only").is_none());
        assert!(parse_callback("abc123#").is_none());
        assert!(parse_callback("#xyz789").is_none());
    }

    #[test]
    fn refresh_skew_applies() {
        let now = 1_000_000;
        assert!(needs_refresh(now, now));
        assert!(needs_refresh(now.saturating_add(60_000), now));
        assert!(!needs_refresh(
            now.saturating_add(REFRESH_SKEW_MS).saturating_add(1),
            now
        ));
        assert!(needs_refresh(0, now));
    }

    #[test]
    fn authorize_url_carries_oauth_params() {
        let auth = authorize(AccountMode::Max).unwrap();
        assert!(auth.url.starts_with(AUTHORIZE_URL_MAX));
        assert!(auth.url.contains(CLIENT_ID));
        assert!(auth.url.contains("code_challenge_method=S256"));
        assert!(auth.url.contains("code_challenge="));
        assert!(!auth.verifier.is_empty());
        assert!(!auth.state.is_empty());
    }
}
