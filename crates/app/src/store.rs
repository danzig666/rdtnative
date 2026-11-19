//! What the screens read: the saved Reddit API settings, the draft being edited, the one-time
//! phone form, the front page, and the toast. Owned by the rig (main thread); workers report
//! through `host::Work`.

use rd_core::config::{self, ApiConfig, FieldError};
use rd_core::session::Saved;
use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_data_dir(p: PathBuf) {
    let _ = DATA_DIR.set(p);
}

fn data_file(name: &str) -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(std::env::temp_dir).join(name)
}

/// The account file: client ID, redirect URI, user agent, device ID and tokens.
fn account_path() -> PathBuf {
    data_file("reddit.json")
}

pub fn load_saved() -> Option<Saved> {
    let s = std::fs::read_to_string(account_path()).ok()?;
    serde_json::from_str::<Saved>(&s).ok().filter(|s| !s.client_id.is_empty())
}

/// Writes atomically and readable only by the app (it holds tokens).
pub fn store_saved(s: &Saved) -> std::io::Result<()> {
    let p = account_path();
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &p)
}

/// One of the three API settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    ClientId,
    RedirectUri,
    UserAgent,
}

impl Field {
    pub const ALL: [Field; 3] = [Field::ClientId, Field::RedirectUri, Field::UserAgent];

    pub fn key(self) -> &'static str {
        match self {
            Field::ClientId => config::F_CLIENT_ID,
            Field::RedirectUri => config::F_REDIRECT_URI,
            Field::UserAgent => config::F_USER_AGENT,
        }
    }

    #[cfg(test)]
    pub fn from_key(k: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.key() == k)
    }

    /// Label, placeholder (the default when empty), help and limits: one definition shared with
    /// the phone form (`rd_core::config::api_fields`).
    pub fn spec(self, draft: &Draft) -> config::FieldSpec {
        let mut spec = config::api_fields(None).into_iter().find(|s| s.key == self.key()).expect("every field has a spec");
        spec.value = draft.get(self).to_string();
        spec
    }
}

/// The API settings as being edited (raw text; validated on save).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub client_id: String,
    pub redirect_uri: String,
    pub user_agent: String,
}

impl Draft {
    pub fn from_saved(s: Option<&Saved>) -> Draft {
        let Some(s) = s else { return Draft::default() };
        Draft {
            client_id: s.client_id.clone(),
            redirect_uri: s.redirect_uri.clone().filter(|r| r != rd_core::DEFAULT_REDIRECT_URI).unwrap_or_default(),
            user_agent: s.user_agent.clone().unwrap_or_default(),
        }
    }

    pub fn get(&self, f: Field) -> &str {
        match f {
            Field::ClientId => &self.client_id,
            Field::RedirectUri => &self.redirect_uri,
            Field::UserAgent => &self.user_agent,
        }
    }

    pub fn set(&mut self, f: Field, v: String) {
        match f {
            Field::ClientId => self.client_id = v,
            Field::RedirectUri => self.redirect_uri = v,
            Field::UserAgent => self.user_agent = v,
        }
    }

    pub fn validate(&self) -> Result<ApiConfig, Vec<FieldError>> {
        ApiConfig::from_fields(&self.client_id, &self.redirect_uri, &self.user_agent)
    }
}

/// The one-time phone form while it runs (the `PhoneForm` itself lives in the rig).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneView {
    pub url: String,
    pub opened: bool,
}

/// One post of the front page, as the home list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct PostRow {
    pub title: String,
    pub subreddit: String,
    pub score: i64,
    pub comments: i64,
    pub over_18: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Front {
    pub loading: bool,
    pub posts: Vec<PostRow>,
    pub error: Option<String>,
    /// Which listing it is ("Hot" without an account).
    pub title: String,
}

pub struct Store {
    pub saved: Option<Saved>,
    pub draft: Draft,
    /// Messages under the fields from the last save attempt.
    pub errors: Vec<FieldError>,
    pub phone: Option<PhoneView>,
    pub front: Front,
    pub toast: Option<(String, u32)>,
}

impl Store {
    pub fn new() -> Store {
        let saved = load_saved();
        Store { draft: Draft::from_saved(saved.as_ref()), saved, errors: Vec::new(), phone: None, front: Front::default(), toast: None }
    }

    pub fn configured(&self) -> bool {
        self.saved.is_some()
    }

    pub fn toast(&mut self, msg: impl Into<String>, now_ms: u32) {
        self.toast = Some((msg.into(), now_ms.wrapping_add(3500)));
    }

    /// The error for `f` from the last save attempt, if any.
    pub fn error(&self, f: Field) -> Option<&str> {
        self.errors.iter().find(|e| e.field == f.key()).map(|e| e.message.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_shows_defaults_as_empty() {
        let cfg = ApiConfig::from_fields("abcdefghijklmn", "", "").unwrap();
        let d = Draft::from_saved(Some(&Saved::from_config(&cfg)));
        assert_eq!(d, Draft { client_id: "abcdefghijklmn".into(), redirect_uri: String::new(), user_agent: String::new() });
        assert_eq!(d.validate().unwrap(), cfg);
    }

    #[test]
    fn field_specs_match_the_phone_form() {
        let d = Draft { client_id: "x".into(), ..Default::default() };
        let s = Field::ClientId.spec(&d);
        assert_eq!((s.label, s.value.as_str()), ("OAuth client ID", "x"));
        assert_eq!(Field::RedirectUri.spec(&d).placeholder, rd_core::DEFAULT_REDIRECT_URI);
        for f in Field::ALL {
            assert_eq!(Field::from_key(f.key()), Some(f));
        }
    }
}
