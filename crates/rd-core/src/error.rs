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
    /// 403 on an API call: a private/quarantined/banned subreddit or a missing scope, or (when it
    /// happens on public pages) Reddit refusing the app's client ID or user agent. `reason` is what
    /// Reddit's reply said, if anything.
    Forbidden { what: String, reason: Option<String> },
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
                "Reddit rejected the client ID. Check that it was entered correctly."
            ),
            RdError::InvalidGrant => write!(f, "The sign-in expired or was revoked. Please sign in again."),
            RdError::RedirectMismatch => write!(
                f,
                "The redirect URI on your Reddit app does not match. Set it to {}.",
                crate::DEFAULT_REDIRECT_URI
            ),
            RdError::Unauthorized => write!(f, "Reddit did not accept the access token."),
            RdError::Forbidden { what, reason } => {
                write!(f, "Reddit refused {what} (HTTP 403")?;
                if let Some(r) = reason {
                    write!(f, ": {r}")?;
                }
                if reason.as_deref() == Some(BLOCKED) {
                    write!(f, "). Reddit's bot filter blocked the request; try a different user agent in Reddit API settings.")
                } else if what.starts_with("/r/") || what.starts_with("/user/") {
                    write!(f, "). The subreddit or user may be private, quarantined or banned.")
                } else {
                    write!(f, "). This is a public page, so Reddit is refusing this app: check the client ID and user agent in Reddit API settings.")
                }
            }
            RdError::NotFound(p) => write!(f, "Not found: {p}"),
            RdError::RateLimited { reset_secs } => write!(f, "Rate limited; try again in {reset_secs} s."),
            RdError::Http { status, body } => write!(f, "HTTP {status}: {}", truncate(body, 200)),
            RdError::Network(e) => write!(f, "Network error: {e}"),
            RdError::Parse(e) => write!(f, "Unexpected response: {e}"),
        }
    }
}

impl std::error::Error for RdError {}

const BLOCKED: &str = "blocked by Reddit's network security";

/// Reddit's explanation in a 403 body: the JSON `reason`/`message`/`explanation`, or a note that
/// it is the HTML "blocked" page Reddit serves to refused clients.
pub fn forbidden_reason(body: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let pick = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let mut parts: Vec<String> = Vec::new();
        for k in ["reason", "explanation", "message"] {
            if let Some(p) = pick(k) {
                if !parts.iter().any(|q| q.eq_ignore_ascii_case(&p)) {
                    parts.push(p);
                }
            }
        }
        return (!parts.is_empty()).then(|| truncate(&parts.join(", "), 160).to_string());
    }
    let lower = body.to_ascii_lowercase();
    if lower.contains("blocked") {
        return Some(BLOCKED.into());
    }
    None
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_reason_reads_reddit_bodies() {
        assert_eq!(forbidden_reason(r#"{"message": "Forbidden", "error": 403}"#).as_deref(), Some("Forbidden"));
        assert_eq!(
            forbidden_reason(r#"{"reason": "private", "message": "Forbidden", "error": 403}"#).as_deref(),
            Some("private, Forbidden")
        );
        assert_eq!(forbidden_reason("<html><body>You've been blocked by network security.</body></html>").as_deref(), Some("blocked by Reddit's network security"));
        assert_eq!(forbidden_reason(""), None);
    }

    #[test]
    fn forbidden_on_public_pages_blames_the_app() {
        let e = RdError::Forbidden { what: "/hot".into(), reason: Some("Forbidden".into()) }.to_string();
        assert!(e.contains("Reddit refused /hot (HTTP 403: Forbidden)"), "{e}");
        assert!(e.contains("client ID"), "{e}");
        let e = RdError::Forbidden { what: "/hot".into(), reason: Some(BLOCKED.into()) }.to_string();
        assert!(e.contains("bot filter") && e.contains("user agent"), "{e}");
        let e = RdError::Forbidden { what: "/r/secret/hot".into(), reason: None }.to_string();
        assert!(e.contains("private, quarantined or banned"), "{e}");
    }
}
