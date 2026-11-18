//! Errors the UI turns into plain-language screens (see PLAN.md §2, "Errors to show clearly").

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum RdError {
    /// 401 from the token endpoint: the client ID is wrong, or the app is not an "installed app"
    /// (a "web app" or "script" needs a secret we never have).
    InvalidClient,
    /// `invalid_grant`: the sign-in code expired / was used, or the refresh token was revoked.
    InvalidGrant,
    /// `redirect_uri` does not match the one registered on the user's Reddit app.
    RedirectMismatch,
    /// The token was rejected by oauth.reddit.com even after one refresh.
    Unauthorized,
    /// 403 on an API call: private/quarantined/banned subreddit, or a missing scope.
    Forbidden(String),
    /// 404: no such subreddit / post / user.
    NotFound(String),
    /// 429 or the rate budget is spent; seconds until the window resets.
    RateLimited { reset_secs: u64 },
    /// Any other HTTP status.
    Http { status: u16, body: String },
    /// Network failure (DNS, TLS, timeout).
    Network(String),
    /// The response was not the JSON shape we expected.
    Parse(String),
}

impl fmt::Display for RdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RdError::InvalidClient => write!(
                f,
                "Reddit rejected the client ID. Check that it was copied correctly and that the app \
                 type is \"installed app\" (not \"web app\" or \"script\")."
            ),
            RdError::InvalidGrant => write!(f, "The sign-in expired or was revoked. Please sign in again."),
            RdError::RedirectMismatch => write!(
                f,
                "The redirect URI on your Reddit app does not match. Set it to {}.",
                crate::DEFAULT_REDIRECT_URI
            ),
            RdError::Unauthorized => write!(f, "Reddit did not accept the access token."),
            RdError::Forbidden(p) => write!(f, "Not allowed: {p} (private, quarantined or missing permission)."),
            RdError::NotFound(p) => write!(f, "Not found: {p}"),
            RdError::RateLimited { reset_secs } => write!(f, "Rate limited; try again in {reset_secs} s."),
            RdError::Http { status, body } => write!(f, "HTTP {status}: {}", truncate(body, 200)),
            RdError::Network(e) => write!(f, "Network error: {e}"),
            RdError::Parse(e) => write!(f, "Unexpected response: {e}"),
        }
    }
}

impl std::error::Error for RdError {}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

impl From<reqwest::Error> for RdError {
    fn from(e: reqwest::Error) -> Self {
        RdError::Network(e.to_string())
    }
}
