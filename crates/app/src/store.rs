//! What the screens read: the saved Reddit API settings and the draft being edited, the user's
//! preferences, the posts (shared by every feed that lists them, so a vote shows everywhere),
//! feeds, comment threads, subreddit info, the signed-in user, and the toast. Owned by the rig
//! (main thread); workers report through `host::Work`.

use crate::prefs::{Prefs, ReadPosts};
use rd_core::comments::Node;
use rd_core::config::{self, ApiConfig, FieldError};
use rd_core::listing::{CommentSort, Feed, Sort, Time};
use rd_core::media::Media;
use rd_core::model::{Post, Subreddit};
use rd_core::session::Saved;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_data_dir(p: PathBuf) {
    let _ = DATA_DIR.set(p);
}

pub fn data_file(name: &str) -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(std::env::temp_dir).join(name)
}

/// The account file: client ID, redirect URI, user agent, device ID and the tokens of the account
/// in use (every signed-in account is also listed in `accounts.json`).
fn account_path() -> PathBuf {
    data_file("reddit.json")
}

pub fn load_saved() -> Option<Saved> {
    let s = std::fs::read_to_string(account_path()).ok()?;
    serde_json::from_str::<Saved>(&s).ok().filter(|s| !s.client_id.is_empty())
}

pub(crate) fn write_atomic(p: &std::path::Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    let _ = private;
    std::fs::rename(&tmp, p)
}

/// Writes atomically and readable only by the app (it holds tokens).
pub fn store_saved(s: &Saved) -> std::io::Result<()> {
    write_atomic(&account_path(), &serde_json::to_vec_pretty(s)?, true)
}

/// Writes a file that holds tokens: atomically, readable only by the app.
pub fn write_private(p: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic(p, bytes, true)
}

pub fn load_prefs() -> Prefs {
    std::fs::read_to_string(data_file("prefs.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn store_prefs(p: &Prefs) {
    if let Ok(b) = serde_json::to_vec_pretty(p) {
        if let Err(e) = write_atomic(&data_file("prefs.json"), &b, false) {
            crate::log(&format!("prefs: save failed: {e}"));
        }
    }
}

fn load_read() -> ReadPosts {
    ReadPosts::from_text(&std::fs::read_to_string(data_file("read.txt")).unwrap_or_default())
}

pub fn store_read(r: &ReadPosts) {
    let _ = write_atomic(&data_file("read.txt"), r.to_text().as_bytes(), false);
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

/// The keyboard's "Type on phone" form while it runs (the `PhoneForm` itself lives in the rig).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypePhoneView {
    pub url: String,
    pub opened: bool,
    /// The keyboard it types for.
    pub target: crate::host::Typing,
}

/// Text for the open keyboard from the phone or from voice search. Each one gets a new `seq`, and
/// a keyboard takes only what arrived after it opened.
#[derive(Clone, Debug, Default)]
pub struct Incoming {
    pub seq: u32,
    pub text: String,
    /// Used as Done uses it (the phone sent it); otherwise only typed in.
    pub done: bool,
}

/// A post as the screens use it: the post, its resolved media and a one-line text preview.
#[derive(Clone, Debug)]
pub struct PostItem {
    pub post: Post,
    pub media: Media,
    pub preview: String,
    /// Comments added since the user last opened the post (0 when never opened).
    pub new_comments: i64,
}

impl PostItem {
    pub fn new(post: Post) -> PostItem {
        let media = rd_core::media::resolve(&post);
        let preview = if post.selftext.is_empty() {
            String::new()
        } else {
            let p = rd_core::markdown::plain(&post.selftext);
            p.chars().take(400).collect()
        };
        PostItem { post, media, preview, new_comments: 0 }
    }
    pub fn id(&self) -> &str {
        &self.post.id
    }
}

/// A feed's identity: where from and how sorted.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeedKey {
    pub feed: Feed,
    pub sort: Sort,
    pub time: Time,
}

#[derive(Clone, Debug, Default)]
pub struct FeedState {
    pub ids: Vec<String>,
    pub after: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Reddit has no more pages.
    pub done: bool,
    /// Reddit withheld this NSFW subreddit's posts (it sent only its "view mature content" stand-in).
    pub gated: bool,
    /// Bumped on every change (screens rebuild their layout when it moves).
    pub gen: u32,
}

#[derive(Clone, Debug)]
pub struct ThreadState {
    pub tree: Vec<Node>,
    /// Comments the user opened or closed (flips Reddit's own collapsed flag).
    pub toggled: HashSet<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub sort: CommentSort,
    /// "Load more" rows being fetched.
    pub loading_more: HashSet<String>,
    pub gen: u32,
    /// When the comments last arrived (Unix seconds; 0 = never).
    pub fetched_at: u64,
    /// Only a preview's few top comments are here (or on their way): opening the post loads all.
    pub partial: bool,
    /// Comments a reload brought that the thread did not have before (new while it was open).
    pub arrived: HashSet<String>,
}

/// A thread's key in [`Store::threads`]: the post ID, or `post/comment` for the thread of one
/// comment (a comment link), kept apart from the post's whole thread.
pub fn thread_key(post: &str, comment: Option<&str>) -> String {
    match comment {
        Some(c) => format!("{post}/{c}"),
        None => post.to_string(),
    }
}

/// A thread key's post ID and comment.
pub fn split_thread_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once('/') {
        Some((p, c)) => (p, Some(c)),
        None => (key, None),
    }
}

/// What a subreddit's header shows.
#[derive(Clone, Debug)]
pub struct SubAbout {
    pub sub: Subreddit,
    pub subscribed: bool,
    pub active: i64,
}

#[derive(Clone, Debug, Default)]
pub struct SubList {
    pub items: Vec<Subreddit>,
    pub loading: bool,
    pub error: Option<String>,
    pub gen: u32,
}

/// The signed-in user's custom feeds.
#[derive(Clone, Debug, Default)]
pub struct MultiList {
    pub items: Vec<rd_core::model::Multi>,
    pub loading: bool,
    pub error: Option<String>,
    pub gen: u32,
}

/// A subreddit's info page: its about and its rules.
#[derive(Clone, Debug, Default)]
pub struct SubPage {
    pub info: Option<rd_core::model::SubInfo>,
    pub rules: Vec<rd_core::model::Rule>,
    pub loading: bool,
    pub error: Option<String>,
    pub gen: u32,
}

/// A user's profile header.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub info: Option<rd_core::model::UserInfo>,
    pub loading: bool,
    pub error: Option<String>,
}

/// One entry of a user's listing: a post (kept in `Store::posts` with every other post, so a
/// vote shows everywhere) or a comment.
#[derive(Clone, Debug)]
pub enum UserItem {
    Post(String),
    Comment(Box<rd_core::comments::UserComment>),
}

impl UserItem {
    fn key(&self) -> &str {
        match self {
            UserItem::Post(id) => id,
            UserItem::Comment(c) => &c.comment.name,
        }
    }
}

/// One tab of a user's listing, keyed like a feed (`Feed::User`).
#[derive(Clone, Debug, Default)]
pub struct UserPage {
    pub items: Vec<UserItem>,
    pub after: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Reddit has no more pages.
    pub done: bool,
    pub gen: u32,
}

impl UserPage {
    /// Takes a page that arrived: the first one replaces the list, a next one is added without
    /// the entries already shown (pages shift while the user reads).
    pub fn take(&mut self, items: Vec<UserItem>, after: Option<String>, more: bool) {
        if !more {
            self.items.clear();
        }
        self.done = after.is_none() || items.is_empty();
        self.after = after;
        let mut seen: HashSet<String> = self.items.iter().map(|i| i.key().to_string()).collect();
        self.items.extend(items.into_iter().filter(|i| seen.insert(i.key().to_string())));
        self.error = None;
    }
}

/// The sign-in flow while it runs.
#[derive(Clone, Debug, Default)]
pub struct SignInView {
    pub phone_url: Option<String>,
    /// The sign-in page's server as a redirect URI (`http://<tv>:<port>/`): registered on the
    /// Reddit app, sign-in needs no copying.
    pub auto_redirect: Option<String>,
    pub busy: bool,
    pub error: Option<String>,
}

/// One inbox message.
#[derive(Clone, Debug)]
pub struct Message {
    pub name: String,
    pub author: String,
    pub subject: String,
    pub body: String,
    pub created: i64,
    pub unread: bool,
    pub context: Option<String>,
    pub subreddit: Option<String>,
}

pub struct Store {
    pub saved: Option<Saved>,
    pub draft: Draft,
    /// Messages under the fields from the last save attempt.
    pub errors: Vec<FieldError>,
    pub phone: Option<PhoneView>,
    pub toast: Option<(String, u32)>,
    pub prefs: Prefs,
    pub read: ReadPosts,
    /// Posts opened before, with their comment counts then.
    pub seen: crate::seen::SeenPosts,
    pub posts: HashMap<String, PostItem>,
    pub feeds: HashMap<FeedKey, FeedState>,
    pub threads: HashMap<String, ThreadState>,
    pub about: HashMap<String, SubAbout>,
    /// The signed-in user's subscriptions.
    pub subs_mine: SubList,
    pub subs_popular: SubList,
    pub subs_search: (String, SubList),
    pub multis: MultiList,
    /// Subreddit info pages and user profiles by lower-cased name, and the users' listings.
    pub sub_pages: HashMap<String, SubPage>,
    pub profiles: HashMap<String, Profile>,
    pub user_pages: HashMap<FeedKey, UserPage>,
    pub inbox: SubList,
    pub messages: Vec<Message>,
    pub unread: i64,
    pub signin: Option<SignInView>,
    /// Imgur lookups in flight (post IDs).
    pub imgur_pending: HashSet<String>,
    /// Bumped on any change worth a redraw.
    pub gen: u32,
    /// The video player's state, copied each frame.
    pub play: crate::player::Status,
    /// A page to open, from a background job (a resolved share link); taken by the frame loop.
    pub nav_request: Option<crate::host::RdArg>,
    /// Every account signed in on this TV, the one in use included (`accounts.rs`).
    pub accounts: Vec<Saved>,
    /// Bumped by every successful save of the API settings and every successful sign-in: the
    /// pages that started them see that it went through.
    pub api_saves: u32,
    pub signins: u32,
    /// Unsent replies by parent fullname: kept when the keyboard closes or the send fails.
    pub drafts: HashMap<String, String>,
    pub type_phone: Option<TypePhoneView>,
    pub incoming: Incoming,
}

impl Store {
    pub fn new() -> Store {
        let saved = load_saved();
        let accounts = crate::accounts::load(saved.as_ref());
        let prefs = load_prefs();
        prefs.apply();
        Store {
            draft: Draft::from_saved(saved.as_ref()),
            saved,
            errors: Vec::new(),
            phone: None,
            toast: None,
            prefs,
            read: load_read(),
            seen: crate::seen::load(),
            posts: HashMap::new(),
            feeds: HashMap::new(),
            threads: HashMap::new(),
            about: HashMap::new(),
            subs_mine: SubList::default(),
            subs_popular: SubList::default(),
            subs_search: (String::new(), SubList::default()),
            multis: MultiList::default(),
            sub_pages: HashMap::new(),
            profiles: HashMap::new(),
            user_pages: HashMap::new(),
            inbox: SubList::default(),
            messages: Vec::new(),
            unread: 0,
            signin: None,
            imgur_pending: HashSet::new(),
            gen: 0,
            play: Default::default(),
            nav_request: None,
            accounts,
            api_saves: 0,
            signins: 0,
            drafts: HashMap::new(),
            type_phone: None,
            incoming: Incoming::default(),
        }
    }

    pub fn configured(&self) -> bool {
        self.saved.is_some()
    }

    pub fn signed_in(&self) -> bool {
        self.saved.as_ref().is_some_and(|s| s.refresh_token.is_some())
    }

    pub fn username(&self) -> Option<&str> {
        self.saved.as_ref().and_then(|s| s.username.as_deref()).filter(|_| self.signed_in())
    }

    pub fn toast(&mut self, msg: impl Into<String>, now_ms: u32) {
        self.toast = Some((msg.into(), now_ms.wrapping_add(3500)));
    }

    /// The error for `f` from the last save attempt, if any.
    pub fn error(&self, f: Field) -> Option<&str> {
        self.errors.iter().find(|e| e.field == f.key()).map(|e| e.message.as_str())
    }

    pub fn post(&self, id: &str) -> Option<&PostItem> {
        self.posts.get(id)
    }

    pub fn feed(&self, k: &FeedKey) -> Option<&FeedState> {
        self.feeds.get(k)
    }

    /// The posts of feed `k` that pass the user's filters, in order.
    pub fn feed_items(&self, k: &FeedKey) -> Vec<&PostItem> {
        let Some(f) = self.feeds.get(k) else { return Vec::new() };
        let in_sub = k.feed.sub().is_some();
        f.ids
            .iter()
            .filter_map(|id| self.posts.get(id))
            .filter(|p| {
                let read = self.read.contains(&p.post.id);
                // A subreddit's own page shows its own posts even when it is filtered elsewhere.
                let sub_ok = in_sub || !self.prefs.filter_subs.iter().any(|f| f.eq_ignore_ascii_case(&p.post.subreddit));
                sub_ok && self.prefs.keeps("", &p.post.title, p.post.over_18, p.post.stickied, read)
            })
            .collect()
    }

    /// The sort a feed opens with: remembered per subreddit, else the user's default.
    pub fn key_for(&self, feed: Feed) -> FeedKey {
        let signed_in = self.signed_in();
        let sorts = feed.sorts(signed_in);
        let mut sort = Sort::from_key(self.prefs.get("default_sort")).unwrap_or(Sort::Hot);
        if !sorts.contains(&sort) {
            sort = feed.default_sort(signed_in);
        }
        if matches!(feed, Feed::Search { .. } | Feed::User(..)) {
            sort = feed.default_sort(signed_in);
        }
        let mut time = Time::from_key(self.prefs.get("default_time")).unwrap_or(Time::Day);
        if let (Some(sub), true) = (feed.sub(), self.prefs.on("remember_sort")) {
            if let Some(v) = self.prefs.sub_sorts.get(&sub.to_ascii_lowercase()) {
                let (s, t) = v.split_once(':').unwrap_or((v, ""));
                if let Some(s) = Sort::from_key(s).filter(|s| sorts.contains(s)) {
                    sort = s;
                }
                if let Some(t) = Time::from_key(t) {
                    time = t;
                }
            }
        }
        if sorts.is_empty() {
            sort = Sort::New;
        }
        FeedKey { feed, sort, time }
    }

    /// Media of a post with Imgur pages resolved once they are.
    pub fn media<'a>(&'a self, p: &'a PostItem) -> &'a Media {
        &p.media
    }

    pub fn thread(&self, id: &str) -> Option<&ThreadState> {
        self.threads.get(id)
    }

    pub fn bump(&mut self) {
        self.gen = self.gen.wrapping_add(1);
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
    fn user_pages_add_pages_without_repeats() {
        let comment = |name: &str| {
            let v = serde_json::json!({"kind":"Listing","data":{"children":[{"kind":"t1","data":{"name": name, "link_id": "t3_p"}}]}});
            match rd_core::comments::parse_things(&v).unwrap().items.remove(0) {
                rd_core::comments::Thing::Comment(c) => UserItem::Comment(Box::new(c)),
                _ => unreachable!(),
            }
        };
        let keys = |p: &UserPage| p.items.iter().map(|i| i.key().to_string()).collect::<Vec<_>>();
        let mut p = UserPage { error: Some("old".into()), ..Default::default() };
        p.take(vec![UserItem::Post("a".into()), comment("t1_b")], Some("t1_b".into()), false);
        assert_eq!((keys(&p), p.done, p.error.as_deref()), (vec!["a".to_string(), "t1_b".to_string()], false, None));
        // The next page repeats an entry that moved down meanwhile.
        p.take(vec![comment("t1_b"), UserItem::Post("c".into())], None, true);
        assert_eq!(keys(&p), ["a", "t1_b", "c"]);
        assert!(p.done);
        // A reload starts over.
        p.take(vec![UserItem::Post("d".into())], Some("t3_d".into()), false);
        assert_eq!((keys(&p), p.done), (vec!["d".to_string()], false));
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

    #[test]
    fn thread_keys_split_back() {
        assert_eq!(thread_key("abc", None), "abc");
        assert_eq!(split_thread_key(&thread_key("abc", Some("def"))), ("abc", Some("def")));
        assert_eq!(split_thread_key("abc"), ("abc", None));
    }
}
