//! One Reddit session: a client ID plus either an app-only identity (browse without an account) or
//! a signed-in user (refresh token). It keeps the access token fresh, sends the required headers,
//! tracks the rate-limit window and maps HTTP failures to `RdError`.

use crate::auth::{self, ClientId, Grant, Token};
use crate::ratelimit::{should_wait, Priority, RateLimit};
use crate::{now_secs, RdError, OAUTH};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// Refresh this many seconds before the access token expires.
const EXPIRY_SKEW: u64 = 120;

/// Requests in a row that a fresh token could not get through (401 twice each) before a signed-in
/// session counts its sign-in as gone.
const UNAUTHORIZED_LIMIT: u32 = 3;

/// What is stored per account on disk (the TV keeps one per account; the spike keeps one file).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Saved {
    pub client_id: String,
    /// The redirect URI registered on the user's Reddit app (`None` = rdtnative's callback page).
    #[serde(default)]
    pub redirect_uri: Option<String>,
    /// User-Agent override (`None` = rdtnative's default).
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Stable per install; used for app-only tokens.
    pub device_id: String,
    /// Present once the user has signed in.
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub expires_at: u64,
}

impl Saved {
    /// A fresh app-only identity for `cfg` (new device ID, no tokens).
    pub fn from_config(cfg: &crate::config::ApiConfig) -> Saved {
        Saved {
            client_id: cfg.client_id.clone(),
            redirect_uri: Some(cfg.redirect_uri.clone()),
            user_agent: cfg.user_agent.clone(),
            device_id: crate::auth::new_device_id(),
            ..Saved::default()
        }
    }

    /// The redirect URI to use for sign-in.
    pub fn redirect(&self) -> &str {
        self.redirect_uri.as_deref().filter(|r| !r.is_empty()).unwrap_or(crate::DEFAULT_REDIRECT_URI)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Identity {
    AppOnly { device_id: String },
    User { refresh_token: String },
}

pub struct Session {
    http: reqwest::blocking::Client,
    client: ClientId,
    identity: Identity,
    platform: String,
    ua_override: Option<String>,
    redirect_uri: Option<String>,
    username: Mutex<Option<String>>,
    token: Mutex<Option<Token>>,
    rate: Mutex<Option<RateLimit>>,
    /// Reddit no longer takes this user's sign-in (see [`Session::take_auth_lost`]).
    auth_lost: AtomicBool,
    unauthorized: AtomicU32,
}

/// A JSON response with the rate-limit snapshot that came with it.
#[derive(Debug, Clone)]
pub struct Reply {
    pub json: Value,
    pub rate: Option<RateLimit>,
    pub status: u16,
}

impl Session {
    pub fn new(http: reqwest::blocking::Client, client: ClientId, identity: Identity, platform: &str) -> Session {
        Session {
            http,
            client,
            identity,
            platform: platform.to_string(),
            ua_override: None,
            redirect_uri: None,
            username: Mutex::new(None),
            token: Mutex::new(None),
            rate: Mutex::new(None),
            auth_lost: AtomicBool::new(false),
            unauthorized: AtomicU32::new(0),
        }
    }

    /// Restores a session from disk; `None` if the saved client ID is malformed.
    pub fn from_saved(http: reqwest::blocking::Client, saved: &Saved, platform: &str) -> Option<Session> {
        let client = ClientId::parse(&saved.client_id).ok()?;
        let identity = match &saved.refresh_token {
            Some(rt) => Identity::User { refresh_token: rt.clone() },
            None => Identity::AppOnly { device_id: saved.device_id.clone() },
        };
        let mut s = Session::new(http, client, identity, platform);
        s.ua_override = saved.user_agent.clone().filter(|u| !u.is_empty());
        s.redirect_uri = saved.redirect_uri.clone().filter(|u| !u.is_empty());
        *s.username.lock().unwrap() = saved.username.clone();
        if let Some(at) = &saved.access_token {
            *s.token.lock().unwrap() = Some(Token {
                access: at.clone(),
                expires_at: saved.expires_at,
                refresh: saved.refresh_token.clone(),
                scope: String::new(),
            });
        }
        Some(s)
    }

    pub fn to_saved(&self, device_id: &str) -> Saved {
        let tok = self.token.lock().unwrap().clone();
        Saved {
            client_id: self.client.as_str().to_string(),
            redirect_uri: self.redirect_uri.clone(),
            user_agent: self.ua_override.clone(),
            device_id: device_id.to_string(),
            refresh_token: match &self.identity {
                Identity::User { refresh_token } => Some(refresh_token.clone()),
                Identity::AppOnly { .. } => None,
            },
            username: self.username.lock().unwrap().clone(),
            access_token: tok.as_ref().map(|t| t.access.clone()),
            expires_at: tok.map(|t| t.expires_at).unwrap_or(0),
        }
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn is_signed_in(&self) -> bool {
        matches!(self.identity, Identity::User { .. })
    }

    /// The override when the user set one, otherwise Reddit's recommended format.
    pub fn user_agent(&self) -> String {
        if let Some(u) = &self.ua_override {
            return u.clone();
        }
        crate::ua::user_agent(&self.platform, self.username.lock().unwrap().as_deref())
    }

    pub fn set_username(&self, name: Option<String>) {
        *self.username.lock().unwrap() = name;
    }

    pub fn last_rate(&self) -> Option<RateLimit> {
        *self.rate.lock().unwrap()
    }

    /// Whether Reddit stopped taking the signed-in user's sign-in (the refresh token was revoked
    /// or expired, or fresh tokens keep being refused), once: the app signs out and asks again.
    pub fn take_auth_lost(&self) -> bool {
        self.auth_lost.swap(false, Ordering::Relaxed)
    }

    /// Notes how a request went for [`Self::take_auth_lost`]; app-only sessions never lose one.
    fn note_auth<T>(&self, r: &Result<T, RdError>) {
        if !self.is_signed_in() {
            return;
        }
        match r {
            Ok(_) => self.unauthorized.store(0, Ordering::Relaxed),
            Err(RdError::InvalidGrant) => self.auth_lost.store(true, Ordering::Relaxed),
            Err(RdError::Unauthorized) => {
                if self.unauthorized.fetch_add(1, Ordering::Relaxed) + 1 >= UNAUTHORIZED_LIMIT {
                    self.unauthorized.store(0, Ordering::Relaxed);
                    self.auth_lost.store(true, Ordering::Relaxed);
                }
            }
            Err(_) => {}
        }
    }

    /// A valid access token, minting or refreshing one when needed.
    pub fn token(&self) -> Result<Token, RdError> {
        if let Some(t) = self.token.lock().unwrap().clone() {
            if !t.expires_within(EXPIRY_SKEW, now_secs()) {
                return Ok(t);
            }
        }
        self.renew()
    }

    /// Forces a new access token.
    pub fn renew(&self) -> Result<Token, RdError> {
        let grant = match &self.identity {
            Identity::AppOnly { device_id } => Grant::AppOnly { device_id },
            Identity::User { refresh_token } => Grant::Refresh { refresh_token },
        };
        let t = auth::post_token(&self.http, &self.user_agent(), &self.client, &grant)?;
        *self.token.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    /// GET `https://oauth.reddit.com{path}` with `raw_json=1` (unescaped `&amp;` in bodies/URLs).
    pub fn get(&self, path: &str, query: &[(&str, &str)], prio: Priority) -> Result<Reply, RdError> {
        let mut q: Vec<(&str, &str)> = query.to_vec();
        q.push(("raw_json", "1"));
        let url = format!("{OAUTH}{path}?{}", crate::form(&q));
        self.send(prio, path, |tok| self.http.get(&url).bearer_auth(tok))
    }

    /// POST a form to `https://oauth.reddit.com{path}` (votes, saves, replies).
    pub fn post(&self, path: &str, fields: &[(&str, &str)]) -> Result<Reply, RdError> {
        let url = format!("{OAUTH}{path}");
        let body = crate::form(fields);
        self.send(Priority::Action, path, |tok| {
            self.http
                .post(&url)
                .bearer_auth(tok)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body.clone())
        })
    }

    fn send(
        &self,
        prio: Priority,
        what: &str,
        build: impl Fn(&str) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Reply, RdError> {
        let r = self.send_retrying(prio, what, build);
        self.note_auth(&r);
        r
    }

    fn send_retrying(
        &self,
        prio: Priority,
        what: &str,
        build: impl Fn(&str) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Reply, RdError> {
        if let Some(wait) = should_wait(self.last_rate().as_ref(), prio, now_secs()) {
            return Err(RdError::RateLimited { reset_secs: wait });
        }
        let mut retried = false;
        loop {
            let tok = self.token()?;
            let resp = build(&tok.access).header("User-Agent", self.user_agent()).send()?;
            let status = resp.status().as_u16();
            let headers = resp.headers().clone();
            let rate = RateLimit::from_headers(
                |k| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string),
                now_secs(),
            );
            if rate.is_some() {
                *self.rate.lock().unwrap() = rate;
            }
            let body = resp.text()?;
            match status {
                200..=299 => {
                    let json = serde_json::from_str(&body).map_err(|e| RdError::Parse(format!("{what}: {e}")))?;
                    return Ok(Reply { json, rate, status });
                }
                401 if !retried => {
                    retried = true;
                    *self.token.lock().unwrap() = None;
                    continue;
                }
                401 => return Err(RdError::Unauthorized),
                // A signed-out token Reddit no longer likes answers 403: mint a new one, once.
                403 if !retried && matches!(self.identity, Identity::AppOnly { .. }) && crate::error::forbidden_reason(&body).as_deref() != Some("blocked by Reddit's network security") => {
                    retried = true;
                    *self.token.lock().unwrap() = None;
                    continue;
                }
                403 => {
                    return Err(RdError::Forbidden { what: what.to_string(), reason: crate::error::forbidden_reason(&body) })
                }
                404 => return Err(RdError::NotFound(what.to_string())),
                429 if !retried && prio != Priority::Action => {
                    let wait = headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .or(rate.map(|r| r.reset_secs))
                        .unwrap_or(2);
                    if wait <= 10 {
                        retried = true;
                        std::thread::sleep(std::time::Duration::from_secs(wait.max(1)));
                        continue;
                    }
                    return Err(RdError::RateLimited { reset_secs: wait });
                }
                429 => {
                    return Err(RdError::RateLimited { reset_secs: rate.map(|r| r.reset_secs).unwrap_or(60) })
                }
                _ => return Err(RdError::Http { status, body }),
            }
        }
    }

    /// `/api/v1/me` → username (signed-in sessions only).
    pub fn fetch_username(&self) -> Result<String, RdError> {
        let r = self.get("/api/v1/me", &[], Priority::Action)?;
        let name = r.json["name"].as_str().ok_or_else(|| RdError::Parse("/api/v1/me has no name".into()))?;
        self.set_username(Some(name.to_string()));
        Ok(name.to_string())
    }
}

/// The HTTP client every session shares (gzip, 20 s timeout, like ytnative's `net.rs`).
pub fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .gzip(true)
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_round_trip_keeps_identity_and_token() {
        let saved = Saved {
            client_id: "abcdefghijklmn".into(),
            redirect_uri: None,
            user_agent: None,
            device_id: "d".repeat(30),
            refresh_token: Some("RT".into()),
            username: Some("alice".into()),
            access_token: Some("AT".into()),
            expires_at: 99,
        };
        let s = Session::from_saved(http_client(), &saved, "linux").unwrap();
        assert!(s.is_signed_in());
        assert!(s.user_agent().ends_with("(by /u/alice)"));
        assert_eq!(s.to_saved(&saved.device_id), saved);

        let anon = Saved { refresh_token: None, username: None, access_token: None, expires_at: 0, ..saved.clone() };
        let s = Session::from_saved(http_client(), &anon, "linux").unwrap();
        assert_eq!(s.identity(), &Identity::AppOnly { device_id: "d".repeat(30) });
        assert!(Session::from_saved(http_client(), &Saved { client_id: "bad id".into(), ..anon.clone() }, "linux").is_none());

        let custom = Saved { user_agent: Some("webos:x.y:v9 (by /u/me)".into()), redirect_uri: Some("http://localhost/cb".into()), ..anon };
        let s = Session::from_saved(http_client(), &custom, "linux").unwrap();
        assert_eq!(s.user_agent(), "webos:x.y:v9 (by /u/me)");
        assert_eq!(s.to_saved(&custom.device_id), custom);
    }

    #[test]
    fn a_refused_sign_in_is_noticed_once() {
        let user = Saved { client_id: "abcdefghijklmn".into(), device_id: "d".repeat(30), refresh_token: Some("RT".into()), ..Saved::default() };
        let s = Session::from_saved(http_client(), &user, "linux").unwrap();
        let unauthorized: Result<(), RdError> = Err(RdError::Unauthorized);
        // A revoked refresh token: at once.
        s.note_auth(&Err::<(), _>(RdError::InvalidGrant));
        assert!(s.take_auth_lost());
        assert!(!s.take_auth_lost(), "reported once");
        // Refused tokens: only when it keeps happening; a success in between starts over.
        s.note_auth(&unauthorized);
        s.note_auth(&unauthorized);
        s.note_auth(&Ok(()));
        s.note_auth(&unauthorized);
        s.note_auth(&Err::<(), _>(RdError::NotFound("/x".into())));
        assert!(!s.take_auth_lost());
        s.note_auth(&unauthorized);
        s.note_auth(&unauthorized);
        assert!(s.take_auth_lost());
        // Browsing without an account has no sign-in to lose.
        let anon = Session::from_saved(http_client(), &Saved { refresh_token: None, ..user }, "linux").unwrap();
        anon.note_auth(&Err::<(), _>(RdError::InvalidGrant));
        assert!(!anon.take_auth_lost());
    }
}
