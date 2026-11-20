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

/// The account file: client ID, redirect URI, user agent, device ID and tokens.
fn account_path() -> PathBuf {
    data_file("reddit.json")
}

pub fn load_saved() -> Option<Saved> {
    let s = std::fs::read_to_string(account_path()).ok()?;
    serde_json::from_str::<Saved>(&s).ok().filter(|s| !s.client_id.is_empty())
}

fn write_atomic(p: &std::path::Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
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

/// A post as the screens use it: the post, its resolved media and a one-line text preview.
#[derive(Clone, Debug)]
pub struct PostItem {
    pub post: Post,
    pub media: Media,
    pub preview: String,
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
        PostItem { post, media, preview }
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
    /// When the first page last arrived (Unix seconds; 0 = never): an old feed is refreshed or
    /// says how old it is.
    pub fetched_at: u64,
    /// The last error was Reddit refusing the sign-in: the feed offers Sign in.
    pub auth_failed: bool,
    /// The last request hit the rate limit: try again after this many seconds, the next page
    /// (`true`) or the first.
    pub retry: Option<(u64, bool)>,
}

/// A feed fetched longer ago than this is out of date (refreshed when the user is near its top).
pub const FEED_STALE_SECS: u64 = 30 * 60;

impl FeedState {
    /// Fetched, and longer than [`FEED_STALE_SECS`] before `now` (Unix seconds).
    pub fn stale(&self, now: u64) -> bool {
        self.fetched_at > 0 && now.saturating_sub(self.fetched_at) > FEED_STALE_SECS
    }
}

/// The store keeps every post it was sent until it holds more than this; then the posts (and
/// comment threads) no feed and no open page needs are dropped.
pub const POSTS_MAX: usize = 5000;

/// A hidden post's places, kept for a few seconds so Yellow can undo the hide.
#[derive(Clone, Debug)]
pub struct HiddenPost {
    pub id: String,
    /// The feeds it was removed from and its index in each.
    pub places: Vec<(FeedKey, usize)>,
    /// Until when (ms) Yellow undoes it.
    pub until_ms: u32,
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

/// How long a toast stays up unless it says otherwise.
pub const TOAST_MS: u32 = 3500;

/// Where a feed's last sort is remembered in `Prefs::sub_sorts`: a subreddit under its name,
/// Home, Popular and All under `@front`, `@popular` and `@all`. Searches and user pages are not
/// remembered (a search in r/x must not change r/x's sort).
pub fn sort_slot(feed: &Feed) -> Option<String> {
    match feed {
        Feed::Sub(s) => Some(s.to_ascii_lowercase()),
        Feed::Front => Some("@front".into()),
        Feed::Popular => Some("@popular".into()),
        Feed::All => Some("@all".into()),
        _ => None,
    }
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
    pub posts: HashMap<String, PostItem>,
    pub feeds: HashMap<FeedKey, FeedState>,
    pub threads: HashMap<String, ThreadState>,
    pub about: HashMap<String, SubAbout>,
    /// The signed-in user's subscriptions.
    pub subs_mine: SubList,
    pub subs_popular: SubList,
    pub subs_search: (String, SubList),
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
    /// How long the current toast stays up (ms).
    pub toast_len: u32,
    /// Bumped when the app comes back from the background (screens check what went stale).
    pub resume_gen: u32,
    /// The post hidden last, while Yellow can still undo it.
    pub hidden: Option<HiddenPost>,
}

impl Store {
    pub fn new() -> Store {
        let saved = load_saved();
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
            posts: HashMap::new(),
            feeds: HashMap::new(),
            threads: HashMap::new(),
            about: HashMap::new(),
            subs_mine: SubList::default(),
            subs_popular: SubList::default(),
            subs_search: (String::new(), SubList::default()),
            inbox: SubList::default(),
            messages: Vec::new(),
            unread: 0,
            signin: None,
            imgur_pending: HashSet::new(),
            gen: 0,
            play: Default::default(),
            nav_request: None,
            toast_len: TOAST_MS,
            resume_gen: 0,
            hidden: None,
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
        self.toast_for(msg, now_ms, TOAST_MS);
    }

    /// A toast that stays up `ms` (one that names a key's time window lasts exactly as long).
    pub fn toast_for(&mut self, msg: impl Into<String>, now_ms: u32, ms: u32) {
        self.toast = Some((msg.into(), now_ms.wrapping_add(ms)));
        self.toast_len = ms;
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

    /// The sort a feed opens with: remembered per subreddit (and for Home, Popular, All), else the
    /// user's default ("auto": Reddit's own for that feed).
    pub fn key_for(&self, feed: Feed) -> FeedKey {
        let signed_in = self.signed_in();
        let sorts = feed.sorts(signed_in);
        let mut sort = Sort::from_key(self.prefs.get("default_sort")).unwrap_or(feed.default_sort(signed_in));
        if !sorts.contains(&sort) {
            sort = feed.default_sort(signed_in);
        }
        if matches!(feed, Feed::Search { .. } | Feed::User(..)) {
            sort = feed.default_sort(signed_in);
        }
        let mut time = Time::from_key(self.prefs.get("default_time")).unwrap_or(Time::Day);
        if let (Some(slot), true) = (sort_slot(&feed), self.prefs.on("remember_sort")) {
            if let Some(v) = self.prefs.sub_sorts.get(&slot) {
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

    /// Takes post `id` out of every feed; returns where it was (for [`Store::unhide`]).
    pub fn hide_post(&mut self, id: &str) -> Vec<(FeedKey, usize)> {
        let mut places = Vec::new();
        for (k, f) in self.feeds.iter_mut() {
            if let Some(i) = f.ids.iter().position(|x| x == id) {
                f.ids.remove(i);
                f.gen = f.gen.wrapping_add(1);
                places.push((k.clone(), i));
            }
        }
        places
    }

    /// Puts a hidden post back where [`Store::hide_post`] took it from (feeds reloaded since keep
    /// it out, and so do feeds that list it again).
    pub fn unhide(&mut self, id: &str, places: &[(FeedKey, usize)]) {
        for (k, i) in places {
            if let Some(f) = self.feeds.get_mut(k) {
                if !f.ids.iter().any(|x| x == id) {
                    f.ids.insert((*i).min(f.ids.len()), id.to_string());
                    f.gen = f.gen.wrapping_add(1);
                }
            }
        }
        self.bump();
    }

    /// Once there are more than [`POSTS_MAX`] posts: drops the posts no feed lists and that are
    /// not in `open` (posts on pages still on the stack), and the comment threads of those posts
    /// (a thread key may carry a comment after a '/').
    pub fn evict(&mut self, open: &HashSet<String>) {
        if self.posts.len() <= POSTS_MAX {
            return;
        }
        let mut keep: HashSet<&str> = open.iter().map(String::as_str).collect();
        for f in self.feeds.values() {
            keep.extend(f.ids.iter().map(String::as_str));
        }
        if let Some(h) = &self.hidden {
            keep.insert(&h.id);
        }
        let before = (self.posts.len(), self.threads.len());
        self.posts.retain(|id, _| keep.contains(id.as_str()));
        self.threads.retain(|k, _| keep.contains(k.split('/').next().unwrap_or(k)));
        crate::log(&format!("store: dropped {} posts and {} threads", before.0 - self.posts.len(), before.1 - self.threads.len()));
        self.bump();
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

    /// A store with no saved files (the tests' data folder is empty).
    fn blank() -> Store {
        let dir = std::env::temp_dir().join("rdtnative-store-tests");
        let _ = std::fs::create_dir_all(&dir);
        set_data_dir(dir);
        Store::new()
    }

    fn feed(ids: &[&str]) -> FeedState {
        FeedState { ids: ids.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn post(id: &str) -> PostItem {
        PostItem::new(Post::from_json(&serde_json::json!({ "id": id })))
    }

    #[test]
    fn sorts_are_remembered_for_home_but_not_for_searches() {
        let mut s = blank();
        assert_eq!(sort_slot(&Feed::Front).as_deref(), Some("@front"));
        assert_eq!(sort_slot(&Feed::Sub("Pics".into())).as_deref(), Some("pics"));
        assert_eq!(sort_slot(&Feed::Search { q: "cats".into(), sub: Some("pics".into()) }), None);
        // Reddit's default: Hot for a subreddit (Best is for a signed-in Home).
        assert_eq!(s.key_for(Feed::Sub("pics".into())).sort, Sort::Hot);
        s.prefs.sub_sorts.insert("@popular".into(), "top:week".into());
        let k = s.key_for(Feed::Popular);
        assert_eq!((k.sort, k.time), (Sort::Top, Time::Week));
        s.prefs.set("default_sort", "new");
        assert_eq!(s.key_for(Feed::All).sort, Sort::New);
    }

    #[test]
    fn a_hidden_post_comes_back_where_it_was() {
        let mut s = blank();
        let (a, b) = (s.key_for(Feed::Front), s.key_for(Feed::All));
        s.feeds.insert(a.clone(), feed(&["1", "2", "3"]));
        s.feeds.insert(b.clone(), feed(&["2"]));
        let places = s.hide_post("2");
        assert_eq!(places.len(), 2);
        assert_eq!(s.feeds[&a].ids, ["1", "3"]);
        s.unhide("2", &places);
        s.unhide("2", &places);
        assert_eq!(s.feeds[&a].ids, ["1", "2", "3"]);
        assert_eq!(s.feeds[&b].ids, ["2"]);
    }

    #[test]
    fn a_full_store_keeps_what_feeds_and_pages_show() {
        let mut s = blank();
        for i in 0..=POSTS_MAX {
            s.posts.insert(i.to_string(), post(&i.to_string()));
        }
        s.feeds.insert(s.key_for(Feed::Front), feed(&["1", "2"]));
        let thread = || ThreadState { tree: Vec::new(), toggled: HashSet::new(), loading: false, error: None, sort: CommentSort::Top, loading_more: HashSet::new(), gen: 0 };
        s.threads.insert("3/c1".into(), thread());
        s.threads.insert("4".into(), thread());
        s.evict(&HashSet::from(["3".to_string()]));
        let mut kept: Vec<&String> = s.posts.keys().collect();
        kept.sort();
        assert_eq!(kept, ["1", "2", "3"]);
        assert!(s.threads.contains_key("3/c1") && !s.threads.contains_key("4"));
    }

    #[test]
    fn feeds_go_stale_and_toasts_keep_their_time() {
        let f = FeedState { fetched_at: 1_000, ..Default::default() };
        assert!(!f.stale(1_000 + FEED_STALE_SECS));
        assert!(f.stale(1_001 + FEED_STALE_SECS));
        assert!(!FeedState::default().stale(u64::MAX), "never fetched is not stale");
        let mut s = blank();
        s.toast_for("x", 100, 2000);
        assert_eq!((s.toast.as_ref().map(|t| t.1), s.toast_len), (Some(2100), 2000));
        s.toast("y", 0);
        assert_eq!(s.toast_len, TOAST_MS);
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
