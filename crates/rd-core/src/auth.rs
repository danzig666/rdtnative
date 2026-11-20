//! Reddit OAuth for an "installed app" (public client, no secret):
//! - app-only tokens (`installed_client` grant) for browsing without an account;
//! - the authorization-code flow (sign in on a phone) with permanent refresh tokens.
//!
//! Request building and response parsing are pure functions so they can be unit-tested; `post_token`
//! is the only part that touches the network.

use crate::{enc, form, now_secs, RdError, WWW};
use serde_json::Value;

/// Scopes rdtnative asks for at sign-in. No mod scopes, no `account` (settings changes).
pub const SCOPES: &[&str] =
    &["identity", "read", "mysubreddits", "subscribe", "vote", "save", "history", "submit", "edit", "privatemessages", "report"];

/// A Reddit client ID as the user typed or pasted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientId(String);

impl ClientId {
    /// Trims whitespace (pasting often adds some) and checks the shape: Reddit IDs are
    /// 14–30 characters of `[A-Za-z0-9_-]`. A wrong-but-well-formed ID is only caught by the server.
    pub fn parse(raw: &str) -> Result<ClientId, String> {
        let s = raw.trim();
        if !(14..=30).contains(&s.len()) {
            return Err(format!("a client ID is 14-30 characters long, this one has {}", s.len()));
        }
        if let Some(c) = s.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-')) {
            return Err(format!("a client ID cannot contain {c:?}"));
        }
        Ok(ClientId(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `Authorization: Basic base64("<id>:")`: installed apps have an empty secret.
    pub fn basic_auth(&self) -> String {
        format!("Basic {}", base64(format!("{}:", self.0).as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub access: String,
    /// Unix seconds after which the access token is no longer valid.
    pub expires_at: u64,
    /// Present only for user sign-ins with `duration=permanent`.
    pub refresh: Option<String>,
    pub scope: String,
}

impl Token {
    /// True when the token expires within `skew` seconds (refresh a little early).
    pub fn expires_within(&self, skew: u64, now: u64) -> bool {
        now + skew >= self.expires_at
    }
}

/// The three grants we use.
#[derive(Debug, Clone)]
pub enum Grant<'a> {
    /// Browsing without an account. `device_id` is 20–30 characters, stable per install.
    AppOnly { device_id: &'a str },
    Code { code: &'a str, redirect_uri: &'a str },
    Refresh { refresh_token: &'a str },
}

impl Grant<'_> {
    pub fn body(&self) -> String {
        match self {
            Grant::AppOnly { device_id } => {
                form(&[("grant_type", "https://oauth.reddit.com/grants/installed_client"), ("device_id", device_id)])
            }
            Grant::Code { code, redirect_uri } => {
                form(&[("grant_type", "authorization_code"), ("code", code), ("redirect_uri", redirect_uri)])
            }
            Grant::Refresh { refresh_token } => form(&[("grant_type", "refresh_token"), ("refresh_token", refresh_token)]),
        }
    }
}

/// A new per-install device ID for app-only tokens (Reddit wants 20–30 characters).
pub fn new_device_id() -> String {
    crate::random_token(30)
}

/// The page the user opens on their phone to approve rdtnative. `state` comes back unchanged on the
/// redirect; the TV puts a nonce and its LAN address in it (see `State`).
pub fn authorize_url(client: &ClientId, redirect_uri: &str, state: &str) -> String {
    format!(
        "{WWW}/api/v1/authorize.compact?client_id={}&response_type=code&state={}&redirect_uri={}&duration=permanent&scope={}",
        enc(client.as_str()),
        enc(state),
        enc(redirect_uri),
        enc(&SCOPES.join(" "))
    )
}

/// The `state` round-tripped through Reddit: `<nonce>.<tv host:port>` (the host part is optional;
/// without it the callback page just shows the code for manual entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub nonce: String,
    pub tv: Option<String>,
}

impl State {
    pub fn encode(&self) -> String {
        match &self.tv {
            Some(tv) => format!("{}.{}", self.nonce, tv),
            None => self.nonce.clone(),
        }
    }

    pub fn decode(s: &str) -> State {
        match s.split_once('.') {
            Some((n, tv)) if !tv.is_empty() => State { nonce: n.to_string(), tv: Some(tv.to_string()) },
            _ => State { nonce: s.trim_end_matches('.').to_string(), tv: None },
        }
    }
}

/// Result of the redirect back from Reddit: `?state=..&code=..` or `?state=..&error=access_denied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    Code { code: String, state: String },
    Denied { error: String, state: String },
}

/// The callback address inside a longer paste, such as Chrome's console line "Failed to launch
/// 'myapp://cb?state=..&code=..' because the scheme does not have a registered handler.": the
/// first word (split at spaces and quotes) that carries `code=` or `error=`.
fn callback_in(s: &str) -> &str {
    s.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '<' | '>' | '‘' | '’' | '“' | '”'))
        .find(|w| w.contains("code=") || w.contains("error="))
        .unwrap_or(s)
}

/// Parses a pasted callback URL (or just its query string, or a bare code, or a line of text
/// with the URL in it).
pub fn parse_callback(input: &str) -> Result<Callback, String> {
    let s = callback_in(input.trim());
    let query = match s.split_once('?') {
        Some((_, q)) => q,
        None if s.contains('=') => s,
        None if !s.is_empty() => return Ok(Callback::Code { code: s.to_string(), state: String::new() }),
        None => return Err("empty input".into()),
    };
    let query = query.split('#').next().unwrap_or("");
    let mut code = None;
    let mut state = String::new();
    let mut error = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode_component(v);
        match k {
            "code" => code = Some(v),
            "state" => state = v,
            "error" => error = Some(v),
            _ => {}
        }
    }
    match (code, error) {
        (_, Some(error)) => Ok(Callback::Denied { error, state }),
        (Some(code), None) => Ok(Callback::Code { code, state }),
        (None, None) => Err("no code= in the URL".into()),
    }
}

fn decode_component(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Turns a token-endpoint response into a `Token` or a precise error.
/// `prev_refresh` is kept when a refresh response does not repeat the refresh token.
pub fn parse_token_response(status: u16, body: &str, now: u64, prev_refresh: Option<&str>) -> Result<Token, RdError> {
    if status == 401 {
        return Err(RdError::InvalidClient);
    }
    let v: Value = serde_json::from_str(body)
        .map_err(|_| RdError::Http { status, body: body.to_string() })?;
    if let Some(err) = v["error"].as_str().or_else(|| v["error"].as_u64().map(|_| "http")) {
        return Err(match err {
            "invalid_grant" => RdError::InvalidGrant,
            "redirect_uri_mismatch" => RdError::RedirectMismatch,
            "unauthorized_client" | "invalid_client" => RdError::InvalidClient,
            "unsupported_grant_type" => RdError::InvalidClient,
            _ => RdError::Http { status, body: body.to_string() },
        });
    }
    if status != 200 {
        return Err(RdError::Http { status, body: body.to_string() });
    }
    let access = v["access_token"].as_str().ok_or_else(|| RdError::Parse("no access_token".into()))?;
    let expires_in = v["expires_in"].as_u64().unwrap_or(3600);
    Ok(Token {
        access: access.to_string(),
        expires_at: now + expires_in,
        refresh: v["refresh_token"].as_str().map(str::to_string).or_else(|| prev_refresh.map(str::to_string)),
        scope: v["scope"].as_str().unwrap_or_default().to_string(),
    })
}

/// POSTs a grant to `https://www.reddit.com/api/v1/access_token`.
pub fn post_token(http: &reqwest::blocking::Client, ua: &str, client: &ClientId, grant: &Grant) -> Result<Token, RdError> {
    let resp = http
        .post(format!("{WWW}/api/v1/access_token"))
        .header("User-Agent", ua)
        .header("Authorization", client.basic_auth())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(grant.body())
        .send()?;
    let status = resp.status().as_u16();
    let body = resp.text()?;
    let prev = match grant {
        Grant::Refresh { refresh_token } => Some(*refresh_token),
        _ => None,
    };
    parse_token_response(status, &body, now_secs(), prev)
}

/// Revokes a refresh token on sign-out (best effort).
pub fn revoke(http: &reqwest::blocking::Client, ua: &str, client: &ClientId, refresh_token: &str) -> Result<(), RdError> {
    let resp = http
        .post(format!("{WWW}/api/v1/revoke_token"))
        .header("User-Agent", ua)
        .header("Authorization", client.basic_auth())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form(&[("token", refresh_token), ("token_type_hint", "refresh_token")]))
        .send()?;
    match resp.status().as_u16() {
        200 | 204 => Ok(()),
        s => Err(RdError::Http { status: s, body: resp.text().unwrap_or_default() }),
    }
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_is_trimmed_and_validated() {
        assert_eq!(ClientId::parse("  abcDEF123_-xyz12 \n").unwrap().as_str(), "abcDEF123_-xyz12");
        assert!(ClientId::parse("short").is_err());
        assert!(ClientId::parse("has a space in it!!").is_err());
        assert!(ClientId::parse(&"x".repeat(31)).is_err());
    }

    #[test]
    fn basic_auth_has_an_empty_secret() {
        let c = ClientId::parse("p-jcoLKBynTLew").unwrap();
        // base64("p-jcoLKBynTLew:")
        assert_eq!(c.basic_auth(), "Basic cC1qY29MS0J5blRMZXc6");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    #[test]
    fn grant_bodies() {
        assert_eq!(
            Grant::AppOnly { device_id: "dev1234567890123456789" }.body(),
            "grant_type=https%3A%2F%2Foauth.reddit.com%2Fgrants%2Finstalled_client&device_id=dev1234567890123456789"
        );
        assert_eq!(
            Grant::Code { code: "c0de", redirect_uri: "https://x.io/cb" }.body(),
            "grant_type=authorization_code&code=c0de&redirect_uri=https%3A%2F%2Fx.io%2Fcb"
        );
        assert_eq!(Grant::Refresh { refresh_token: "r1" }.body(), "grant_type=refresh_token&refresh_token=r1");
        let d = new_device_id();
        assert!((20..=30).contains(&d.len()));
    }

    #[test]
    fn authorize_url_carries_everything() {
        let c = ClientId::parse("abcdefghijklmn").unwrap();
        let u = authorize_url(&c, crate::DEFAULT_REDIRECT_URI, "n0nce.192.168.1.20:8091");
        assert!(u.starts_with("https://www.reddit.com/api/v1/authorize.compact?client_id=abcdefghijklmn&response_type=code"));
        assert!(u.contains("&state=n0nce.192.168.1.20%3A8091&"));
        assert!(u.contains("&redirect_uri=https%3A%2F%2Fdanzig666.github.io%2Frdtnative%2Fcallback&"));
        assert!(u.contains("&duration=permanent&scope=identity%20read%20"));
    }

    #[test]
    fn state_round_trips() {
        let s = State { nonce: "abc".into(), tv: Some("192.168.1.20:8091".into()) };
        assert_eq!(s.encode(), "abc.192.168.1.20:8091");
        assert_eq!(State::decode(&s.encode()), s);
        assert_eq!(State::decode("abc"), State { nonce: "abc".into(), tv: None });
    }

    #[test]
    fn callback_parsing() {
        assert_eq!(
            parse_callback("https://danzig666.github.io/rdtnative/callback?state=ab.1.2.3.4%3A8091&code=XyZ-1_2#_").unwrap(),
            Callback::Code { code: "XyZ-1_2".into(), state: "ab.1.2.3.4:8091".into() }
        );
        assert_eq!(
            parse_callback("state=s&error=access_denied").unwrap(),
            Callback::Denied { error: "access_denied".into(), state: "s".into() }
        );
        assert_eq!(parse_callback(" bareCode ").unwrap(), Callback::Code { code: "bareCode".into(), state: "".into() });
        assert!(parse_callback("https://x/cb?state=s").is_err());
        // Chrome's console line, copied whole (and its "Copy link address" form).
        let line = "Failed to launch 'myapp://oauth?state=n1.192.168.0.5%3A8091&code=Ab-c_9' because the scheme does not have a registered handler.";
        let want = Callback::Code { code: "Ab-c_9".into(), state: "n1.192.168.0.5:8091".into() };
        assert_eq!(parse_callback(line).unwrap(), want);
        assert_eq!(parse_callback("myapp://oauth?state=n1.192.168.0.5%3A8091&code=Ab-c_9").unwrap(), want);
        assert_eq!(decode_component("a%2Fb+c%"), "a/b c%");
    }

    #[test]
    fn token_responses() {
        let ok = r#"{"access_token":"AT","token_type":"bearer","expires_in":86400,"refresh_token":"RT","scope":"read identity"}"#;
        let t = parse_token_response(200, ok, 100, None).unwrap();
        assert_eq!(t, Token { access: "AT".into(), expires_at: 86500, refresh: Some("RT".into()), scope: "read identity".into() });
        assert!(t.expires_within(60, 86450));
        assert!(!t.expires_within(60, 86000));

        let refreshed = r#"{"access_token":"AT2","expires_in":3600,"scope":"*"}"#;
        assert_eq!(parse_token_response(200, refreshed, 0, Some("RT")).unwrap().refresh.as_deref(), Some("RT"));

        assert_eq!(parse_token_response(401, r#"{"message":"Unauthorized","error":401}"#, 0, None), Err(RdError::InvalidClient));
        assert_eq!(parse_token_response(200, r#"{"error":"invalid_grant"}"#, 0, None), Err(RdError::InvalidGrant));
        assert_eq!(parse_token_response(400, r#"{"error":"redirect_uri_mismatch"}"#, 0, None), Err(RdError::RedirectMismatch));
        assert!(matches!(parse_token_response(503, "<html>", 0, None), Err(RdError::Http { status: 503, .. })));
    }
}
