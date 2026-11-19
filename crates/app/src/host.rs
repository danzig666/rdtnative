//! The app's `machine::Host`: screen arguments, effects, messages and views, plus the `Rig` the
//! dispatcher drives (mounting screens, running Reddit requests on worker threads, per-frame
//! preparation). Same shape as ytnative's `host.rs`, with Reddit's state.

use crate::screens;
use crate::store::{self, FeedKey, FeedState, Field, Message, PhoneView, PostItem, SignInView, Store, SubAbout, ThreadState};
use plx_machine::machine::{
    Addr, Canon, Chrome, Cx, Effects, EntryId, Handled, Host, InputOwner, InstanceId, LogicalState, MachineId, RequestId, ScreenId,
    TimerId,
};
use plx_machine::present::Present;
use plx_ui::dispatch::{CxParts, Rig, Split};
use plx_ui::frame::Budget;
use plx_ui::screen::{Mounter, ReturnState, Screen, ScreenArg};
use rd_core::comments::{self, Node};
use rd_core::config::{self, ApiConfig};
use rd_core::listing::{self, CommentSort, Feed};
use rd_core::media::{self, Media};
use rd_core::model::{self, Post, Subreddit};
use rd_core::ratelimit::Priority;
use rd_core::session::{Saved, Session};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub struct RdHost;

/// What a typed text is for (the on-screen keyboard's target).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Typing {
    Api(Field),
    Search { sub: Option<String> },
    GoToSub,
    FindSub,
    ImgurId,
    FilterWord,
    FilterSub,
    /// A reply to this fullname (a post or a comment), on post `post`.
    Reply { parent: String, post: String },
    SignInCode,
}

/// The pop-up menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuKind {
    /// Actions on a post.
    Post(String),
    /// Actions on a comment of post `0`.
    Comment(String, String),
    /// Sort of a feed.
    Sort(FeedKey),
    /// Time range of a feed.
    Time(FeedKey),
    /// Comment sort of a post.
    CommentSort(String),
    /// Actions on a subreddit.
    Sub(String),
    /// One settings option's values.
    Option(&'static str),
    /// Confirm being an adult before showing NSFW unblurred.
    AdultCheck,
    /// Links in a post or comment: (post id, comment name or empty).
    Links(String, String),
    /// A newer release was found: install it now?
    Update,
}

/// What a navigation entry shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RdArg {
    /// The start page: the side menu and a feed.
    Home,
    /// A feed opened on its own (a subreddit, a user, search results).
    Feed(Feed),
    /// A post with its comments; optionally scrolled to one comment.
    Post { id: String, comment: Option<String> },
    /// Full-screen pictures (a gallery from item `index`), video posters, links.
    Viewer { id: String, index: usize },
    /// A link opened on the TV: a QR code to open it on a phone.
    Link(String),
    Settings,
    SettingsPage(usize),
    /// Filters, favourites: lists the user edits.
    Lists,
    About,
    /// Settings › Reddit API: client ID, redirect URI, user agent; typed here or on a phone.
    ApiSettings,
    /// The on-screen keyboard.
    Type(Typing),
    Subs,
    SignIn,
    Inbox,
    Menu(MenuKind),
    /// Full-screen video: (stream URL, title, loops).
    Play { url: String, title: String, looping: bool },
}

impl ScreenArg for RdArg {
    fn chrome(&self) -> Chrome {
        Chrome::None
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            RdArg::Home => 1,
            RdArg::ApiSettings => 2,
            RdArg::Type(_) => 3,
            RdArg::Feed(_) => 4,
            RdArg::Post { .. } => 5,
            RdArg::Viewer { .. } => 6,
            RdArg::Link(_) => 7,
            RdArg::Settings => 8,
            RdArg::SettingsPage(_) => 9,
            RdArg::Lists => 10,
            RdArg::About => 11,
            RdArg::Subs => 12,
            RdArg::SignIn => 13,
            RdArg::Inbox => 14,
            RdArg::Menu(_) => 15,
            RdArg::Play { .. } => 16,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

impl LogicalState for RdArg {
    fn write(&self, c: &mut Canon) {
        c.u32(self.id().0);
        c.str(&format!("{self:?}"));
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("{self:?}"));
    }
}

pub enum RdFx {
    Toast(String),
    /// The keyboard changed a field of the draft.
    SetField(Field, String),
    /// Validate the draft; when it is valid, save it and connect.
    SaveApi,
    /// Open / close the one-time phone form for the API settings.
    PhoneStart,
    PhoneStop,
    /// Load a feed (again when `reload`), or its next page.
    LoadFeed { key: FeedKey, reload: bool },
    MoreFeed(FeedKey),
    LoadThread { id: String, sort: Option<CommentSort>, reload: bool },
    /// Fetch a "load more comments" row.
    MoreComments { post: String, more_id: String },
    /// Open or close a comment thread.
    ToggleComment { post: String, name: String },
    /// Resolve an Imgur page of a post through Imgur's API.
    ResolveImgur(String),
    LoadAbout(String),
    LoadSubs { mine: bool },
    SearchSubs(String),
    /// 1 up, 0 clear, -1 down.
    Vote { fullname: String, dir: i8 },
    Save { fullname: String, on: bool },
    Hide { id: String },
    Subscribe { sub: String, on: bool },
    Reply { parent: String, post: String, text: String },
    /// A post was opened: remember it as read.
    MarkRead(String),
    SetPref(&'static str, String),
    /// Change the user's lists (favourites, filters, history).
    EditPrefs(fn(&mut crate::prefs::Prefs, &str), String),
    /// A typed text arrived for `Typing`.
    Typed(Typing, String),
    SignInStart,
    SignInStop,
    SignOut,
    LoadInbox,
    ClearReadHistory,
    Play { url: String, looping: bool },
    StopVideo,
    PauseVideo,
    SeekVideo(i64),
    /// Play a YouTube video in ytnative (or LG's YouTube app).
    OpenYouTube(String),
    /// Open a reddit.com share link (resolved in the background, then opened).
    OpenShare(String),
}

pub enum RdMsg {
    /// The store changed (a page arrived, the phone sent the settings).
    Changed,
}

#[derive(Clone, Copy)]
pub struct Views<'a> {
    pub store: &'a Store,
    pub now_ms: u32,
}

#[derive(Default)]
pub struct Init;
impl LogicalState for Init {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}

impl Host for RdHost {
    type Arg = RdArg;
    type Fx = RdFx;
    type Msg = RdMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = Init;
    type Memory = ();
}

struct Mount;

impl Mounter<RdHost> for Mount {
    fn mount(
        &mut self,
        _id: InstanceId,
        arg: &RdArg,
        _ret: &ReturnState<u32, ()>,
        cx: &Cx<'_, RdHost>,
        _fx: &mut Effects<'_, RdHost>,
    ) -> Box<dyn Screen<RdHost>> {
        let entry = match cx.owner {
            InputOwner::Entry(e) => e,
            _ => EntryId(0),
        };
        use screens::*;
        match arg {
            RdArg::Home => Box::new(feed::FeedScreen::home(entry, cx)),
            RdArg::Feed(f) => Box::new(feed::FeedScreen::new(entry, f.clone(), cx)),
            RdArg::Post { id, comment } => Box::new(post::PostScreen::new(entry, id.clone(), comment.clone())),
            RdArg::Viewer { id, index } => Box::new(viewer::ViewerScreen::new(entry, id.clone(), *index)),
            RdArg::Link(url) => Box::new(viewer::LinkScreen::new(entry, url.clone())),
            RdArg::Settings => Box::new(settings::SettingsScreen::root(entry)),
            RdArg::SettingsPage(i) => Box::new(settings::SettingsScreen::page(entry, *i)),
            RdArg::Lists => Box::new(settings::ListsScreen::new(entry)),
            RdArg::About => Box::new(settings::SettingsScreen::about(entry)),
            RdArg::ApiSettings => Box::new(api::ApiScreen::new(entry)),
            RdArg::Type(t) => {
                let current = match t {
                    Typing::Api(f) => cx.views.store.draft.get(*f).to_string(),
                    Typing::ImgurId => cx.views.store.prefs.imgur_client_id.clone().unwrap_or_default(),
                    _ => String::new(),
                };
                Box::new(keyboard::KeyboardScreen::new(entry, t.clone(), &current))
            }
            RdArg::Subs => Box::new(subs::SubsScreen::new(entry)),
            RdArg::SignIn => Box::new(signin::SignInScreen::new(entry)),
            RdArg::Inbox => Box::new(subs::InboxScreen::new(entry)),
            RdArg::Menu(k) => Box::new(menu::MenuScreen::new(entry, k.clone(), cx)),
            RdArg::Play { url, title, looping } => Box::new(player::PlayerScreen::new(entry, url.clone(), title.clone(), *looping)),
        }
    }
}

/// Results workers post back to the main thread.
enum Work {
    Feed { epoch: u64, key: FeedKey, more: bool, result: Result<(Vec<Post>, Option<String>), String> },
    Thread { epoch: u64, id: String, sort: CommentSort, result: Result<(Post, Vec<Node>), String> },
    More { epoch: u64, post: String, more_id: String, result: Result<(Vec<Node>, Vec<String>), String> },
    Imgur { id: String, media: Option<Media> },
    About { sub: String, result: Result<SubAbout, String> },
    Subs { mine: bool, query: Option<String>, result: Result<Vec<Subreddit>, String> },
    /// An action went through, or failed with this message.
    Action { undo: Option<Box<dyn FnOnce(&mut Store) + Send>>, error: Option<String>, ok_toast: Option<String> },
    Replied { post: String, result: Result<Option<Node>, String> },
    Me { name: Option<String>, unread: i64 },
    Inbox { result: Result<Vec<Message>, String> },
    SignedIn { result: Result<Saved, String> },
    /// The session's token changed: keep it.
    Saved(Saved),
    /// Open this page (a share link was resolved).
    Navigate(Option<RdArg>),
}

pub struct AppRig {
    mounter: Mount,
    pub store: Store,
    pub now_ms: u32,
    measure: plx_gfx::text::TtfMeasure,
    next_req: u32,
    done: Arc<Mutex<Vec<(Addr, Work)>>>,
    /// Bumped when the API settings or the account change: results of an older session are dropped.
    epoch: u64,
    session: Option<Arc<Session>>,
    phone: Option<rd_phone::PhoneForm>,
    /// The machine that opened the phone form (gets `Changed` when it delivers).
    phone_from: Option<MachineId>,
    /// The sign-in flow's phone page and the nonce it expects.
    signin_phone: Option<rd_phone::PhoneForm>,
    signin_nonce: String,
    /// The Sign in screen: it hears when the sign-in ends (and closes once signed in).
    signin_from: Option<MachineId>,
    /// Notifications made on the main thread, delivered with the next frame.
    pending: Vec<(Addr, RdMsg)>,
    /// Every screen that asked for something gets `Changed` when anything arrives.
    listeners: Vec<MachineId>,
    read_dirty: bool,
    read_saved_ms: u32,
    player: crate::player::Player,
}

impl AppRig {
    pub fn new() -> Self {
        let mut rig = AppRig {
            mounter: Mount,
            store: Store::new(),
            now_ms: 0,
            measure: plx_gfx::text::TtfMeasure,
            next_req: 0,
            done: Default::default(),
            epoch: 0,
            session: None,
            phone: None,
            phone_from: None,
            signin_phone: None,
            signin_nonce: String::new(),
            signin_from: None,
            pending: Vec::new(),
            listeners: Vec::new(),
            read_dirty: false,
            read_saved_ms: 0,
            player: crate::player::Player::new(),
        };
        rig.apply_prefs();
        if rig.store.signed_in() {
            rig.refresh_me();
        }
        rig
    }

    fn apply_prefs(&self) {
        let p = &self.store.prefs;
        p.apply();
        crate::flow::SMOOTH.store(p.on("smooth_scroll"), std::sync::atomic::Ordering::Relaxed);
        crate::EDGE_SPEED.store(
            match p.get("edge_scroll") {
                "off" => 0,
                "slow" => 3,
                "fast" => 12,
                _ => 6,
            },
            std::sync::atomic::Ordering::Relaxed,
        );
        crate::WHEEL_ROWS.store(p.num("wheel_rows").clamp(1, 3) as u8, std::sync::atomic::Ordering::Relaxed);
    }

    fn next_addr(&mut self, to: MachineId) -> Addr {
        self.next_req += 1;
        Addr { to, req: RequestId(self.next_req) }
    }

    fn listen(&mut self, m: MachineId) {
        if !self.listeners.contains(&m) {
            self.listeners.push(m);
            if self.listeners.len() > 32 {
                self.listeners.remove(0);
            }
        }
    }

    fn spawn(&mut self, from: MachineId, name: &'static str, job: impl FnOnce() -> Work + Send + 'static) {
        self.listen(from);
        let addr = self.next_addr(from);
        let done = self.done.clone();
        let spawned = plx_base::task::spawn(name, move || {
            let r = job();
            done.lock().unwrap().push((addr, r));
            plx_machine::present::wake_from_worker();
        });
        if spawned.is_none() {
            crate::log(&format!("{name}: could not start worker thread"));
        }
    }

    /// The Reddit session of the saved settings (made once, shared by every request).
    fn session(&mut self) -> Option<Arc<Session>> {
        if self.session.is_none() {
            let saved = self.store.saved.as_ref()?;
            let s = Session::from_saved(rd_core::session::http_client(), saved, rd_core::ua::host_platform())?;
            self.session = Some(Arc::new(s));
        }
        self.session.clone()
    }

    fn reset_session(&mut self) {
        self.session = None;
        self.epoch += 1;
        self.store.feeds.clear();
        self.store.threads.clear();
        self.store.about.clear();
        self.store.subs_mine = Default::default();
        self.store.bump();
    }

    fn notify_all(&mut self) {
        for m in self.listeners.clone() {
            let a = self.next_addr(m);
            self.pending.push((a, RdMsg::Changed));
        }
    }

    /// Main thread, once per frame: poll the phone forms, move finished worker results into the
    /// store, and return the notifications for the screens that asked.
    pub fn drain(&mut self, now_ms: u32) -> Vec<(Addr, RdMsg)> {
        self.now_ms = now_ms;
        if self.store.toast.as_ref().is_some_and(|(_, until)| now_ms.wrapping_sub(*until) < u32::MAX / 2) {
            self.store.toast = None;
            plx_machine::present::wake_from_worker();
        }
        self.poll_phone();
        self.poll_signin_phone();
        if self.player.active() {
            self.player.tick();
            let st = self.player.status.lock().unwrap().clone();
            if st.position_ms != self.store.play.position_ms || st.phase != self.store.play.phase {
                plx_machine::present::wake_from_worker();
            }
            self.store.play = st;
        }
        if self.read_dirty && now_ms.wrapping_sub(self.read_saved_ms) > 10_000 {
            self.read_dirty = false;
            self.read_saved_ms = now_ms;
            store::store_read(&self.store.read);
        }
        let mut out = std::mem::take(&mut self.pending);
        let done = std::mem::take(&mut *self.done.lock().unwrap());
        let any = !done.is_empty();
        for (addr, w) in done {
            self.take(w);
            out.push((addr, RdMsg::Changed));
        }
        if any {
            self.store.bump();
            // Everyone listening redraws (a vote shows on the feed and the post at once).
            for m in self.listeners.clone() {
                if !out.iter().any(|(a, _)| a.to == m) {
                    let a = self.next_addr(m);
                    out.push((a, RdMsg::Changed));
                }
            }
        }
        out
    }

    fn take(&mut self, w: Work) {
        let now = self.now_ms;
        match w {
            Work::Feed { epoch, key, more, result } => {
                if epoch != self.epoch {
                    return;
                }
                let st = self.store.feeds.entry(key).or_default();
                st.loading = false;
                st.gen = st.gen.wrapping_add(1);
                match result {
                    Ok((posts, after)) => {
                        st.error = None;
                        if !more {
                            st.ids.clear();
                        }
                        // Reddit's stand-in for a withheld NSFW subreddit is not a post to show.
                        let sent = posts.len();
                        let posts: Vec<_> = posts.into_iter().filter(|p| !rd_core::model::is_mature_gate(p)).collect();
                        if !more {
                            st.gated = sent > 0 && posts.is_empty();
                        }
                        st.done = after.is_none() || posts.is_empty();
                        st.after = after;
                        let mut seen: HashSet<String> = st.ids.iter().cloned().collect();
                        let mut new_items = Vec::new();
                        for p in posts {
                            if seen.insert(p.id.clone()) {
                                st.ids.push(p.id.clone());
                                new_items.push(p);
                            }
                        }
                        for p in new_items {
                            let id = p.id.clone();
                            let item = PostItem::new(p);
                            // A post resolved before (Imgur) keeps its resolved media.
                            let keep = self.store.posts.get(&id).filter(|old| !matches!(old.media, Media::Imgur(_))).map(|o| o.media.clone());
                            let mut item = item;
                            if let (Some(m), Media::Imgur(_)) = (keep, &item.media) {
                                item.media = m;
                            }
                            self.store.posts.insert(id, item);
                        }
                    }
                    Err(e) => st.error = Some(e),
                }
                self.resolve_visible_imgur();
            }
            Work::Thread { epoch, id, sort, result } => {
                if epoch != self.epoch {
                    return;
                }
                let st = self.store.threads.entry(id.clone()).or_insert_with(|| new_thread(sort));
                st.loading = false;
                st.gen = st.gen.wrapping_add(1);
                match result {
                    Ok((post, tree)) => {
                        st.error = None;
                        st.sort = sort;
                        st.tree = tree;
                        apply_auto_collapse(st, &self.store.prefs);
                        let old_media = self.store.posts.get(&id).map(|o| o.media.clone());
                        let mut item = PostItem::new(post);
                        if let (Some(m), Media::Imgur(_)) = (old_media, &item.media) {
                            if !matches!(m, Media::Imgur(_)) {
                                item.media = m;
                            }
                        }
                        let imgur = matches!(item.media, Media::Imgur(_));
                        self.store.posts.insert(id.clone(), item);
                        if imgur {
                            self.resolve_imgur(MachineId::Nav, &id);
                        }
                    }
                    Err(e) => st.error = Some(e),
                }
            }
            Work::More { epoch, post, more_id, result } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(st) = self.store.threads.get_mut(&post) {
                    st.loading_more.remove(&more_id);
                    st.gen = st.gen.wrapping_add(1);
                    match result {
                        Ok((things, remaining)) => {
                            comments::splice_more(&mut st.tree, &more_id, things, remaining);
                        }
                        Err(e) => self.store.toast(format!("Couldn't load more comments: {e}"), now),
                    }
                }
            }
            Work::Imgur { id, media } => {
                self.store.imgur_pending.remove(&id);
                if let (Some(m), Some(p)) = (media, self.store.posts.get_mut(&id)) {
                    p.media = m;
                }
                for f in self.store.feeds.values_mut() {
                    if f.ids.contains(&id) {
                        f.gen = f.gen.wrapping_add(1);
                    }
                }
            }
            Work::About { sub, result } => match result {
                Ok(a) => {
                    self.store.about.insert(sub.to_ascii_lowercase(), a);
                }
                Err(e) => crate::log(&format!("about r/{sub}: {e}")),
            },
            Work::Subs { mine, query, result } => {
                let list = match (&query, mine) {
                    (Some(q), _) => {
                        if self.store.subs_search.0 != *q {
                            return;
                        }
                        &mut self.store.subs_search.1
                    }
                    (None, true) => &mut self.store.subs_mine,
                    (None, false) => &mut self.store.subs_popular,
                };
                list.loading = false;
                list.gen = list.gen.wrapping_add(1);
                match result {
                    Ok(mut items) => {
                        if mine {
                            items.sort_by_key(|s| s.display_name.to_ascii_lowercase());
                        }
                        list.items = items;
                        list.error = None;
                    }
                    Err(e) => list.error = Some(e),
                }
            }
            Work::Action { undo, error, ok_toast } => {
                if let Some(e) = error {
                    if let Some(u) = undo {
                        u(&mut self.store);
                    }
                    self.store.toast(e, now);
                } else if let Some(t) = ok_toast {
                    self.store.toast(t, now);
                }
            }
            Work::Replied { post, result } => match result {
                Ok(node) => {
                    if let (Some(st), Some(node)) = (self.store.threads.get_mut(&post), node) {
                        insert_reply(&mut st.tree, node);
                        st.gen = st.gen.wrapping_add(1);
                    }
                    self.store.toast("Reply sent", now);
                }
                Err(e) => self.store.toast(format!("Couldn't send the reply: {e}"), now),
            },
            Work::Me { name, unread } => {
                self.store.unread = unread;
                if let (Some(n), Some(s)) = (name, self.store.saved.as_mut()) {
                    if s.username.as_deref() != Some(&n) {
                        s.username = Some(n);
                        let _ = store::store_saved(s);
                    }
                }
            }
            Work::Inbox { result } => {
                self.store.inbox.loading = false;
                self.store.inbox.gen = self.store.inbox.gen.wrapping_add(1);
                match result {
                    Ok(m) => {
                        self.store.messages = m;
                        self.store.inbox.error = None;
                    }
                    Err(e) => self.store.inbox.error = Some(e),
                }
            }
            Work::SignedIn { result } => {
                if let Some(v) = self.store.signin.as_mut() {
                    v.busy = false;
                }
                match result {
                    Ok(saved) => {
                        let name = saved.username.clone().unwrap_or_default();
                        if let Err(e) = store::store_saved(&saved) {
                            self.store.toast(format!("Couldn't save the account: {e}"), now);
                            return;
                        }
                        self.store.saved = Some(saved);
                        self.signin_stop();
                        self.reset_session();
                        self.store.toast(format!("Signed in as u/{name}"), now);
                        self.refresh_me();
                    }
                    Err(e) => {
                        // A code works once: start a fresh sign-in (new page and address) and
                        // keep the reason on screen.
                        if self.store.signin.is_some() {
                            self.signin_start();
                        }
                        if let Some(v) = self.store.signin.as_mut() {
                            v.error = Some(e);
                        }
                    }
                }
            }
            Work::Navigate(arg) => match arg {
                Some(a) => self.store.nav_request = Some(a),
                None => self.store.toast("Couldn't open that share link", now),
            },
            Work::Saved(s) => {
                if self.store.saved.as_ref().is_some_and(|old| old.client_id == s.client_id && old.access_token != s.access_token) {
                    let _ = store::store_saved(&s);
                    self.store.saved = Some(s);
                }
            }
        }
    }

    /// The phone form: is it still open, was it opened, did it deliver?
    fn poll_phone(&mut self) {
        let Some(p) = &self.phone else { return };
        if let Some(values) = p.take() {
            let get = |k: &str| values.get(k).cloned().unwrap_or_default();
            for f in Field::ALL {
                self.store.draft.set(f, get(f.key()));
            }
            self.close_phone();
            // The page validated with the same rules, so this saves.
            self.save_api(true);
            self.notify_phone_owner();
            return;
        }
        let opened = p.opened();
        if !p.running() {
            self.close_phone();
            self.store.toast("The phone link expired", self.now_ms);
            self.notify_phone_owner();
        } else if let Some(v) = &mut self.store.phone {
            if v.opened != opened {
                v.opened = opened;
                self.notify_phone_owner();
            }
        }
    }

    fn notify_signin_screen(&mut self) {
        if let Some(m) = self.signin_from {
            let a = self.next_addr(m);
            self.pending.push((a, RdMsg::Changed));
        }
    }

    fn notify_phone_owner(&mut self) {
        if let Some(m) = self.phone_from {
            let a = self.next_addr(m);
            self.pending.push((a, RdMsg::Changed));
        }
    }

    fn close_phone(&mut self) {
        self.phone = None;
        self.store.phone = None;
    }

    fn open_phone(&mut self) {
        let mut form = rd_phone::Form::new("rdtnative: Reddit API settings");
        form.intro = config::SETUP_STEPS.iter().map(|s| s.to_string()).collect();
        for f in Field::ALL {
            let s = f.spec(&self.store.draft);
            form.fields.push(
                rd_phone::Field::new(s.key, s.label)
                    .value(&s.value)
                    .placeholder(&s.placeholder)
                    .help(s.help)
                    .kind(if s.url { rd_phone::Kind::Url } else { rd_phone::Kind::Text })
                    .max_len(s.max_len),
            );
        }
        let validate = |v: &rd_phone::Values| {
            config::from_values(|k| v.get(k).cloned().unwrap_or_default())
                .map(|_| ())
                .map_err(|e| e.into_iter().map(|e| (e.field.to_string(), e.message)).collect())
        };
        match rd_phone::PhoneForm::start(form, Box::new(validate)) {
            Ok(p) => {
                crate::log("phone form: open");
                self.store.phone = Some(PhoneView { url: p.url.clone(), opened: false });
                self.phone = Some(p);
            }
            Err(e) => self.store.toast(format!("Couldn't open the phone link: {e}"), self.now_ms),
        }
    }

    /// Validates the draft; when valid, stores it (a new client ID starts a new identity, other
    /// changes keep the tokens) and reloads.
    fn save_api(&mut self, from_phone: bool) {
        match self.store.draft.validate() {
            Err(errs) => {
                self.store.errors = errs;
                self.store.toast("Check the highlighted settings", self.now_ms);
            }
            Ok(cfg) => {
                self.store.errors.clear();
                let saved = merge(self.store.saved.as_ref(), &cfg);
                if let Err(e) = store::store_saved(&saved) {
                    crate::log(&format!("api: save failed: {e}"));
                    self.store.toast(format!("Couldn't save: {e}"), self.now_ms);
                    return;
                }
                self.store.draft = store::Draft::from_saved(Some(&saved));
                self.store.saved = Some(saved);
                self.store.toast(if from_phone { "Received from your phone and saved" } else { "Saved" }, self.now_ms);
                self.reset_session();
                self.notify_all();
            }
        }
    }

    // ------------------------------------------------------------------ Reddit requests

    fn load_feed(&mut self, from: MachineId, key: FeedKey, more: bool) {
        let Some(s) = self.session() else { return };
        let st = self.store.feeds.entry(key.clone()).or_default();
        if st.loading || (more && (st.done || st.after.is_none())) {
            return;
        }
        st.loading = true;
        if !more {
            st.error = None;
        }
        let after = if more { st.after.clone() } else { None };
        let epoch = self.epoch;
        let device = self.store.saved.as_ref().map(|s| s.device_id.clone()).unwrap_or_default();
        self.spawn(from, "feed", move || {
            let (path, q) = key.feed.request(key.sort, key.time, after.as_deref(), 50);
            let q: Vec<(&str, &str)> = q.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let result = s
                .get(&path, &q, Priority::Page)
                .and_then(|r| model::parse_posts(&r.json))
                .map(|l| (l.items, l.after))
                .map_err(|e| e.to_string());
            if let Err(e) = &result {
                crate::log(&format!("feed {path}: {e}"));
            }
            let _ = device;
            Work::Feed { epoch, key, more, result }
        });
        self.save_token_later(from);
    }

    /// After a request, keep a refreshed token on disk (one job that waits for the others).
    fn save_token_later(&mut self, from: MachineId) {
        let Some(s) = self.session.clone() else { return };
        let Some(device) = self.store.saved.as_ref().map(|s| s.device_id.clone()) else { return };
        self.spawn(from, "token", move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            Work::Saved(s.to_saved(&device))
        });
    }

    fn load_thread(&mut self, from: MachineId, id: String, sort: Option<CommentSort>, reload: bool) {
        let Some(s) = self.session() else { return };
        let default_sort = CommentSort::from_key(self.store.prefs.get("comment_sort")).unwrap_or(CommentSort::Confidence);
        let st = self.store.threads.entry(id.clone()).or_insert_with(|| new_thread(default_sort));
        let sort = sort.unwrap_or(st.sort);
        if st.loading || (!reload && !st.tree.is_empty() && st.sort == sort) {
            return;
        }
        st.loading = true;
        st.error = None;
        st.sort = sort;
        let epoch = self.epoch;
        self.spawn(from, "thread", move || {
            let (path, q) = listing::thread_request(&id, sort, None);
            let q: Vec<(&str, &str)> = q.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let result = s
                .get(&path, &q, Priority::Page)
                .and_then(|r| comments::parse_thread(&r.json))
                .map(|t| (t.post, t.comments))
                .map_err(|e| e.to_string());
            Work::Thread { epoch, id, sort, result }
        });
    }

    fn more_comments(&mut self, from: MachineId, post: String, more_id: String) {
        let Some(s) = self.session() else { return };
        let Some(st) = self.store.threads.get_mut(&post) else { return };
        let Some(more) = find_more(&st.tree, &more_id).cloned() else { return };
        if !st.loading_more.insert(more_id.clone()) {
            return;
        }
        let sort = st.sort;
        let epoch = self.epoch;
        self.spawn(from, "more", move || {
            // Focused listings (Reddit blocks /api/morechildren since late October 2026).
            let focused = |id: &str| -> Result<Vec<Node>, String> {
                let (path, q) = comments::focused_request(&post, id, sort.key());
                let q: Vec<(&str, &str)> = q.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                s.get(&path, &q, Priority::Page).and_then(|r| comments::parse_thread(&r.json)).map(|t| t.comments).map_err(|e| e.to_string())
            };
            let wanted = more.children.clone();
            let mut things: Vec<Node> = Vec::new();
            let mut got: HashSet<String> = HashSet::new();
            let mut error = None;
            // A row of replies: one listing of the parent brings them all.
            if let Some(parent) = more.parent_id.strip_prefix("t1_") {
                match focused(parent) {
                    Ok(nodes) => {
                        let (t, g) = comments::take_wanted_replies(nodes, &wanted);
                        things = t;
                        got = g;
                    }
                    Err(e) => error = Some(e),
                }
            }
            // Top-level rows (and whatever the parent's listing left out): one comment at a time.
            let mut asked = 0;
            for id in &wanted {
                if got.contains(id) || asked >= comments::FOCUSED_BATCH || error.is_some() && things.is_empty() && asked > 0 {
                    continue;
                }
                asked += 1;
                match focused(id) {
                    Ok(nodes) => {
                        let mut flat = Vec::new();
                        comments::flatten_nodes(nodes, &mut flat);
                        for n in flat {
                            if let Node::Comment(c) = &n {
                                if !got.insert(c.id.clone()) {
                                    continue;
                                }
                            }
                            things.push(n);
                        }
                        got.insert(id.clone());
                    }
                    Err(e) => {
                        crate::log(&format!("more: {id}: {e}"));
                        error.get_or_insert(e);
                    }
                }
            }
            let remaining: Vec<String> = wanted.into_iter().filter(|w| !got.contains(w)).collect();
            let result = if things.is_empty() { Err(error.unwrap_or_else(|| "nothing came back".into())) } else { Ok((things, remaining)) };
            Work::More { epoch, post, more_id, result }
        });
    }

    fn resolve_imgur(&mut self, from: MachineId, id: &str) {
        let Some(p) = self.store.posts.get(id) else { return };
        let Media::Imgur(r) = &p.media else { return };
        if !self.store.imgur_pending.insert(id.to_string()) {
            return;
        }
        let r = r.clone();
        let url = media::imgur_api(&r);
        let client = self.store.prefs.imgur_id().to_string();
        let id = id.to_string();
        self.spawn(from, "remote-media", move || {
            let http = rd_core::session::http_client();
            let get = |auth: String| {
                http.get(&url)
                    .header("Authorization", auth)
                    .header("User-Agent", rd_core::ua::user_agent(rd_core::ua::host_platform(), None))
                    .send()
                    .ok()
                    .filter(|r| r.status().is_success())
                    .and_then(|r| r.json::<serde_json::Value>().ok())
            };
            let media = match &r {
                media::ImgurRef::Redgifs(_) => redgifs_token(&http).and_then(|t| get(format!("Bearer {t}"))).and_then(|v| media::parse_redgifs(&v)),
                media::ImgurRef::Streamable(_) => http.get(&url).send().ok().filter(|r| r.status().is_success()).and_then(|r| r.json::<serde_json::Value>().ok()).and_then(|v| media::parse_streamable(&v)),
                _ => get(format!("Client-ID {client}")).and_then(|v| media::parse_imgur(&v)),
            };
            if media.is_none() {
                crate::log(&format!("{} {url}: no media", r.service()));
            }
            Work::Imgur { id, media }
        });
    }

    /// Resolves the Imgur posts of loaded feeds (a handful at a time).
    fn resolve_visible_imgur(&mut self) {
        let ids: Vec<String> = self
            .store
            .posts
            .values()
            .filter(|p| matches!(p.media, Media::Imgur(_)) && !self.store.imgur_pending.contains(&p.post.id))
            .map(|p| p.post.id.clone())
            .take(12)
            .collect();
        for id in ids {
            self.resolve_imgur(MachineId::Nav, &id);
        }
    }

    fn load_about(&mut self, from: MachineId, sub: String) {
        let Some(s) = self.session() else { return };
        if self.store.about.contains_key(&sub.to_ascii_lowercase()) {
            return;
        }
        self.spawn(from, "about", move || {
            let result = s
                .get(&format!("/r/{sub}/about"), &[], Priority::Page)
                .map(|r| {
                    let d = &r.json["data"];
                    SubAbout {
                        sub: Subreddit::from_json(d),
                        subscribed: d["user_is_subscriber"].as_bool().unwrap_or(false),
                        active: d["active_user_count"].as_i64().or_else(|| d["accounts_active"].as_i64()).unwrap_or(0),
                    }
                })
                .map_err(|e| e.to_string());
            Work::About { sub, result }
        });
    }

    fn load_subs(&mut self, from: MachineId, mine: bool) {
        let Some(s) = self.session() else { return };
        let list = if mine { &mut self.store.subs_mine } else { &mut self.store.subs_popular };
        if list.loading {
            return;
        }
        list.loading = true;
        self.spawn(from, "subs", move || {
            let mut all = Vec::new();
            let mut after: Option<String> = None;
            let path = if mine { "/subreddits/mine/subscriber" } else { "/subreddits/popular" };
            let result = loop {
                let mut q = vec![("limit", "100")];
                if let Some(a) = &after {
                    q.push(("after", a.as_str()));
                }
                match s.get(path, &q, Priority::Page).and_then(|r| model::parse_subreddits(&r.json)) {
                    Ok(l) => {
                        all.extend(l.items);
                        after = l.after;
                        if after.is_none() || !mine || all.len() >= 1000 {
                            break Ok(all);
                        }
                    }
                    Err(e) => break Err(e.to_string()),
                }
            };
            Work::Subs { mine, query: None, result }
        });
    }

    fn search_subs(&mut self, from: MachineId, q: String) {
        let Some(s) = self.session() else { return };
        self.store.subs_search = (q.clone(), store::SubList { loading: true, ..Default::default() });
        self.spawn(from, "subsearch", move || {
            let result = s
                .get("/subreddits/search", &[("q", q.as_str()), ("limit", "50"), ("include_over_18", "on")], Priority::Page)
                .and_then(|r| model::parse_subreddits(&r.json))
                .map(|l| l.items)
                .map_err(|e| e.to_string());
            Work::Subs { mine: false, query: Some(q), result }
        });
    }

    fn need_sign_in(&mut self) -> bool {
        if self.store.signed_in() {
            return false;
        }
        self.store.toast("Sign in to vote, save, subscribe and reply (Settings \u{203a} Account)", self.now_ms);
        true
    }

    /// Runs a signed-in POST in the background; on failure `undo` puts the store back.
    fn action(
        &mut self,
        from: MachineId,
        req: (&'static str, Vec<(String, String)>),
        undo: Option<Box<dyn FnOnce(&mut Store) + Send>>,
        ok_toast: Option<String>,
    ) {
        let Some(s) = self.session() else { return };
        self.spawn(from, "action", move || {
            let (path, fields) = req;
            let f: Vec<(&str, &str)> = fields.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let error = s.post(path, &f).err().map(|e| e.to_string());
            Work::Action { undo, error, ok_toast }
        });
    }

    fn vote(&mut self, from: MachineId, fullname: String, dir: i8) {
        if self.need_sign_in() {
            return;
        }
        let want = match dir {
            1 => Some(true),
            -1 => Some(false),
            _ => None,
        };
        let undo: Option<Box<dyn FnOnce(&mut Store) + Send>>;
        if let Some(id) = fullname.strip_prefix("t3_") {
            let Some(p) = self.store.posts.get_mut(id) else { return };
            let before = (p.post.likes, p.post.score);
            p.post.score += score_delta(p.post.likes, want);
            p.post.likes = want;
            let id = id.to_string();
            undo = Some(Box::new(move |s: &mut Store| {
                if let Some(p) = s.posts.get_mut(&id) {
                    (p.post.likes, p.post.score) = before;
                }
            }));
        } else {
            let mut found = None;
            for (pid, st) in self.store.threads.iter_mut() {
                if let Some(c) = find_comment(&mut st.tree, &fullname) {
                    let before = (c.likes, c.score);
                    c.score += score_delta(c.likes, want);
                    c.likes = want;
                    st.gen = st.gen.wrapping_add(1);
                    found = Some((pid.clone(), before));
                    break;
                }
            }
            let Some((pid, before)) = found else { return };
            let name = fullname.clone();
            undo = Some(Box::new(move |s: &mut Store| {
                if let Some(st) = s.threads.get_mut(&pid) {
                    if let Some(c) = find_comment(&mut st.tree, &name) {
                        (c.likes, c.score) = before;
                    }
                }
            }));
        }
        self.store.bump();
        self.action(from, listing::vote(&fullname, dir), undo, None);
    }

    fn save_thing(&mut self, from: MachineId, fullname: String, on: bool) {
        if self.need_sign_in() {
            return;
        }
        if let Some(p) = fullname.strip_prefix("t3_").and_then(|id| self.store.posts.get_mut(id)) {
            p.post.saved = on;
        }
        self.store.bump();
        self.action(from, listing::save(&fullname, on), None, Some(if on { "Saved".into() } else { "Removed from saved".into() }));
    }

    fn refresh_me(&mut self) {
        let Some(s) = self.session() else { return };
        if !s.is_signed_in() {
            return;
        }
        self.spawn(MachineId::Nav, "me", move || match s.get("/api/v1/me", &[], Priority::Prefetch) {
            Ok(r) => Work::Me { name: r.json["name"].as_str().map(str::to_string), unread: r.json["inbox_count"].as_i64().unwrap_or(0) },
            Err(e) => {
                crate::log(&format!("me: {e}"));
                Work::Me { name: None, unread: 0 }
            }
        });
    }

    fn load_inbox(&mut self, from: MachineId) {
        let Some(s) = self.session() else { return };
        self.store.inbox.loading = true;
        self.spawn(from, "inbox", move || {
            let result = s
                .get("/message/inbox", &[("limit", "50")], Priority::Page)
                .map(|r| {
                    r.json["data"]["children"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|t| {
                                    let d = &t["data"];
                                    let g = |k: &str| d[k].as_str().unwrap_or_default().to_string();
                                    Message {
                                        name: g("name"),
                                        author: g("author"),
                                        subject: g("subject"),
                                        body: g("body"),
                                        created: d["created_utc"].as_f64().unwrap_or(0.0) as i64,
                                        unread: d["new"].as_bool().unwrap_or(false),
                                        context: d["context"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                                        subreddit: d["subreddit"].as_str().map(str::to_string),
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .map_err(|e| e.to_string());
            Work::Inbox { result }
        });
    }

    // ------------------------------------------------------------------ sign-in

    fn signin_start(&mut self) {
        let Some(saved) = self.store.saved.clone() else {
            self.store.toast("Enter your Reddit API settings first", self.now_ms);
            return;
        };
        let Ok(client) = rd_core::auth::ClientId::parse(&saved.client_id) else { return };
        self.signin_nonce = rd_core::random_token(16);
        let state = rd_core::auth::State { nonce: self.signin_nonce.clone(), tv: None };
        let url = rd_core::auth::authorize_url(&client, saved.redirect(), &state.encode());
        let phone = rd_phone::PhoneForm::start(signin_form(&url), Box::new(signin_validator(self.signin_nonce.clone()))).ok();
        let auto = phone.as_ref().and_then(|p| p.url.rsplit_once('/')).map(|(base, _)| format!("{base}/"));
        self.store.signin = Some(SignInView {
            phone_url: phone.as_ref().map(|p| p.url.clone()),
            auto_redirect: auto,
            busy: false,
            error: None,
        });
        self.signin_phone = phone;
    }

    fn signin_stop(&mut self) {
        self.signin_phone = None;
        self.store.signin = None;
    }

    fn poll_signin_phone(&mut self) {
        let Some(p) = &self.signin_phone else { return };
        if let Some(values) = p.take() {
            let text = values.get("callback").cloned().unwrap_or_default();
            self.signin_phone = None;
            if let Some(v) = self.store.signin.as_mut() {
                v.phone_url = None;
            }
            self.signin_code(&text);
            self.notify_signin_screen();
        } else if !p.running() {
            // The page's lifetime ran out while Sign in is still open: a fresh one (new address).
            self.signin_phone = None;
            let busy = self.store.signin.as_ref().is_some_and(|v| v.busy);
            if self.store.signin.is_some() && !busy {
                self.signin_start();
            } else if let Some(v) = self.store.signin.as_mut() {
                v.phone_url = None;
            }
        }
    }

    /// The user brought back a callback address or a code: trade it for tokens.
    fn signin_code(&mut self, text: &str) {
        if let Err(e) = check_signin_state(text, &self.signin_nonce) {
            if let Some(v) = self.store.signin.as_mut() {
                v.error = Some(e);
            }
            return;
        }
        let code = match rd_core::auth::parse_callback(text) {
            Ok(rd_core::auth::Callback::Code { code, .. }) => code,
            Ok(rd_core::auth::Callback::Denied { error, .. }) => {
                if let Some(v) = self.store.signin.as_mut() {
                    v.error = Some(format!("Reddit said: {error}"));
                }
                return;
            }
            Err(e) => {
                if let Some(v) = self.store.signin.as_mut() {
                    v.error = Some(e);
                }
                return;
            }
        };
        let Some(saved) = self.store.saved.clone() else { return };
        if let Some(v) = self.store.signin.as_mut() {
            v.busy = true;
            v.error = None;
        }
        self.spawn(self.signin_from.unwrap_or(MachineId::Nav), "signin", move || {
            let result = (|| -> Result<Saved, String> {
                let http = rd_core::session::http_client();
                let client = rd_core::auth::ClientId::parse(&saved.client_id)?;
                let ua = saved.user_agent.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| rd_core::ua::user_agent(rd_core::ua::host_platform(), None));
                let t = rd_core::auth::post_token(&http, &ua, &client, &rd_core::auth::Grant::Code { code: &code, redirect_uri: saved.redirect() })
                    .map_err(|e| e.to_string())?;
                let mut next = saved.clone();
                next.refresh_token = t.refresh.clone();
                next.access_token = Some(t.access.clone());
                next.expires_at = t.expires_at;
                if next.refresh_token.is_none() {
                    return Err("Reddit gave no refresh token (ask for a permanent sign-in)".into());
                }
                let s = Session::from_saved(http, &next, rd_core::ua::host_platform()).ok_or("bad client ID")?;
                next.username = Some(s.fetch_username().map_err(|e| e.to_string())?);
                Ok(next)
            })();
            Work::SignedIn { result }
        });
    }

    fn sign_out(&mut self) {
        let Some(saved) = self.store.saved.as_mut() else { return };
        let rt = saved.refresh_token.take();
        saved.username = None;
        saved.access_token = None;
        saved.expires_at = 0;
        let copy = saved.clone();
        let _ = store::store_saved(&copy);
        if let (Some(rt), Ok(client)) = (rt, rd_core::auth::ClientId::parse(&copy.client_id)) {
            let ua = rd_core::ua::user_agent(rd_core::ua::host_platform(), None);
            let _ = plx_base::task::spawn("revoke", move || {
                let _ = rd_core::auth::revoke(&rd_core::session::http_client(), &ua, &client, &rt);
            });
        }
        self.store.unread = 0;
        self.store.messages.clear();
        self.reset_session();
        self.store.toast("Signed out", self.now_ms);
    }

    fn typed(&mut self, from: MachineId, t: Typing, text: String) {
        let text = text.trim().to_string();
        match t {
            Typing::Api(f) => {
                self.store.draft.set(f, text);
                self.store.errors.retain(|e| e.field != f.key());
            }
            Typing::ImgurId => {
                self.store.prefs.imgur_client_id = (!text.is_empty()).then_some(text);
                store::store_prefs(&self.store.prefs);
                // Posts that could not be resolved with the old ID get another try.
                self.store.imgur_pending.clear();
                self.store.toast("Imgur client ID saved", self.now_ms);
            }
            Typing::FilterWord if !text.is_empty() => {
                if !self.store.prefs.filter_words.iter().any(|w| w.eq_ignore_ascii_case(&text)) {
                    self.store.prefs.filter_words.push(text);
                }
                store::store_prefs(&self.store.prefs);
            }
            Typing::FilterSub if !text.is_empty() => {
                let s = text.trim_start_matches("r/").trim_start_matches("/r/").to_string();
                if !self.store.prefs.filter_subs.iter().any(|w| w.eq_ignore_ascii_case(&s)) {
                    self.store.prefs.filter_subs.push(s);
                }
                store::store_prefs(&self.store.prefs);
            }
            Typing::Search { .. } if !text.is_empty() => {
                self.store.prefs.remember_search(&text);
                store::store_prefs(&self.store.prefs);
            }
            Typing::FindSub if !text.is_empty() => self.search_subs(from, text),
            Typing::Reply { parent, post } if !text.is_empty() => self.reply(from, parent, post, text),
            Typing::SignInCode if !text.is_empty() => self.signin_code(&text),
            _ => {}
        }
        self.store.bump();
    }

    fn reply(&mut self, from: MachineId, parent: String, post: String, text: String) {
        if self.need_sign_in() {
            return;
        }
        let Some(s) = self.session() else { return };
        self.spawn(from, "reply", move || {
            let (path, fields) = listing::reply(&parent, &text);
            let f: Vec<(&str, &str)> = fields.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let result = s
                .post(path, &f)
                .map_err(|e| e.to_string())
                .and_then(|r| {
                    let errs = r.json["json"]["errors"].as_array().cloned().unwrap_or_default();
                    if let Some(e) = errs.first() {
                        return Err(e.as_array().and_then(|a| a.get(1)).and_then(|m| m.as_str()).unwrap_or("Reddit refused it").to_string());
                    }
                    let thing = r.json["json"]["data"]["things"][0].clone();
                    Ok(comments::parse_morechildren(&serde_json::json!({ "json": { "data": { "things": [thing] } } })).ok().and_then(|mut v| v.pop()))
                });
            Work::Replied { post, result }
        });
    }
}

/// RedGIFs' temporary token, fetched once and kept for 20 hours.
fn redgifs_token(http: &reqwest::blocking::Client) -> Option<String> {
    static TOKEN: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);
    if let Some((t, at)) = TOKEN.lock().unwrap().clone() {
        if at.elapsed() < std::time::Duration::from_secs(20 * 3600) {
            return Some(t);
        }
    }
    let v: serde_json::Value = http.get(media::REDGIFS_TOKEN_URL).send().ok()?.json().ok()?;
    let t = v["token"].as_str()?.to_string();
    *TOKEN.lock().unwrap() = Some((t.clone(), std::time::Instant::now()));
    Some(t)
}

/// The sign-in page the TV serves while Sign in is open: typed into a computer's browser (a
/// short address) or opened from the QR code on a phone. It links to Reddit's approve page and
/// takes the address Reddit sent the browser to; when the app's redirect URI points at this
/// server, Reddit's redirect lands here and signs in by itself.
fn signin_form(authorize_url: &str) -> rd_phone::Form {
    let mut form = rd_phone::Form::new("rdtnative: sign in");
    form.short = true;
    form.paste_submits = true;
    form.catch_redirect = Some("callback".into());
    form.submit = "Sign in on the TV".into();
    form.steps = vec![
        "Approve rdtnative on Reddit (the button opens a new tab).".into(),
        "Reddit then sends the browser to your app's redirect URI. Copy that whole address (it has code= in it).".into(),
        "Paste it below. Pasting sends it to the TV at once.".into(),
    ];
    form.action = Some(("Approve on Reddit".into(), authorize_url.to_string()));
    form.fields.push(
        rd_phone::Field::new("callback", "Address Reddit sent you to (or the code)")
            .kind(rd_phone::Kind::Url)
            .placeholder("myapp://callback?state=…&code=…")
            .max_len(2048),
    );
    form.notes = vec![
        (
            "Firefox".into(),
            "After you approve, Firefox shows an error page (\"The address wasn't understood\" or \"Unable to connect\"). The address bar holds the address: click it, copy all of it, paste it here.".into(),
        ),
        (
            "Chrome, Edge, Brave".into(),
            "With an http:// or https:// redirect URI, the address bar holds the address after you approve: copy it.\n\nWith another kind (like myapp://callback), Approve seems to do nothing: the browser stays on Reddit's page. The address is in the developer tools: press F12, open Console, and look for the red line \"Failed to launch '…' because the scheme does not have a registered handler\". Right-click the address in it, choose \"Copy link address\" and paste it here. If the Console was not open when you approved, open it and approve again.".into(),
        ),
        (
            "Phone".into(),
            "If the Reddit app opens instead of the browser: come back, press \"Copy link\" and paste it into the browser's address bar (an address typed or pasted there stays in the browser). On Android the button opens Chrome.\n\nAfter you approve, the browser may show an error page or ask to open another app (cancel that). Copy the address from the address bar and paste it here. Some phone browsers show nothing for a redirect URI like myapp://; then a computer (Firefox) is easier.".into(),
        ),
        (
            "Sign in without copying".into(),
            "If your Reddit app's redirect URI is this page's address without the last part (shown on the TV, like http://192.168.1.20:8091/), Reddit sends the browser straight here and the TV signs in by itself.".into(),
        ),
    ];
    form.done = ("Sent to the TV.".into(), "The TV is signing in now; it says when it is done. You can close this page.".into());
    form
}

/// Checks a sign-in page submission: a callback address or code, from this sign-in.
fn signin_validator(nonce: String) -> impl Fn(&rd_phone::Values) -> Result<(), Vec<(String, String)>> + Send + Sync {
    move |v: &rd_phone::Values| {
        let text = v.get("callback").map(String::as_str).unwrap_or("");
        let checked = rd_core::auth::parse_callback(text).map(|_| ()).and_then(|_| check_signin_state(text, &nonce));
        checked.map_err(|e| vec![("callback".to_string(), e)])
    }
}

/// A callback address carries the `state` of the sign-in it belongs to: it must be this one (a
/// bare code has none and is taken as is).
fn check_signin_state(text: &str, nonce: &str) -> Result<(), String> {
    let state = match rd_core::auth::parse_callback(text) {
        Ok(rd_core::auth::Callback::Code { state, .. } | rd_core::auth::Callback::Denied { state, .. }) => state,
        Err(_) => return Ok(()),
    };
    if state.is_empty() || rd_core::auth::State::decode(&state).nonce == nonce {
        Ok(())
    } else {
        Err("This address is from an earlier sign-in. Approve again with the current page.".into())
    }
}

/// Follows a share link's redirects (on www.reddit.com, without the API) to the post it shares.
fn resolve_share(url: &str) -> Option<rd_core::listing::RedditLink> {
    let http = reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(15)).build().ok()?;
    let mut cur = url.to_string();
    for _ in 0..4 {
        let r = http.head(&cur).header("User-Agent", rd_core::ua::user_agent(rd_core::ua::host_platform(), None)).send().ok()?;
        let loc = r.headers().get("location")?.to_str().ok()?.to_string();
        let loc = if loc.starts_with('/') { format!("https://www.reddit.com{loc}") } else { loc };
        if let Some(l) = rd_core::listing::share_target(&loc) {
            return Some(l);
        }
        cur = loc;
    }
    None
}

fn new_thread(sort: CommentSort) -> ThreadState {
    ThreadState { tree: Vec::new(), toggled: HashSet::new(), loading: false, error: None, sort, loading_more: HashSet::new(), gen: 0 }
}

/// Starts deep replies and moderator notes collapsed, as the settings ask.
fn apply_auto_collapse(st: &mut ThreadState, prefs: &crate::prefs::Prefs) {
    let depth = prefs.num("collapse_depth") as u32;
    let mods = prefs.on("collapse_mod");
    fn walk(nodes: &[Node], depth: u32, max: u32, mods: bool, out: &mut HashSet<String>) {
        for n in nodes {
            if let Node::Comment(c) = n {
                let is_mod = c.stickied && (c.distinguished.as_deref() == Some("moderator") || c.author == "AutoModerator");
                let want = (max > 0 && depth >= max && !c.replies.is_empty() && depth == max) || (mods && is_mod);
                // `toggled` flips Reddit's own flag: only toggle what Reddit left open.
                if want && !c.collapsed {
                    out.insert(c.name.clone());
                }
                walk(&c.replies, depth + 1, max, mods, out);
            }
        }
    }
    let mut set = HashSet::new();
    walk(&st.tree, 0, depth, mods, &mut set);
    st.toggled = set;
}

fn score_delta(before: Option<bool>, after: Option<bool>) -> i64 {
    let v = |x: Option<bool>| match x {
        Some(true) => 1,
        Some(false) => -1,
        None => 0,
    };
    v(after) - v(before)
}

pub fn find_comment<'a>(nodes: &'a mut [Node], name: &str) -> Option<&'a mut rd_core::comments::Comment> {
    for n in nodes.iter_mut() {
        if let Node::Comment(c) = n {
            if c.name == name {
                return Some(c);
            }
            if let Some(f) = find_comment(&mut c.replies, name) {
                return Some(f);
            }
        }
    }
    None
}

fn find_more<'a>(nodes: &'a [Node], id: &str) -> Option<&'a rd_core::comments::More> {
    for n in nodes {
        match n {
            Node::More(m) if m.id == id => return Some(m),
            Node::Comment(c) => {
                if let Some(m) = find_more(&c.replies, id) {
                    return Some(m);
                }
            }
            _ => {}
        }
    }
    None
}

/// A fresh reply goes first under its parent (or first in the thread).
fn insert_reply(tree: &mut Vec<Node>, node: Node) {
    let parent = match &node {
        Node::Comment(c) => c.parent_id.clone(),
        Node::More(m) => m.parent_id.clone(),
    };
    if parent.starts_with("t3_") {
        tree.insert(0, node);
        return;
    }
    if let Some(c) = find_comment(tree, &parent) {
        c.replies.insert(0, node);
    }
}

/// The saved account after the user saved `cfg`: a different client ID is a different app, so
/// its tokens and device ID start over; otherwise only the redirect URI and user agent change.
fn merge(old: Option<&Saved>, cfg: &ApiConfig) -> Saved {
    match old {
        Some(o) if o.client_id == cfg.client_id => {
            Saved { redirect_uri: Some(cfg.redirect_uri.clone()), user_agent: cfg.user_agent.clone(), ..o.clone() }
        }
        _ => Saved::from_config(cfg),
    }
}

impl Rig<RdHost> for AppRig {
    fn split(&mut self) -> Split<'_, RdHost> {
        Split { mounter: &mut self.mounter, views: Views { store: &self.store, now_ms: self.now_ms }, measure: &self.measure }
    }
    fn deliver(&mut self, _to: MachineId, _msg: &RdMsg, _p: &CxParts<u32>, _fx: &mut Effects<'_, RdHost>) -> Handled {
        Handled::No
    }
    fn timer(&mut self, _o: MachineId, _id: TimerId, _p: &CxParts<u32>, _fx: &mut Effects<'_, RdHost>) {}
    fn app_fx(&mut self, from: MachineId, fx: RdFx, _p: &CxParts<u32>, _out: &mut Effects<'_, RdHost>) {
        self.listen(from);
        let now = self.now_ms;
        match fx {
            RdFx::Toast(m) => self.store.toast(m, now),
            RdFx::SetField(f, v) => {
                self.store.draft.set(f, v);
                self.store.errors.retain(|e| e.field != f.key());
            }
            RdFx::SaveApi => self.save_api(false),
            RdFx::PhoneStart => {
                if self.phone.is_none() {
                    self.phone_from = Some(from);
                    self.open_phone();
                }
            }
            RdFx::PhoneStop => self.close_phone(),
            RdFx::LoadFeed { key, reload } => {
                let have = self.store.feeds.get(&key).is_some_and(|f| !f.ids.is_empty() || f.loading);
                if reload || !have {
                    self.load_feed(from, key, false);
                }
            }
            RdFx::MoreFeed(key) => self.load_feed(from, key, true),
            RdFx::LoadThread { id, sort, reload } => self.load_thread(from, id, sort, reload),
            RdFx::MoreComments { post, more_id } => self.more_comments(from, post, more_id),
            RdFx::ToggleComment { post, name } => {
                if let Some(st) = self.store.threads.get_mut(&post) {
                    if !st.toggled.remove(&name) {
                        st.toggled.insert(name);
                    }
                    st.gen = st.gen.wrapping_add(1);
                }
            }
            RdFx::ResolveImgur(id) => self.resolve_imgur(from, &id),
            RdFx::LoadAbout(sub) => self.load_about(from, sub),
            RdFx::LoadSubs { mine } => self.load_subs(from, mine),
            RdFx::SearchSubs(q) => self.search_subs(from, q),
            RdFx::Vote { fullname, dir } => self.vote(from, fullname, dir),
            RdFx::Save { fullname, on } => self.save_thing(from, fullname, on),
            RdFx::Hide { id } => {
                if self.need_sign_in() {
                    return;
                }
                for f in self.store.feeds.values_mut() {
                    if let Some(i) = f.ids.iter().position(|x| *x == id) {
                        f.ids.remove(i);
                        f.gen = f.gen.wrapping_add(1);
                    }
                }
                self.action(from, listing::hide(&format!("t3_{id}"), true), None, Some("Hidden".into()));
            }
            RdFx::Subscribe { sub, on } => {
                if self.need_sign_in() {
                    return;
                }
                if let Some(a) = self.store.about.get_mut(&sub.to_ascii_lowercase()) {
                    a.subscribed = on;
                }
                self.store.subs_mine = Default::default();
                self.action(from, listing::subscribe(&sub, on), None, Some(if on { format!("Joined r/{sub}") } else { format!("Left r/{sub}") }));
            }
            RdFx::Reply { parent, post, text } => self.reply(from, parent, post, text),
            RdFx::MarkRead(id) => {
                if self.store.read.add(&id) {
                    self.read_dirty = true;
                }
            }
            RdFx::SetPref(key, value) => {
                if key == "nsfw" && value == "show" && !self.store.prefs.adult_ok {
                    self.store.prefs.adult_ok = true;
                }
                self.store.prefs.set(key, &value);
                store::store_prefs(&self.store.prefs);
                self.apply_prefs();
                // Text sizes changed: cached layouts are for the old ones.
                self.store.bump();
                for f in self.store.feeds.values_mut() {
                    f.gen = f.gen.wrapping_add(1);
                }
            }
            RdFx::EditPrefs(f, arg) => {
                f(&mut self.store.prefs, &arg);
                store::store_prefs(&self.store.prefs);
                for fd in self.store.feeds.values_mut() {
                    fd.gen = fd.gen.wrapping_add(1);
                }
                self.store.bump();
            }
            RdFx::Typed(t, text) => self.typed(from, t, text),
            RdFx::SignInStart => {
                self.signin_from = Some(from);
                self.listen(from);
                self.signin_start();
            }
            RdFx::SignInStop => self.signin_stop(),
            RdFx::SignOut => self.sign_out(),
            RdFx::LoadInbox => self.load_inbox(from),
            RdFx::Play { url, looping } => {
                self.player.start(url, looping);
                self.store.play = self.player.status.lock().unwrap().clone();
            }
            RdFx::StopVideo => {
                self.player.stop();
                self.store.play = Default::default();
            }
            RdFx::PauseVideo => self.player.toggle_pause(),
            RdFx::SeekVideo(d) => self.player.seek_by(d),
            RdFx::OpenShare(url) => {
                self.store.toast("Opening the shared link\u{2026}", now);
                self.spawn(from, "share", move || {
                    use rd_core::listing::RedditLink;
                    Work::Navigate(resolve_share(&url).map(|l| match l {
                        RedditLink::Sub(s) => RdArg::Feed(Feed::Sub(s)),
                        RedditLink::User(u) => RdArg::Feed(Feed::User(u, rd_core::listing::UserTab::Submitted)),
                        RedditLink::Post(id, c) => RdArg::Post { id, comment: c },
                        RedditLink::Share(_) => RdArg::Link(url.clone()),
                    }))
                });
            }
            RdFx::OpenYouTube(id) => {
                self.store.toast("Opening the video in ytnative…", now);
                self.spawn(from, "youtube", move || match crate::player::launch_youtube(&id) {
                    Ok(_) => Work::Action { undo: None, error: None, ok_toast: None },
                    Err(e) => {
                        crate::log(&format!("youtube: {e}"));
                        Work::Action {
                            undo: None,
                            error: Some("Install ytnative (or LG's YouTube app) to play YouTube videos here. Full screen shows a QR code for a phone.".into()),
                            ok_toast: None,
                        }
                    }
                });
            }
            RdFx::ClearReadHistory => {
                self.store.read = Default::default();
                store::store_read(&self.store.read);
                self.store.toast("Read history cleared", now);
            }
        }
        self.store.bump();
        plx_machine::present::wake_from_worker();
    }
    fn log(&mut self, line: &str) {
        plx_base::eventlog::log(line)
    }
    fn prepare(&mut self, b: &mut Budget, present: &mut Present) {
        crate::images::prepare(b, present);
    }
    fn ls2_pump(&mut self) {
        crate::wl::ls2_pump()
    }
    fn opaque_route(&mut self, _video: bool) {}
    fn clear_opaque_region(&mut self) {
        crate::wl::clear_opaque_region()
    }
    fn now_us(&self) -> u64 {
        crate::now_us()
    }
    fn back_at_root(&mut self) {
        plx_platform::tv::home::go_home()
    }
}

/// Whether feed `f` has posts loaded (used by screens deciding to ask).
pub fn feed_loaded(f: Option<&FeedState>) -> bool {
    f.is_some_and(|f| !f.ids.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signin_state_must_match() {
        assert!(super::check_signin_state("myapp://cb?state=abc&code=x", "abc").is_ok());
        assert!(super::check_signin_state("myapp://cb?state=abc.192.168.1.2%3A8091&code=x", "abc").is_ok());
        assert!(super::check_signin_state("myapp://cb?state=old&code=x", "abc").is_err());
        assert!(super::check_signin_state("justacode", "abc").is_ok());
        let v = super::signin_validator("abc".into());
        let one = |t: &str| rd_phone::Values::from([("callback".to_string(), t.to_string())]);
        assert!(v(&one("http://tv/?state=abc&code=x")).is_ok());
        assert!(v(&one("http://tv/?state=zzz&code=x")).is_err());
        assert!(v(&one("")).is_err());
    }

    #[test]
    fn saving_keeps_tokens_unless_the_client_id_changes() {
        let a = ApiConfig::from_fields("abcdefghijklmn", "", "").unwrap();
        let mut old = Saved::from_config(&a);
        old.refresh_token = Some("RT".into());
        let ua = ApiConfig::from_fields("abcdefghijklmn", "", "webos:x.y:v1 (by /u/me)").unwrap();
        let m = merge(Some(&old), &ua);
        assert_eq!((m.refresh_token.as_deref(), m.device_id.as_str()), (Some("RT"), old.device_id.as_str()));
        assert_eq!(m.user_agent.as_deref(), Some("webos:x.y:v1 (by /u/me)"));
        let other = ApiConfig::from_fields("zyxwvutsrqponm", "", "").unwrap();
        let m = merge(Some(&old), &other);
        assert_eq!(m.refresh_token, None);
        assert_ne!(m.device_id, old.device_id);
    }

    #[test]
    fn vote_deltas() {
        assert_eq!(score_delta(None, Some(true)), 1);
        assert_eq!(score_delta(Some(true), Some(false)), -2);
        assert_eq!(score_delta(Some(false), None), 1);
    }
}
