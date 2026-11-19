//! The user's Reddit API settings: their own client ID, the redirect URI registered on their Reddit
//! app, and an optional User-Agent override. Entered on the TV (on-screen keyboard) or on a phone
//! through the one-time LAN form (`rd-phone`); both go through `ApiConfig::from_fields`.

use crate::auth::ClientId;
use serde::{Deserialize, Serialize};

/// Field keys shared by the TV editor and the phone form.
pub const F_CLIENT_ID: &str = "client_id";
pub const F_REDIRECT_URI: &str = "redirect_uri";
pub const F_USER_AGENT: &str = "user_agent";

pub const MAX_REDIRECT_URI: usize = 512;
pub const MAX_USER_AGENT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiConfig {
    pub client_id: String,
    /// Must equal the redirect URI on the user's Reddit app, character for character.
    pub redirect_uri: String,
    /// `None` = rdtnative's default (`webos:io.github.danzig666.rdtnative:v… (by /u/<name>)`).
    #[serde(default)]
    pub user_agent: Option<String>,
}

/// A problem with one field, for showing next to that field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}

impl ApiConfig {
    /// Builds and validates from raw text fields (trimmed). An empty redirect URI means the default;
    /// an empty user agent means the default too.
    pub fn from_fields(client_id: &str, redirect_uri: &str, user_agent: &str) -> Result<ApiConfig, Vec<FieldError>> {
        let mut errs = Vec::new();
        let client_id = match ClientId::parse(client_id) {
            Ok(c) => c.as_str().to_string(),
            Err(e) => {
                errs.push(FieldError { field: F_CLIENT_ID, message: capitalise(&e) });
                String::new()
            }
        };
        let redirect_uri = match redirect_uri.trim() {
            "" => crate::DEFAULT_REDIRECT_URI.to_string(),
            r => {
                if let Err(e) = check_redirect_uri(r) {
                    errs.push(FieldError { field: F_REDIRECT_URI, message: e });
                }
                r.to_string()
            }
        };
        let user_agent = match user_agent.trim() {
            "" => None,
            u => {
                if let Err(e) = check_user_agent(u) {
                    errs.push(FieldError { field: F_USER_AGENT, message: e });
                }
                Some(u.to_string())
            }
        };
        if errs.is_empty() {
            Ok(ApiConfig { client_id, redirect_uri, user_agent })
        } else {
            Err(errs)
        }
    }

    pub fn client(&self) -> ClientId {
        ClientId::parse(&self.client_id).expect("validated in from_fields")
    }

    /// True when sign-in can hand the code to the TV by itself (our callback page does that);
    /// with any other redirect URI the user types or pastes the code.
    pub fn uses_default_redirect(&self) -> bool {
        self.redirect_uri == crate::DEFAULT_REDIRECT_URI
    }
}

/// One editable setting, described once for both the TV editor (on-screen keyboard) and the phone
/// form, so labels, help and limits never drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    /// Current value (empty when unset, so the placeholder shows the default).
    pub value: String,
    /// The default that applies when the field is left empty.
    pub placeholder: String,
    pub help: &'static str,
    /// Opens a URL keyboard.
    pub url: bool,
    pub max_len: usize,
}

/// Where users create their Reddit app.
pub const REDDIT_APPS_URL: &str = "https://www.reddit.com/prefs/apps";

/// Short instructions shown above the fields (TV and phone).
pub const SETUP_STEPS: &[&str] = &[
    "1. Open reddit.com/prefs/apps and create an app of type \"installed app\".",
    "2. Set its redirect URI to the one below (or your own, and enter that here).",
    "3. Copy the client ID shown under the app's name.",
];

/// The three API settings, pre-filled from `current`.
pub fn api_fields(current: Option<&ApiConfig>) -> Vec<FieldSpec> {
    let default_ua = crate::ua::user_agent(crate::ua::host_platform(), Some("<your username>"));
    vec![
        FieldSpec {
            key: F_CLIENT_ID,
            label: "OAuth client ID",
            value: current.map(|c| c.client_id.clone()).unwrap_or_default(),
            placeholder: "e.g. p-jcoLKBynTLew".into(),
            help: "The short code under your app's name on reddit.com/prefs/apps. Installed apps have no secret.",
            url: false,
            max_len: 30,
        },
        FieldSpec {
            key: F_REDIRECT_URI,
            label: "Redirect URI",
            value: current
                .filter(|c| !c.uses_default_redirect())
                .map(|c| c.redirect_uri.clone())
                .unwrap_or_default(),
            placeholder: crate::DEFAULT_REDIRECT_URI.into(),
            help: "Must match your Reddit app exactly. Leave empty for rdtnative's page, which hands the sign-in to the TV by itself.",
            url: true,
            max_len: MAX_REDIRECT_URI,
        },
        FieldSpec {
            key: F_USER_AGENT,
            label: "User agent (optional)",
            value: current.and_then(|c| c.user_agent.clone()).unwrap_or_default(),
            placeholder: default_ua,
            help: "Leave empty for the default. Reddit asks for <platform>:<app id>:<version> (by /u/<username>).",
            url: false,
            max_len: MAX_USER_AGENT,
        },
    ]
}

/// Validates submitted values by key (missing keys count as empty).
pub fn from_values(get: impl Fn(&str) -> String) -> Result<ApiConfig, Vec<FieldError>> {
    ApiConfig::from_fields(&get(F_CLIENT_ID), &get(F_REDIRECT_URI), &get(F_USER_AGENT))
}

/// Reddit compares the redirect URI exactly; we only make sure it is a plausible absolute URI.
/// Any scheme is accepted (`https://`, `http://`, a custom `myapp://cb` or `com.example:/cb`).
pub fn check_redirect_uri(r: &str) -> Result<(), String> {
    if r.len() > MAX_REDIRECT_URI {
        return Err(format!("Too long (at most {MAX_REDIRECT_URI} characters)"));
    }
    let (scheme, rest) = r.split_once(':').ok_or("Must start with a scheme, like https:// or myapp://")?;
    let mut sc = scheme.chars();
    if !sc.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !sc.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return Err("Must start with a scheme, like https:// or myapp://".into());
    }
    if let Some(auth) = rest.strip_prefix("//") {
        if auth.split(['/', '?', '#']).next().unwrap_or("").is_empty() {
            return Err("Has no host name".into());
        }
    } else if rest.is_empty() {
        return Err("Has nothing after the scheme".into());
    }
    if r.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Cannot contain spaces".into());
    }
    if r.contains('#') {
        return Err("Cannot contain #".into());
    }
    Ok(())
}

/// Printable ASCII only (HTTP headers), and it should identify the app the way Reddit asks.
pub fn check_user_agent(u: &str) -> Result<(), String> {
    if u.len() > MAX_USER_AGENT {
        return Err(format!("Too long (at most {MAX_USER_AGENT} characters)"));
    }
    if let Some(c) = u.chars().find(|c| !(' '..='~').contains(c)) {
        return Err(format!("Only plain ASCII letters, digits and punctuation ({c:?} is not allowed)"));
    }
    if u.len() < 8 {
        return Err("Too short: Reddit asks for <platform>:<app id>:<version> (by /u/<username>)".into());
    }
    Ok(())
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_specs_prefill_only_non_defaults() {
        let f = api_fields(None);
        assert_eq!(f.iter().map(|f| f.key).collect::<Vec<_>>(), [F_CLIENT_ID, F_REDIRECT_URI, F_USER_AGENT]);
        assert!(f.iter().all(|f| f.value.is_empty()));
        assert_eq!(f[1].placeholder, crate::DEFAULT_REDIRECT_URI);
        assert!(f[2].placeholder.contains("(by /u/<your username>)"));

        let c = ApiConfig::from_fields("abcdefghijklmn", "", "webos:a.b:v1 (by /u/me)").unwrap();
        let f = api_fields(Some(&c));
        assert_eq!((f[0].value.as_str(), f[1].value.as_str(), f[2].value.as_str()), ("abcdefghijklmn", "", "webos:a.b:v1 (by /u/me)"));
        let back = from_values(|k| f.iter().find(|x| x.key == k).unwrap().value.clone()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn defaults_fill_empty_fields() {
        let c = ApiConfig::from_fields(" abcdefghijklmn ", "", "  ").unwrap();
        assert_eq!(c.client_id, "abcdefghijklmn");
        assert_eq!(c.redirect_uri, crate::DEFAULT_REDIRECT_URI);
        assert_eq!(c.user_agent, None);
        assert!(c.uses_default_redirect());
    }

    #[test]
    fn custom_values_are_kept() {
        let c = ApiConfig::from_fields("abcdefghijklmn", "http://localhost:8080/cb", "webos:my.app:v1 (by /u/me)").unwrap();
        assert_eq!(c.redirect_uri, "http://localhost:8080/cb");
        assert_eq!(c.user_agent.as_deref(), Some("webos:my.app:v1 (by /u/me)"));
        assert!(!c.uses_default_redirect());
    }

    #[test]
    fn every_bad_field_is_reported() {
        let e = ApiConfig::from_fields("x", "1ftp://a", "ua\u{e9}tooshort").unwrap_err();
        let fields: Vec<_> = e.iter().map(|e| e.field).collect();
        assert_eq!(fields, [F_CLIENT_ID, F_REDIRECT_URI, F_USER_AGENT]);
        assert!(e[0].message.starts_with("A client ID"));
        assert!(check_redirect_uri("https://").is_err());
        assert!(check_redirect_uri("https://a b/").is_err());
        assert!(check_redirect_uri("https://a/cb#x").is_err());
        assert!(check_redirect_uri("no-scheme/cb").is_err());
        assert!(check_redirect_uri("myapp:").is_err());
        assert!(check_redirect_uri("//host/cb").is_err());
    }

    #[test]
    fn any_scheme_is_accepted() {
        for ok in ["https://a/cb", "http://localhost:8080/cb", "ftp://a", "myapp://callback", "com.example.app:/oauth2redirect", "rdt+tv://x"] {
            assert_eq!(check_redirect_uri(ok), Ok(()), "{ok}");
        }
        assert!(check_user_agent("short").is_err());
        assert!(check_user_agent(&"a".repeat(300)).is_err());
    }
}
