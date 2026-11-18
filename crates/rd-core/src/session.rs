//! One Reddit session: a client ID plus either an app-only identity (browse without an account) or
//! a signed-in user (refresh token). It keeps the access token fresh, sends the required headers,
//! tracks the rate-limit window and maps HTTP failures to `RdError`.

use crate::auth::{self, ClientId, Grant, Token};
use crate::ratelimit::{should_wait, Priority, RateLimit};
use crate::{now_secs, RdError, OAUTH};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;

/// Refresh this many seconds before the access token expires.
const EXPIRY_SKEW: u64 = 120;

/// What is stored per account on disk (the TV keeps one per account; the spike keeps one file).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Saved {
    pub client_id: String,
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
    username: Mutex<Option<String>>,
    token: Mutex<Option<Token>>,
    rate: Mutex<Option<RateLimit>>,
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
            username: Mutex::new(None),
            token: Mutex::new(None),
            rate: Mutex::new(None),
        }
    }

    /// Restores a session from disk; `None` if the saved client ID is malformed.
    pub fn from_saved(http: reqwest::blocking::Client, saved: &Saved, platform: &str) -> Option<Session> {
        let client = ClientId::parse(&saved.client_id).ok()?;
        let identity = match &saved.refresh_token {
            Some(rt) => Identity::User { refresh_token: rt.clone() },
            None => Identity::AppOnly { device_id: saved.device_id.clone() },
        };
        let s = Session::new(http, client, identity, platform);
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

    pub fn user_agent(&self) -> String {
        crate::ua::user_agent(&self.platform, self.username.lock().unwrap().as_deref())
    }

    pub fn set_username(&self, name: Option<String>) {
        *self.username.lock().unwrap() = name;
    }

    pub fn last_rate(&self) -> Option<RateLimit> {
        *self.rate.lock().unwrap()
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
                403 => return Err(RdError::Forbidden(what.to_string())),
                404 => return Err(RdError::NotFound(what.to_string())),
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
        assert!(Session::from_saved(http_client(), &Saved { client_id: "bad id".into(), ..anon }, "linux").is_none());
    }
}
