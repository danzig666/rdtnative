//! The feed: a page of posts in the layout the user picked (cards, list, large, compact), with a
//! header of buttons (sort, time range, layout, search, favourite, join, refresh). The start page
//! (`FeedScreen::home`) adds the side menu (Search, Home, Popular, All, Subreddits, the user's
//! pages, favourite subreddits, Settings, Account), which slides in with Left or BACK.
//!
//! OK opens a post; holding OK opens its menu; the colour buttons vote, save and open comments.
//! In two panes Right moves to the preview: the post with its whole thread under it. There Up
//! and Down go through the comments, OK on the post shows its media full screen and on a comment
//! opens or closes it, Right skips to the next thread and Left goes back to the list.
//!
//! The feed keeps the user going: a page that leaves nothing to show after filters fetches the
//! next ones, a failed next page is tried again with Down on the last post (or after the rate
//! limit's wait), a feed older than half an hour is refreshed when the user is near its top (or
//! says how old it is), and each feed of the start page keeps its place while others are shown.

use super::common::{self, Color, State};
use super::ctree::{self, CItem};
use crate::flow::Flow;
use crate::host::{MenuKind, RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::i18n::tr;
use crate::images::{self, Fit};
use crate::richtext;
use crate::store::{FeedKey, PostItem};
use crate::ui::{self, icon::Kind as Icon, ChoiceBox, Row, VList};
use plx_machine::machine::{By, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::listing::{Feed, Sort, Time, UserTab};
use rd_core::media::Media;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

const G_POSTS: GroupId = GroupId(1);
const G_BAR: GroupId = GroupId(2);
const G_NAV: GroupId = GroupId(3);
const G_PREVIEW: GroupId = GroupId(4);
const POST_BASE: u32 = 0x1000_0000;
/// The two-pane layout's preview: the post (item 0), then its comments.
const PREVIEW_ELEM: u32 = 0x2000_0000;
const BAR_BASE: u32 = 0x3000_0000;
const NAV_BASE: u32 = 0x4000_0000;
const CHOICE_BASE: u32 = ui::CHOICE_BASE;

const LEFT: f32 = 80.0;
const NAV_W: f32 = 440.0;
const T_BACK: plx_machine::machine::TimerId = plx_machine::machine::TimerId(41);
/// The preview pane asks for the focused post's comments once focus rests on it.
const T_PREVIEW: plx_machine::machine::TimerId = plx_machine::machine::TimerId(42);
/// A load refused for the rate limit is tried again once Reddit's wait is over: the timer's id
/// carries the page's entry, as each feed page has its own (cancelling a timer cancels every
/// timer with its id, whoever armed it).
fn retry_timer(entry: EntryId) -> plx_machine::machine::TimerId {
    plx_machine::machine::TimerId(0x8000_0000 | entry.0)
}
/// The second BACK on the side menu leaves the app within this window (ytnative's double press);
/// after it a single BACK closes the menu, so it is kept short.
const EXIT_WINDOW_MS: u32 = 700;
/// Width of the list in the two-pane layout.
const SPLIT_W: f32 = 800.0;

/// The room under the last post for the end-of-feed line: the spinner, or two lines of text.
fn footer_h() -> f32 {
    (20.0 + (ui::sz(24) + ui::sz(22)) as f32 * 1.5 + 16.0).max(90.0)
}
/// Fewer posts than this left after the filters: the next page is fetched by itself...
const AUTO_MIN_ITEMS: usize = 8;
/// ...at most this many pages in a row that add nothing to show.
const AUTO_PAGES: u8 = 5;

#[derive(Clone, Debug, PartialEq)]
enum BarItem {
    Setup,
    SignIn,
    Sort,
    Time,
    Layout,
    Search,
    Refresh,
    /// A subreddit's menu: favourite, join, its info page, slideshow (kept off the bar, which
    /// has no room for them beside Play, Home, a time range and the clock).
    Menu,
    /// Slideshow or video channel of this feed.
    Play,
    /// Back to the start page (feeds opened on their own).
    Home,
}

#[derive(Clone, Debug, PartialEq)]
enum NavItem {
    Search,
    Feed(Feed),
    Subs,
    /// The subreddits opened lately, in a menu.
    Recent,
    CustomFeeds,
    Inbox,
    Settings,
    Account,
    /// Switch to another account on this TV (when there is more than one).
    SwitchAccount,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    Split,
    Cards,
    List,
    Large,
    Compact,
    Dense,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    Sort,
    Time,
    Layout,
}

pub struct FeedScreen {
    entry: EntryId,
    root: bool,
    key: FeedKey,
    flow: Flow,
    nav: VList,
    /// Settings, sizes and data the layout was built for.
    sig: Cell<(u32, u32, usize, u64)>,
    layout: Layout,
    nav_open: Cell<bool>,
    nav_t: f32,
    nav_ms: u32,
    focus_req: Cell<Option<u32>>,
    content_last: Cell<u32>,
    back_at: u32,
    exit_armed: bool,
    choice: Option<(Choice, ChoiceBox)>,
    bar_rects: std::cell::RefCell<Vec<Rect>>,
    asked_more_at: Cell<usize>,
    /// Until the user moves focus, focus goes to the first post when posts arrive.
    auto_focus: Cell<bool>,
    /// The post the two-pane preview shows, and since when (its fade-in).
    preview_shown: std::cell::RefCell<(String, u32)>,
    /// Where the preview drew the post's picture, and whose: clicking it opens the media.
    preview_media: std::cell::RefCell<Option<(Rect, String)>>,
    /// The preview pane's column: the post, then its comment rows.
    pv: Flow,
    /// The preview's comment rows (item `i + 1` is row `i`).
    pv_rows: RefCell<Vec<CItem>>,
    /// What the preview was laid out for: (post id, thread gen, store gen).
    pv_sig: RefCell<(String, u32, u32)>,
    /// Comments in the preview whose spoilers were revealed.
    pv_revealed: RefCell<HashSet<String>>,
    /// The feed's `gen` and the store's `resume_gen` last looked at (results only show in the store).
    watched: (Option<u32>, u32),
    /// Posts shown when the feed last changed, and how many pages in a row were fetched by
    /// themselves without adding any.
    shown_n: usize,
    auto_pages: u8,
    /// Per feed of the start page: the focused post and the scroll, for coming back to it.
    memory: HashMap<FeedKey, (u32, f32)>,
    /// A remembered place to put back once the feed is laid out.
    restore: Cell<Option<(u32, f32)>>,
    /// The "Updated … ago" pill, where it was drawn (a click refreshes).
    pill: Cell<Option<Rect>>,
    state: State,
}

fn layout_of(cx: &Cx<'_, RdHost>) -> Layout {
    match cx.views.store.prefs.get("layout") {
        "cards" => Layout::Cards,
        "list" => Layout::List,
        "large" => Layout::Large,
        "compact" => Layout::Compact,
        "dense" => Layout::Dense,
        _ => Layout::Split,
    }
}

fn header_h() -> f32 {
    ui::ls(128.0).max(118.0)
}

fn start_feed(cx: &Cx<'_, RdHost>) -> Feed {
    let s = cx.views.store;
    match s.prefs.get("start_page") {
        "popular" => Feed::Popular,
        "all" => Feed::All,
        "favorites" if !s.prefs.favorites.is_empty() => Feed::Multi(s.prefs.favorites.clone()),
        "last" => s.prefs.last_feed.as_deref().and_then(|c| feed_of_code(c, &s.prefs.favorites, s.username())).unwrap_or(Feed::Front),
        _ => Feed::Front,
    }
}

/// A side-menu feed as kept for the start page "Last feed" (`None`: not one to start with).
fn feed_code(f: &Feed) -> Option<String> {
    Some(match f {
        Feed::Front => "front".into(),
        Feed::Popular => "popular".into(),
        Feed::All => "all".into(),
        Feed::Multi(_) => "favorites".into(),
        Feed::User(_, UserTab::Saved) => "saved".into(),
        Feed::User(_, UserTab::Submitted) => "mine".into(),
        Feed::Sub(s) => format!("r/{s}"),
        _ => return None,
    })
}

/// The feed [`feed_code`] wrote, as it is now: the favourites as they are, the user's own pages
/// only while signed in.
fn feed_of_code(code: &str, favorites: &[String], me: Option<&str>) -> Option<Feed> {
    match code {
        "front" => Some(Feed::Front),
        "popular" => Some(Feed::Popular),
        "all" => Some(Feed::All),
        "favorites" => (!favorites.is_empty()).then(|| Feed::Multi(favorites.to_vec())),
        "saved" => me.map(|u| Feed::User(u.to_string(), UserTab::Saved)),
        "mine" => me.map(|u| Feed::User(u.to_string(), UserTab::Submitted)),
        c => c.strip_prefix("r/").filter(|s| !s.is_empty()).map(|s| Feed::Sub(s.to_string())),
    }
}

/// Focus at `elem` is at the top of a feed: on one of its first three posts, or not on a post.
fn near_top(elem: u32) -> bool {
    !(POST_BASE..PREVIEW_ELEM).contains(&elem) || elem - POST_BASE < 3
}

/// The two-pane layout's preview pane.
fn preview_pane() -> Rect {
    let x0 = LEFT + SPLIT_W + 40.0;
    Rect::new(x0, header_h() + 8.0, 1920.0 - LEFT - x0, 1080.0 - header_h() - 40.0 - common::hint_band())
}

/// The preview's scrolling column, inside the pane's padding.
fn preview_inner() -> Rect {
    let r = preview_pane();
    Rect::new(r.x + PV_PAD, r.y + PV_PAD, r.w - 2.0 * PV_PAD, r.h - 2.0 * PV_PAD)
}

const PV_PAD: f32 = 26.0;

/// A picture or video post: the preview gives the media the whole pane (the comments are under it).
fn media_post(it: &PostItem, thumbs: bool) -> bool {
    thumbs && matches!(it.media, Media::Image(_) | Media::Gallery(_) | Media::Video(_) | Media::Imgur(_) | Media::YouTube { .. })
}

/// The picture the preview shows for a post: (address, width, height).
fn preview_pic(it: &PostItem) -> Option<(String, u32, u32)> {
    rd_core::media::preview(&it.post, 1100).map(|p| (p.url, p.w, p.h)).or_else(|| match &it.media {
        Media::Image(p) => Some((p.url.clone(), p.w, p.h)),
        Media::Gallery(g) => g.first().map(|i| (i.pic.url.clone(), i.pic.w, i.pic.h)),
        Media::Video(v) => v.poster.as_ref().map(|p| (p.url.clone(), p.w, p.h)),
        Media::YouTube { id, .. } => Some((format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"), 480, 360)),
        Media::Link { preview: Some(p), .. } => Some((p.url.clone(), p.w, p.h)),
        _ => None,
    })
}

/// The preview's post part, laid out for a column `w` wide and a pane `view` tall.
struct Head {
    title: Vec<String>,
    /// The picture's height (none: no picture).
    pic_h: Option<f32>,
    /// The post's text (none: no text shown).
    text: Option<std::rc::Rc<richtext::Laid>>,
    link: bool,
    /// Room the whole part takes.
    h: f32,
}

fn head_layout(it: &PostItem, w: f32, view: f32, thumbs: bool) -> Head {
    let ms = ui::sz(21) as f32 * 1.42;
    let ts = ui::sz(32);
    let title = richtext::wrap_title(&it.post.title, w, ts, true, 3);
    let mut y = ms + 6.0 + richtext::title_h(title.len(), ts) + 8.0 + ms + 16.0;
    let (mut pic_h, mut text, mut link) = (None, None, false);
    match preview_pic(it).filter(|_| thumbs) {
        Some((_, pw, ph)) => {
            // A picture or video post fills the pane; a link's picture takes about half of it.
            // (Exactly the pane's height: Down then moves on to the comments at once.)
            let max = if media_post(it, thumbs) { view - y - 18.0 } else { (view - y) * 0.55 };
            let h = if pw > 0 && ph > 0 { (w * ph as f32 / pw as f32).min(max) } else { max.min(480.0) };
            let h = h.max(120.0);
            pic_h = Some(h);
            y += h + 18.0;
        }
        None if !it.post.selftext.trim().is_empty() => {
            let laid = richtext::cached(&format!("prev:{}:{}", it.post.id, w as u32), &it.post.selftext, w, ui::csz(24));
            y += laid.h + 18.0;
            text = Some(laid);
        }
        None => {
            if matches!(it.media, Media::Link { .. }) {
                link = true;
                y += ui::sz(21) as f32 * 1.6;
            }
        }
    }
    // A picture or video post: its comments start under the pane's bottom edge.
    let h = if media_post(it, thumbs) && pic_h.is_some() { y.max(view) } else { y };
    Head { title, pic_h, text, link, h }
}

impl FeedScreen {
    pub fn home(entry: EntryId, cx: &Cx<'_, RdHost>) -> Self {
        let mut s = Self::new(entry, start_feed(cx), cx);
        s.root = true;
        s.state = State("home".into());
        s
    }

    pub fn new(entry: EntryId, feed: Feed, cx: &Cx<'_, RdHost>) -> Self {
        let key = cx.views.store.key_for(feed);
        let layout = layout_of(cx);
        let mut s = FeedScreen {
            entry,
            root: false,
            state: State(format!("feed {:?}", key.feed)),
            key,
            flow: Flow::new(entry, G_POSTS, POST_BASE, Rect::new(LEFT, header_h(), 1920.0 - 2.0 * LEFT, 1080.0 - header_h())),
            nav: VList { entry, group: G_NAV, base: NAV_BASE, rect: Rect::new(24.0, 150.0, NAV_W - 48.0, 900.0), row_h: 64.0, text_size: 28 },
            sig: Cell::new((u32::MAX, 0, 0, 0)),
            layout,
            nav_open: Cell::new(false),
            nav_t: 0.0,
            nav_ms: 0,
            focus_req: Cell::new(None),
            content_last: Cell::new(POST_BASE),
            back_at: 0,
            exit_armed: false,
            choice: None,
            bar_rects: Default::default(),
            asked_more_at: Cell::new(usize::MAX),
            auto_focus: Cell::new(true),
            preview_shown: Default::default(),
            preview_media: Default::default(),
            pv: {
                let mut pv = Flow::new(entry, G_PREVIEW, PREVIEW_ELEM, preview_inner());
                pv.pad_top = 0.0;
                pv.pad_bottom = 20.0;
                pv.row_gap = 14.0;
                pv.margin = 30.0;
                pv
            },
            pv_rows: Default::default(),
            pv_sig: Default::default(),
            pv_revealed: Default::default(),
            watched: (None, cx.views.store.resume_gen),
            shown_n: 0,
            auto_pages: 0,
            memory: HashMap::new(),
            restore: Cell::new(None),
            pill: Cell::new(None),
        };
        s.configure_flow(cx);
        s
    }

    /// Sets the flow's geometry for the current layout and sizes.
    fn configure_flow(&mut self, cx: &Cx<'_, RdHost>) {
        self.layout = layout_of(cx);
        let top = header_h();
        // The key hints' band is under the list, not over its last post.
        let bottom = 1080.0 - common::hint_band();
        let prefs = &cx.views.store.prefs;
        let f = &mut self.flow;
        f.pad_top = 8.0;
        f.margin = 40.0;
        f.footer = footer_h();
        match self.layout {
            Layout::Cards => {
                f.cols = prefs.num("columns").clamp(2, 6) as usize;
                f.rect = Rect::new(LEFT, top, 1920.0 - 2.0 * LEFT, bottom - top);
                f.gap = 28.0;
                f.row_gap = 32.0;
            }
            Layout::List | Layout::Compact | Layout::Dense => {
                f.cols = 1;
                f.rect = Rect::new(LEFT + 40.0, top, 1920.0 - 2.0 * LEFT - 80.0, bottom - top);
                f.gap = 0.0;
                f.row_gap = match self.layout {
                    Layout::Compact => 8.0,
                    Layout::Dense => 2.0,
                    _ => 18.0,
                };
            }
            Layout::Split => {
                f.cols = 1;
                f.rect = Rect::new(LEFT, top, SPLIT_W, bottom - top);
                f.gap = 0.0;
                f.row_gap = 6.0;
                f.margin = 80.0;
            }
            Layout::Large => {
                f.cols = 1;
                let w = 1280.0;
                f.rect = Rect::new((1920.0 - w) / 2.0, top, w, bottom - top);
                f.gap = 0.0;
                f.row_gap = 36.0;
            }
        }
        self.sig.set((u32::MAX, 0, 0, 0));
    }

    fn items<'a>(&self, cx: &Cx<'a, RdHost>) -> Vec<&'a PostItem> {
        cx.views.store.feed_items(&self.key)
    }

    fn signature(&self, cx: &Cx<'_, RdHost>, n: usize) -> (u32, u32, usize, u64) {
        let st = cx.views.store;
        let gen = st.feed(&self.key).map_or(0, |f| f.gen);
        let prefs_sig = (ui::text_scale() * 100.0) as u64
            | ((ui::content_scale() * 100.0) as u64) << 12
            | (ui::font_code(false) as u64) << 24
            | (st.prefs.num("title_lines") as u64) << 32
            | (st.prefs.on("preview_text") as u64) << 36
            | (st.prefs.on("show_thumbs") as u64) << 37
            | (st.read.contains("") as u64) << 38
            | (st.prefs.on("key_hints") as u64) << 39;
        (gen, st.gen, n, prefs_sig)
    }

    /// Rebuilds item heights when the data or the settings changed.
    fn relayout(&self, cx: &Cx<'_, RdHost>) {
        let items = self.items(cx);
        let sig = self.signature(cx, items.len());
        if sig == self.sig.get() {
            return;
        }
        self.sig.set(sig);
        let w = self.flow.col_w();
        let heights: Vec<f32> = items.iter().map(|it| self.item_h(cx, it, w)).collect();
        self.flow.set_heights(&heights);
        // Back on a feed shown before: where the user was.
        if !heights.is_empty() {
            if let Some((elem, scroll)) = self.restore.take() {
                self.flow.jump(scroll);
                if self.flow.index(elem).is_some() {
                    self.focus_req.set(Some(elem));
                    self.auto_focus.set(false);
                }
            }
        }
    }

    fn title_size(&self) -> i32 {
        match self.layout {
            Layout::Cards => ui::sz(match self.flow.cols {
                2 => 32,
                3 => 29,
                4 => 26,
                5 => 24,
                _ => 22,
            }),
            Layout::List => ui::sz(30),
            Layout::Large => ui::sz(36),
            Layout::Compact => ui::sz(28),
            Layout::Split => ui::sz(25),
            Layout::Dense => ui::sz(25),
        }
    }

    fn meta_size(&self) -> i32 {
        // 21 is the smallest meta line that reads from the couch.
        match self.layout {
            Layout::Cards | Layout::Split | Layout::Dense => ui::sz(21),
            _ => ui::sz(23),
        }
    }

    fn thumbs_on(cx: &Cx<'_, RdHost>) -> bool {
        cx.views.store.prefs.on("show_thumbs")
    }

    fn item_h(&self, cx: &Cx<'_, RdHost>, it: &PostItem, w: f32) -> f32 {
        let prefs = &cx.views.store.prefs;
        let lines = prefs.num("title_lines").clamp(1, 4) as usize;
        let ts = self.title_size();
        let ms = self.meta_size();
        let pad = 18.0;
        match self.layout {
            Layout::Cards => {
                let img = if Self::thumbs_on(cx) { (w * 0.6).round() } else { 0.0 };
                let n = richtext::wrap_title(&it.post.title, w - 2.0 * pad, ts, true, lines).len().max(1);
                img + pad + richtext::title_h(lines.max(n), ts) + 8.0 + 2.0 * (ms as f32 * 1.42) + pad
            }
            Layout::List => {
                let thumb = if Self::thumbs_on(cx) { list_thumb_w() * 0.62 } else { 0.0 };
                let tw = w - if Self::thumbs_on(cx) { list_thumb_w() + 28.0 } else { 0.0 } - 2.0 * pad;
                let n = richtext::wrap_title(&it.post.title, tw, ts, true, lines).len().max(1);
                let mut text = richtext::title_h(n, ts) + 10.0 + 2.0 * (ms as f32 * 1.42);
                if prefs.on("preview_text") && !it.preview.is_empty() {
                    text += ui::csz(22) as f32 * 1.42 * 2.0 + 6.0;
                }
                text.max(thumb) + 2.0 * pad
            }
            Layout::Large => {
                let n = richtext::wrap_title(&it.post.title, w - 2.0 * pad, ts, true, 3).len().max(1);
                let media = if !Self::thumbs_on(cx) {
                    0.0
                } else if let Some(pic) = rd_core::media::preview(&it.post, 1280) {
                    if pic.w > 0 && pic.h > 0 {
                        (w * pic.h as f32 / pic.w as f32).clamp(300.0, 680.0)
                    } else {
                        560.0
                    }
                } else if !it.preview.is_empty() && prefs.on("preview_text") {
                    let laid = richtext::cached(&format!("prev:{}", it.post.id), &it.post.selftext, w - 2.0 * pad - 20.0, ui::csz(26));
                    laid.h.min(ui::csz(26) as f32 * 1.42 * 8.0) + 30.0
                } else {
                    0.0
                };
                pad + richtext::title_h(n, ts) + 12.0 + media + 14.0 + 2.0 * (ms as f32 * 1.42) + pad
            }
            Layout::Compact => (ts as f32 * 1.42) + (ms as f32 * 1.42) + 2.0 * 12.0,
            Layout::Dense => ts as f32 * 1.42 + 14.0,
            Layout::Split => {
                let lines = lines.min(2);
                let pic = Self::thumbs_on(cx) && self.pic_for(cx, it, split_thumb().0).is_some();
                let thumb = if pic { split_thumb().1 } else { 0.0 };
                let tw = w - 2.0 * 14.0 - if pic { split_thumb().0 + 18.0 } else { 0.0 };
                let n = richtext::wrap_title(&it.post.title, tw, ts, true, lines).len().max(1);
                (richtext::title_h(n, ts) + 4.0 + ms as f32 * 1.42).max(thumb) + 2.0 * 12.0
            }
        }
    }

    fn bar_items(&self, cx: &Cx<'_, RdHost>) -> Vec<BarItem> {
        let st = cx.views.store;
        if !st.configured() {
            return vec![BarItem::Setup];
        }
        let mut v = Vec::new();
        // A feed that needs a sign-in: a withheld NSFW subreddit, or a sign-in Reddit refused.
        if st.feeds.get(&self.key).is_some_and(|f| (f.gated && !st.signed_in()) || f.auth_failed) {
            v.push(BarItem::SignIn);
        }
        if !self.key.feed.sorts(st.signed_in()).is_empty() {
            v.push(BarItem::Sort);
            if self.key.sort.timed() || (matches!(self.key.feed, Feed::Search { .. }) && self.key.sort == Sort::Top) {
                v.push(BarItem::Time);
            }
        }
        v.push(BarItem::Layout);
        v.push(BarItem::Search);
        v.push(BarItem::Play);
        if self.key.feed.sub().is_some() {
            v.push(BarItem::Menu);
        }
        v.push(BarItem::Refresh);
        if !self.root {
            v.push(BarItem::Home);
        }
        v
    }

    fn bar_label(&self, b: &BarItem) -> (String, Option<Icon>, bool) {
        match b {
            BarItem::Setup => ("Set up Reddit API".into(), Some(Icon::Key), false),
            BarItem::SignIn => ("Sign in".into(), Some(Icon::User), false),
            BarItem::Sort => (self.key.sort.label_in(&self.key.feed).into(), Some(Icon::Trending), false),
            BarItem::Time => (self.key.time.label().into(), Some(Icon::Clock), false),
            BarItem::Layout => (
                match self.layout {
                    Layout::Cards => "Cards",
                    Layout::List => "List",
                    Layout::Large => "Large",
                    Layout::Compact => "Compact",
                    Layout::Split => "Two panes",
                    Layout::Dense => "Dense",
                }
                .into(),
                Some(Icon::List),
                false,
            ),
            BarItem::Search => ("Search".into(), Some(Icon::Search), false),
            BarItem::Refresh => ("Refresh".into(), Some(Icon::Repeat), false),
            BarItem::Menu => ("More".into(), Some(Icon::Info), false),
            BarItem::Play => ("Play".into(), Some(Icon::Play), false),
            BarItem::Home => ("Home".into(), Some(Icon::Home), false),
        }
    }

    fn nav_items(&self, cx: &Cx<'_, RdHost>) -> Vec<NavItem> {
        let st = cx.views.store;
        let mut v = vec![NavItem::Search, NavItem::Feed(Feed::Front), NavItem::Feed(Feed::Popular), NavItem::Feed(Feed::All), NavItem::Subs];
        if !st.prefs.recent_subs.is_empty() {
            v.push(NavItem::Recent);
        }
        if let Some(u) = st.username() {
            v.push(NavItem::CustomFeeds);
            v.push(NavItem::Feed(Feed::User(u.to_string(), UserTab::Saved)));
            v.push(NavItem::Feed(Feed::User(u.to_string(), UserTab::Submitted)));
            v.push(NavItem::Inbox);
        }
        if st.prefs.favorites.len() > 1 {
            v.push(NavItem::Feed(Feed::Multi(st.prefs.favorites.clone())));
        }
        for f in &st.prefs.favorites {
            v.push(NavItem::Feed(Feed::Sub(f.clone())));
        }
        v.push(NavItem::Settings);
        v.push(NavItem::Account);
        if st.accounts.len() > 1 {
            v.push(NavItem::SwitchAccount);
        }
        v
    }

    fn nav_row(&self, item: &NavItem, cx: &Cx<'_, RdHost>) -> Row {
        let st = cx.views.store;
        let active = matches!(item, NavItem::Feed(f) if *f == self.key.feed);
        match item {
            NavItem::Search => Row::new("Search").icon(Icon::Search),
            NavItem::Feed(Feed::Front) => Row::new("Home").icon(Icon::Home).active(active),
            NavItem::Feed(Feed::Popular) => Row::new("Popular").icon(Icon::Trending).active(active),
            NavItem::Feed(Feed::All) => Row::new("All").icon(Icon::Flame).active(active),
            NavItem::Feed(Feed::Multi(_)) => Row::new("All favourites").icon(Icon::Library).active(active),
            NavItem::Feed(Feed::User(_, UserTab::Saved)) => Row::new("Saved").icon(Icon::Star).active(active),
            NavItem::Feed(Feed::User(_, _)) => Row::new("My posts").icon(Icon::User).active(active),
            NavItem::Feed(f) => Row::new(f.title()).icon(Icon::Subs).active(active).raw(),
            NavItem::Subs => Row::new("Subreddits").icon(Icon::Users),
            NavItem::Recent => Row::new("Recent subreddits").icon(Icon::History),
            NavItem::CustomFeeds => Row::new("Custom feeds").icon(Icon::Folder),
            NavItem::Inbox => {
                let r = Row::new("Inbox").icon(Icon::Bell);
                if st.unread > 0 { r.value(st.unread.to_string()) } else { r }
            }
            NavItem::Settings => Row::new("Settings").icon(Icon::Gear),
            NavItem::Account => match st.username() {
                Some(u) => Row::new(format!("u/{u}")).icon(Icon::User).raw(),
                None => Row::new("Sign in").icon(Icon::User),
            },
            NavItem::SwitchAccount => Row::new("Switch account").icon(Icon::Users),
        }
    }

    fn switch_feed(&mut self, feed: Feed, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if feed == self.key.feed {
            self.refresh(fx);
            return;
        }
        self.remember();
        if let Some(code) = feed_code(&feed) {
            fx.push(Fx::App(RdFx::EditPrefs(|p, c| p.last_feed = Some(c.to_string()), code)));
        }
        self.key = cx.views.store.key_for(feed);
        self.reset_view();
        // A feed shown before comes back where the user left it, unless it is out of date and
        // they were near its top: then it is fetched again.
        let st = cx.views.store;
        let place = self.memory.get(&self.key).copied();
        match st.feed(&self.key).filter(|f| !f.ids.is_empty()) {
            Some(fd) if fd.stale(rd_core::now_secs()) && place.is_none_or(|(e, _)| near_top(e)) => self.refresh(fx),
            Some(_) => self.restore.set(place),
            None => {}
        }
        self.load(cx, fx, false);
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// Keeps the current feed's place (its focused post and scroll) for coming back to it.
    fn remember(&mut self) {
        let elem = Some(self.content_last.get()).filter(|e| self.flow.index(*e).is_some()).unwrap_or(POST_BASE);
        self.memory.insert(self.key.clone(), (elem, self.flow.scroll()));
    }

    /// A new feed (or the same one for a new account): from its top, with nothing asked yet.
    fn reset_view(&mut self) {
        self.flow.to_top();
        self.sig.set((u32::MAX, 0, 0, 0));
        self.content_last.set(POST_BASE);
        self.asked_more_at.set(usize::MAX);
        self.auto_focus.set(true);
        self.auto_pages = 0;
        self.shown_n = 0;
        self.watched.0 = None;
        self.restore.set(None);
    }

    /// Loads the feed again from its top.
    fn refresh(&mut self, fx: &mut Effects<'_, RdHost>) {
        self.flow.to_top();
        self.auto_pages = 0;
        fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload: true }));
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// Reacts to what changed in the store (results reach screens only through it): an error
    /// focuses Refresh on an empty feed and arms the rate limit's retry; posts that leave little
    /// to show (or a focus at the end) fetch the next page; after a resume an out-of-date feed
    /// is refreshed when the user is at its top.
    fn watch(&mut self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let st = cx.views.store;
        let Some(fd) = st.feed(&self.key) else { return };
        let resumed = st.resume_gen != self.watched.1;
        if self.watched.0 == Some(fd.gen) && !resumed {
            return;
        }
        self.watched = (Some(fd.gen), st.resume_gen);
        if fd.loading {
            return;
        }
        let n = self.items(cx).len();
        if n > self.shown_n {
            self.auto_pages = 0;
        }
        self.shown_n = n;
        if fd.error.is_some() {
            // Another try is allowed from the end of the list.
            self.asked_more_at.set(usize::MAX);
            if n == 0 && !self.on_nav(cx) && self.choice.is_none() {
                if let Some(i) = self.bar_items(cx).iter().position(|b| *b == BarItem::Refresh) {
                    self.focus_req.set(Some(BAR_BASE + i as u32));
                }
            }
            if let Some((secs, _)) = fd.retry {
                let t = retry_timer(self.entry);
                fx.push(Fx::CancelTimer(t));
                fx.push(Fx::Timer { id: t, after_ms: secs.clamp(1, 120) as u32 * 1000 + 500 });
            }
        } else if !fd.done && !fd.ids.is_empty() && self.auto_pages < AUTO_PAGES {
            let at_end = self.flow.focused(cx).is_some_and(|i| i + self.flow.cols * 3 >= n);
            if n < AUTO_MIN_ITEMS || at_end {
                self.auto_pages += 1;
                fx.push(Fx::App(RdFx::MoreFeed(self.key.clone())));
            }
        }
        if resumed && fd.stale(rd_core::now_secs()) && near_top(self.content_last.get()) {
            self.refresh(fx);
        }
        fx.invalidate(plx_machine::present::Provenance::Lifecycle);
    }

    fn load(&self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>, reload: bool) {
        if !cx.views.store.configured() {
            return;
        }
        fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload }));
        if let Feed::Sub(s) = &self.key.feed {
            fx.push(Fx::App(RdFx::VisitSub(s.clone())));
        }
        if let Some(sub) = self.key.feed.sub() {
            fx.push(Fx::App(RdFx::LoadAbout(sub.to_string())));
        }
    }

    fn set_key(&mut self, key: FeedKey, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if let (Some(slot), true) = (crate::store::sort_slot(&key.feed), cx.views.store.prefs.on("remember_sort")) {
            let v = format!("{slot}|{}:{}", key.sort.key(), key.time.key());
            fx.push(Fx::App(RdFx::EditPrefs(
                |p, a| {
                    if let Some((s, v)) = a.split_once('|') {
                        p.sub_sorts.insert(s.to_string(), v.to_string());
                    }
                },
                v,
            )));
        }
        self.key = key;
        self.flow.to_top();
        self.sig.set((u32::MAX, 0, 0, 0));
        self.asked_more_at.set(usize::MAX);
        self.auto_pages = 0;
        self.focus_req.set(Some(POST_BASE));
        self.load(cx, fx, false);
    }

    fn open_choice(&mut self, c: Choice, anchor: Rect, row: usize, cx: &Cx<'_, RdHost>) {
        let (options, current): (Vec<String>, usize) = match c {
            Choice::Sort => {
                let sorts = self.key.feed.sorts(cx.views.store.signed_in());
                (sorts.iter().map(|s| s.label_in(&self.key.feed).to_string()).collect(), sorts.iter().position(|s| *s == self.key.sort).unwrap_or(0))
            }
            Choice::Time => (Time::ALL.iter().map(|t| t.label().to_string()).collect(), Time::ALL.iter().position(|t| *t == self.key.time).unwrap_or(0)),
            Choice::Layout => {
                let o = crate::prefs::opt("layout").unwrap();
                let cur = cx.views.store.prefs.get("layout");
                (o.values.iter().map(|(_, l)| l.to_string()).collect(), o.values.iter().position(|(v, _)| *v == cur).unwrap_or(0))
            }
        };
        let b = ChoiceBox::new(self.entry, row, options, current, anchor);
        self.focus_req.set(Some(b.list.elem(current)));
        ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.choice = Some((c, b));
    }

    fn close_choice(&mut self) {
        if let Some((_, b)) = self.choice.take() {
            ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            self.focus_req.set(Some(BAR_BASE + b.row as u32));
        }
    }

    fn pick_choice(&mut self, idx: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some((c, _)) = self.choice.as_ref() else { return };
        let c = *c;
        let row = self.choice.as_ref().map_or(0, |(_, b)| b.row);
        if self.choice.take().is_some() {
            ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        // Back to the button that opened it (a new sort moves to the first post instead).
        self.focus_req.set(Some(BAR_BASE + row as u32));
        match c {
            Choice::Sort => {
                let sorts = self.key.feed.sorts(cx.views.store.signed_in());
                if let Some(s) = sorts.get(idx) {
                    let key = FeedKey { sort: *s, ..self.key.clone() };
                    self.set_key(key, cx, fx);
                }
            }
            Choice::Time => {
                if let Some(t) = Time::ALL.get(idx) {
                    let key = FeedKey { time: *t, ..self.key.clone() };
                    self.set_key(key, cx, fx);
                }
            }
            Choice::Layout => {
                if let Some((v, _)) = crate::prefs::opt("layout").unwrap().values.get(idx) {
                    fx.push(Fx::App(RdFx::SetPref("layout", v.to_string())));
                }
            }
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn activate_bar(&mut self, i: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let items = self.bar_items(cx);
        let Some(b) = items.get(i) else { return };
        let anchor = self.bar_rects.borrow().get(i).copied().unwrap_or(Rect::new(1400.0, 40.0, 200.0, 60.0));
        match b {
            BarItem::Setup => fx.push(Fx::Nav(NavOp::Push(RdArg::ApiSettings))),
            BarItem::SignIn => fx.push(Fx::Nav(NavOp::Push(RdArg::SignIn))),
            BarItem::Sort => self.open_choice(Choice::Sort, anchor, i, cx),
            BarItem::Time => self.open_choice(Choice::Time, anchor, i, cx),
            BarItem::Layout => self.open_choice(Choice::Layout, anchor, i, cx),
            BarItem::Search => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::Search { sub: self.key.feed.sub().map(str::to_string) })))),
            BarItem::Refresh => self.refresh(fx),
            BarItem::Menu => {
                if let Some(sub) = self.key.feed.sub() {
                    // Search results in r/x are not r/x's feed: its info page's "Open r/x" opens it.
                    let from_feed = matches!(self.key.feed, Feed::Sub(_));
                    fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Sub(sub.to_string(), from_feed)))));
                }
            }
            BarItem::Play => {
                // From the post focused last (focus is on this button now).
                let start = self.flow.last_focused().and_then(|i| self.items(cx).get(i).map(|it| it.post.id.clone()));
                fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Play(self.key.clone(), start)))));
            }
            // The start page as it was left, under the pages opened since.
            BarItem::Home => fx.push(Fx::Nav(NavOp::PopToArg(RdArg::Home))),
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn activate_nav(&mut self, item: &NavItem, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        match item {
            NavItem::Search => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::Search { sub: None })))),
            NavItem::Feed(f) => {
                self.switch_feed(f.clone(), cx, fx);
                self.nav_open.set(false);
                self.focus_req.set(Some(POST_BASE));
            }
            NavItem::Subs => fx.push(Fx::Nav(NavOp::Push(RdArg::Subs))),
            NavItem::Recent => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::RecentSubs)))),
            NavItem::CustomFeeds => fx.push(Fx::Nav(NavOp::Push(RdArg::CustomFeeds))),
            NavItem::Inbox => fx.push(Fx::Nav(NavOp::Push(RdArg::Inbox))),
            NavItem::Settings => fx.push(Fx::Nav(NavOp::Push(RdArg::Settings))),
            // Signed in: the user's own profile (their name is the row's label). Otherwise the
            // Accounts page when any account is stored, or Sign in.
            NavItem::Account => fx.push(Fx::Nav(NavOp::Push(match cx.views.store.username() {
                Some(u) => RdArg::Profile(u.to_string()),
                None if !cx.views.store.accounts.is_empty() => RdArg::Accounts,
                None => RdArg::SignIn,
            }))),
            NavItem::SwitchAccount => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Accounts)))),
        }
    }

    fn open_post(&self, it: &PostItem, fx: &mut Effects<'_, RdHost>) {
        fx.push(Fx::App(RdFx::MarkRead(it.post.id.clone())));
        fx.push(Fx::Nav(NavOp::Push(RdArg::Post { id: it.post.id.clone(), comment: None })));
    }

    fn focused_post<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a PostItem> {
        let i = self.flow.focused(cx)?;
        self.items(cx).get(i).copied()
    }

    fn on_preview(&self, cx: &Cx<'_, RdHost>) -> bool {
        cx.focus.current.is_some_and(|k| k.entry == self.entry && self.pv.index(k.elem).is_some())
    }

    /// The preview's comment row under focus.
    fn pv_comment(&self, cx: &Cx<'_, RdHost>) -> Option<CItem> {
        let i = self.pv.focused(cx)?.checked_sub(1)?;
        self.pv_rows.borrow().get(i).cloned()
    }

    /// Lays the preview out again when its post, thread or the sizes changed.
    fn relayout_preview(&self, cx: &Cx<'_, RdHost>) {
        if self.layout != Layout::Split {
            return;
        }
        let st = cx.views.store;
        let Some(it) = self.preview_post(cx) else {
            self.pv.set_heights(&[]);
            self.pv_rows.borrow_mut().clear();
            return;
        };
        let thread = st.thread(&it.post.id);
        let sig = (it.post.id.clone(), thread.map_or(0, |t| t.gen), st.gen);
        let inner = preview_inner();
        if *self.pv_sig.borrow() == sig && self.pv.rect == inner && self.pv.len() > 0 {
            return;
        }
        if self.pv_sig.borrow().0 != sig.0 {
            // Another post: from its top, spoilers hidden again.
            self.pv.to_top();
            self.pv_revealed.borrow_mut().clear();
        }
        *self.pv_sig.borrow_mut() = sig;
        let rows = match thread {
            Some(t) => ctree::rows(t, &it.post.author, st.username().unwrap_or_default(), None, &HashSet::new()),
            None => vec![],
        };
        let w = inner.w;
        let mut heights = vec![head_layout(it, w, inner.h, Self::thumbs_on(cx)).h];
        heights.extend(rows.iter().map(|c| ctree::height(c, w)));
        self.pv.set_heights(&heights);
        *self.pv_rows.borrow_mut() = rows;
    }

    /// The post the two-pane preview shows: the one focused last.
    fn preview_post<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a PostItem> {
        let items = self.items(cx);
        let i = self.flow.focused(cx).or_else(|| self.flow.index(self.content_last.get()));
        i.and_then(|i| items.get(i)).or(items.first()).copied()
    }

    /// The post the colour buttons act on: the focused one, or the preview's.
    fn target_post<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a PostItem> {
        self.focused_post(cx).or_else(|| self.on_preview(cx).then(|| self.preview_post(cx)).flatten())
    }

    fn on_nav(&self, cx: &Cx<'_, RdHost>) -> bool {
        cx.focus.current.is_some_and(|k| k.entry == self.entry && (NAV_BASE..CHOICE_BASE).contains(&k.elem))
    }
}

/// The list thumbnail in the two-pane layout.
fn split_thumb() -> (f32, f32) {
    let w = ui::ls(128.0).clamp(96.0, 180.0);
    (w, (w * 0.66).round())
}

fn list_thumb_w() -> f32 {
    ui::ls(300.0).clamp(220.0, 440.0)
}

impl Machine<RdHost> for FeedScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        // A layout picked here or in Settings takes effect at once.
        if layout_of(cx) != self.layout {
            self.configure_flow(cx);
            fx.invalidate(plx_machine::present::Provenance::Input);
        }
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        // The pointer edge glide: scroll by pixels, focus stays on a post in view.
        if let Some(dy) = common::wheel_dy(ev) {
            let (x, _) = ui::pointer();
            if self.layout == Layout::Split && self.pv.contains_x(x) {
                if self.pv.glide(dy) {
                    if let Some(j) = self.pv.focused(cx).and_then(|i| self.pv.keep_in_view(i)) {
                        self.focus_req.set(Some(self.pv.elem(j)));
                    }
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                return Handled::Yes;
            }
            if self.flow.glide(dy) {
                if let Some(j) = self.flow.focused(cx).and_then(|i| self.flow.keep_in_view(i)) {
                    self.focus_req.set(Some(self.flow.elem(j)));
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        // Holding OK on a post: its menu.
        if let Some(e) = common::press_hold(ev, cx) {
            if let Some(i) = self.pv.index(e) {
                let id = self.preview_post(cx).map(|it| it.post.id.clone());
                match (id, i.checked_sub(1).and_then(|r| self.pv_rows.borrow().get(r).cloned())) {
                    (Some(id), Some(CItem::Comment { name, .. })) => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Comment(id, name))))),
                    (Some(id), None) if i == 0 => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Post(id))))),
                    _ => {}
                }
            } else if let Some(i) = self.flow.index(e) {
                if let Some(it) = self.items(cx).get(i) {
                    fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Post(it.post.id.clone())))));
                }
            }
            return Handled::Yes;
        }
        // The drop-down: BACK closes it.
        if self.choice.is_some() && common::back_any(ev) {
            if common::back_down(ev) {
                self.close_choice();
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        if let Some(c) = common::color_key(ev) {
            if !cx.views.store.prefs.on("color_keys") {
                return Handled::Yes;
            }
            // Yellow right after a hide puts the post back (its toast says so).
            if common::undo_hide_first(c, cx, fx) {
                return Handled::Yes;
            }
            if self.pv_comment(cx).is_some() {
                if c != Color::Blue && common::sign_in_first(c, cx, fx) {
                    return Handled::Yes;
                }
                let row = self.pv_comment(cx);
                match c {
                    Color::Green => ctree::vote(row, 1, fx),
                    Color::Red => ctree::vote(row, -1, fx),
                    Color::Yellow => ctree::save(row, fx),
                    Color::Blue => {
                        if let Some(it) = self.preview_post(cx) {
                            fx.push(Fx::Nav(NavOp::Push(RdArg::PostComments { id: it.post.id.clone() })));
                        }
                    }
                }
                return Handled::Yes;
            }
            match self.target_post(cx) {
                Some(it) => {
                    // Signed out, the vote and save buttons offer to sign in.
                    if common::sign_in_first(c, cx, fx) {
                        return Handled::Yes;
                    }
                    let name = it.post.name.clone();
                    match c {
                        Color::Green => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(true) { 0 } else { 1 } })),
                        Color::Red => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(false) { 0 } else { -1 } })),
                        Color::Yellow => fx.push(Fx::App(RdFx::Save { fullname: name, on: !it.post.saved })),
                        Color::Blue => fx.push(Fx::Nav(NavOp::Push(RdArg::PostComments { id: it.post.id.clone() }))),
                    }
                }
                None => fx.push(Fx::App(RdFx::Toast("Select a post first".into()))),
            }
            return Handled::Yes;
        }
        // BACK on the start page opens the side menu. On the side menu, as in ytnative: BACK twice
        // in quick succession leaves the app; a single BACK closes the menu once the window for
        // the second press has passed (T_BACK).
        if common::back_any(ev) && self.root {
            let down = common::back_down(ev);
            if self.on_nav(cx) && self.nav_open.get() {
                if down {
                    let now = cx.views.now_ms;
                    let twice = cx.views.store.prefs.get("back_exit") == "twice";
                    if !twice || (self.back_at != 0 && now.wrapping_sub(self.back_at) < EXIT_WINDOW_MS) {
                        self.exit_armed = true;
                        return Handled::No;
                    }
                    self.exit_armed = false;
                    self.back_at = now;
                    fx.push(Fx::CancelTimer(T_BACK));
                    fx.push(Fx::Timer { id: T_BACK, after_ms: EXIT_WINDOW_MS });
                    return Handled::Yes;
                }
                return if self.exit_armed { Handled::No } else { Handled::Yes };
            }
            if down {
                self.exit_armed = false;
                self.nav_open.set(true);
                let items = self.nav_items(cx);
                let i = items.iter().position(|n| matches!(n, NavItem::Feed(f) if *f == self.key.feed)).unwrap_or(1);
                self.focus_req.set(Some(self.nav.elem(i)));
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        // Down on the last post after the next page failed: try it again.
        if let ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Down, edge: Edge::Down, .. }, .. }) = ev {
            // Any card of the last row (Cards), once it shows whole: a taller one pages first.
            let last_shown = self
                .flow
                .focused(cx)
                .filter(|&i| self.flow.last_row(i))
                .and_then(|i| self.flow.screen_rect(i))
                .is_some_and(|r| r.y + r.h <= self.flow.rect.y + self.flow.rect.h + 4.0);
            if last_shown && cx.views.store.feed(&self.key).is_some_and(|f| f.error.is_some() && !f.loading && !f.done) {
                fx.push(Fx::App(RdFx::MoreFeed(self.key.clone())));
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
        }
        if let ScreenEvent::Input(InputEvent { kind: InputKind::Click { x, y, .. }, .. }) = ev {
            // The "Updated … ago" pill: refresh.
            if !self.nav_open.get() && self.pill.get().is_some_and(|r| *x >= r.x && *x <= r.x + r.w && *y >= r.y && *y <= r.y + r.h) {
                self.refresh(fx);
                return Handled::Yes;
            }
            // Two panes: a click on the preview's picture or video opens it (zoomed or playing).
            if !self.nav_open.get() && self.choice.is_none() && self.layout == Layout::Split {
                let hit = self.preview_media.borrow().as_ref().filter(|(r, _)| *x >= r.x && *x <= r.x + r.w && *y >= r.y && *y <= r.y + r.h).map(|(_, id)| id.clone());
                if let Some(it) = hit.and_then(|id| self.items(cx).into_iter().find(|it| it.post.id == id)) {
                    super::post::open_media(it, cx, fx);
                    return Handled::Yes;
                }
            }
            // The side menu's handle (top left): a click opens the side menu.
            if self.root && !self.nav_open.get() && self.choice.is_none() && *x < LEFT + 45.0 && *y < header_h() {
                self.nav_open.set(true);
                let items = self.nav_items(cx);
                let i = items.iter().position(|n| matches!(n, NavItem::Feed(f) if *f == self.key.feed)).unwrap_or(1);
                self.focus_req.set(Some(self.nav.elem(i)));
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
            if self.nav_open.get() && *x > NAV_W {
                self.nav_open.set(false);
                self.focus_req.set(Some(self.content_last.get()));
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
        }
        match ev {
            ScreenEvent::Mount => {
                self.load(cx, fx, false);
                // Opened again long after it was fetched: fetched again (the user is at its top).
                if cx.views.store.feed(&self.key).is_some_and(|f| !f.ids.is_empty() && f.stale(rd_core::now_secs())) {
                    self.refresh(fx);
                }
            }
            ScreenEvent::Uncover => {
                self.exit_armed = false;
                // A wait for the rate limit armed before is looked at again (a timer may be gone).
                self.watched.0 = None;
                if layout_of(cx) != self.layout || self.sig.get().3 != self.signature(cx, 0).3 {
                    self.configure_flow(cx);
                }
                // The session was reset (signed in or out, new API settings): every feed is new,
                // and the start page leaves a page of an account that is gone.
                let st = cx.views.store;
                if st.feed(&self.key).is_none() {
                    let feed = match &self.key.feed {
                        Feed::User(u, _) if self.root && st.username() != Some(u.as_str()) => start_feed(cx),
                        f => f.clone(),
                    };
                    self.key = st.key_for(feed);
                    self.memory.clear();
                    self.reset_view();
                    self.load(cx, fx, false);
                }
                fx.invalidate(plx_machine::present::Provenance::Nav);
            }
            ScreenEvent::Tick(_) => self.watch(cx, fx),
            ScreenEvent::Timer(t) if *t == retry_timer(self.entry) => {
                if let Some(fd) = cx.views.store.feed(&self.key).filter(|f| f.error.is_some() && !f.loading) {
                    match fd.retry {
                        Some((_, true)) => fx.push(Fx::App(RdFx::MoreFeed(self.key.clone()))),
                        Some((_, false)) => fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload: true })),
                        None => {}
                    }
                }
            }
            ScreenEvent::Timer(T_PREVIEW) => {
                if let Some(it) = self.focused_post(cx) {
                    let id = it.post.id.clone();
                    fx.push(Fx::App(RdFx::PrefetchThread { id: id.clone() }));
                    if matches!(it.media, Media::Imgur(_)) {
                        fx.push(Fx::App(RdFx::ResolveImgur(id)));
                    }
                }
            }
            // A single BACK on the side menu: the window for a second press is over, so it closes.
            ScreenEvent::Timer(T_BACK) => {
                if self.back_at != 0 && self.on_nav(cx) && self.nav_open.get() {
                    self.nav_open.set(false);
                    self.focus_req.set(Some(self.content_last.get()));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                self.back_at = 0;
            }
            ScreenEvent::Async(_, RdMsg::Changed) => {
                // Settings may have changed the layout.
                if layout_of(cx) != self.layout {
                    self.configure_flow(cx);
                }
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if matches!(by, By::Dir | By::Pointer) {
                    self.auto_focus.set(false);
                }
                if let Some(i) = self.flow.index(to.elem) {
                    if self.nav_open.get() && *by == By::Pointer {
                        // Hovering a card behind the open menu: keep the menu.
                        return Handled::Yes;
                    }
                    self.nav_open.set(false);
                    self.content_last.set(to.elem);
                    self.flow.on_focus(i, *by == By::Pointer);
                    if self.layout == Layout::Split {
                        fx.push(Fx::CancelTimer(T_PREVIEW));
                        fx.push(Fx::Timer { id: T_PREVIEW, after_ms: 450 });
                    }
                    // Near the end: the next page.
                    let n = self.flow.len();
                    if i + self.flow.cols * 3 >= n && self.asked_more_at.get() != n {
                        self.asked_more_at.set(n);
                        fx.push(Fx::App(RdFx::MoreFeed(self.key.clone())));
                    }
                } else if (BAR_BASE..NAV_BASE).contains(&to.elem) {
                    self.nav_open.set(false);
                    self.content_last.set(to.elem);
                } else if let Some(i) = self.pv.index(to.elem) {
                    self.nav_open.set(false);
                    self.pv.on_focus(i, *by == By::Pointer);
                    // In the preview: the whole thread, not just the light first load.
                    if let Some(it) = self.preview_post(cx) {
                        fx.push(Fx::App(RdFx::LoadThread { id: it.post.id.clone(), sort: None, reload: false }));
                    }
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Activate(e) => {
                let e = *e;
                if e >= CHOICE_BASE {
                    if let Some((_, b)) = &self.choice {
                        if let Some(i) = b.list.index(e, b.options.len()) {
                            self.pick_choice(i, cx, fx);
                        }
                    }
                } else if e >= NAV_BASE {
                    let items = self.nav_items(cx);
                    if let Some(i) = self.nav.index(e, items.len()) {
                        let item = items[i].clone();
                        self.activate_nav(&item, cx, fx);
                    }
                } else if e >= BAR_BASE {
                    self.activate_bar((e - BAR_BASE) as usize, cx, fx);
                } else if let Some(i) = self.pv.index(e).filter(|&i| i > 0) {
                    self.activate_comment(i - 1, cx, fx);
                } else if e == PREVIEW_ELEM {
                    // The preview: its media full screen (a text post opens).
                    if let Some(it) = self.preview_post(cx) {
                        if matches!(it.media, Media::None) {
                            self.open_post(it, fx);
                        } else {
                            super::post::open_media(it, cx, fx);
                        }
                    }
                } else if let Some(i) = self.flow.index(e) {
                    if let Some(it) = self.items(cx).get(i) {
                        self.open_post(it, fx);
                    }
                }
                return Handled::Yes;
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<RdHost> for FeedScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        if let Some((_, b)) = &self.choice {
            out.push(b.list.spec(b.options.len()));
            return;
        }
        self.relayout(cx);
        let n = self.bar_items(cx).len();
        out.push(GroupSpec {
            id: G_BAR,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(LEFT, 20.0, 1920.0 - 2.0 * LEFT, header_h() - 30.0),
            len: n,
            elem: ElemKind::Control,
        });
        if self.flow.len() > 0 {
            out.push(self.flow.spec());
            self.relayout_preview(cx);
            if self.layout == Layout::Split && self.pv.len() > 0 {
                let mut g = self.pv.spec();
                g.edge = [EdgeRule::Stop; 4];
                out.push(g);
            }
        }
        if self.root {
            let mut nav = self.nav.spec(self.nav_items(cx).len());
            nav.reachable = AxisMask::HORIZONTAL;
            nav.edge = [EdgeRule::Stop; 4];
            out.push(nav);
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if *key >= CHOICE_BASE {
            ui::CHOICE_GROUP
        } else if *key >= NAV_BASE {
            G_NAV
        } else if *key >= BAR_BASE {
            G_BAR
        } else if self.pv.owns(*key) {
            G_PREVIEW
        } else {
            G_POSTS
        })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        if let Some((_, b)) = &self.choice {
            return b.list.neighbour(key, dir, b.options.len());
        }
        if key.elem >= NAV_BASE {
            let n = self.nav_items(cx).len();
            if dir == Dir::Right {
                self.nav_open.set(false);
                let last = self.content_last.get();
                return Step::Move(FocusKey { entry: self.entry, elem: last });
            }
            return self.nav.neighbour(key, dir, n);
        }
        let open_nav = |me: &Self| -> Step<u32> {
            if me.root && cx.views.store.prefs.on("left_sidebar") {
                me.nav_open.set(true);
                let items = me.nav_items(cx);
                let i = items.iter().position(|n| matches!(n, NavItem::Feed(f) if *f == me.key.feed)).unwrap_or(1);
                Step::Move(me.nav.key(i))
            } else {
                Step::Edge
            }
        };
        if let Some(i) = self.pv.index(key.elem) {
            return match dir {
                // Back to the post it shows.
                Dir::Left => match self.flow.index(self.content_last.get()) {
                    Some(i) => Step::Move(self.flow.key(i)),
                    None => Step::Move(self.flow.key(self.flow.visible().start.min(self.flow.len().saturating_sub(1)))),
                },
                // The next thread (from the post: the first comment).
                Dir::Right => {
                    let to = match i.checked_sub(1) {
                        None => (self.pv.len() > 1).then_some(1),
                        Some(r) => ctree::next_sibling(&self.pv_rows.borrow(), r).map(|j| j + 1),
                    };
                    match to {
                        Some(j) => {
                            self.pv.reveal(j);
                            Step::Move(self.pv.key(j))
                        }
                        None => Step::Edge,
                    }
                }
                _ => match self.pv.neighbour(key, dir) {
                    Step::Edge if dir == Dir::Up => Step::Move(FocusKey { entry: self.entry, elem: BAR_BASE + self.bar_items(cx).len().saturating_sub(1) as u32 }),
                    s => s,
                },
            };
        }
        if key.elem >= BAR_BASE {
            let i = (key.elem - BAR_BASE) as usize;
            let n = self.bar_items(cx).len();
            return match dir {
                Dir::Left if i > 0 => Step::Move(FocusKey { entry: self.entry, elem: key.elem - 1 }),
                Dir::Left => open_nav(self),
                Dir::Right if i + 1 < n => Step::Move(FocusKey { entry: self.entry, elem: key.elem + 1 }),
                Dir::Down if self.flow.len() > 0 => {
                    let v = self.flow.visible();
                    let first = v.start.min(self.flow.len() - 1);
                    self.flow.reveal(first);
                    Step::Move(self.flow.key(first))
                }
                _ => Step::Edge,
            };
        }
        match self.flow.neighbour(key, dir) {
            Step::Edge if dir == Dir::Up => {
                self.flow.to_top();
                let n = self.bar_items(cx).len();
                // The bar button nearest above; Sort when there is one.
                let _ = n;
                Step::Move(FocusKey { entry: self.entry, elem: BAR_BASE })
            }
            Step::Edge if dir == Dir::Left => open_nav(self),
            // Two panes: Right goes over to the preview.
            Step::Edge if dir == Dir::Right && self.layout == Layout::Split => Step::Move(FocusKey { entry: self.entry, elem: PREVIEW_ELEM }),
            // Right at the end of a card row goes on to the next row.
            Step::Edge if dir == Dir::Right => match self.flow.index(key.elem) {
                Some(i) if i + 1 < self.flow.len() => {
                    self.flow.reveal(i + 1);
                    Step::Move(self.flow.key(i + 1))
                }
                _ => Step::Edge,
            },
            s => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        if *key >= CHOICE_BASE {
            let (_, b) = self.choice.as_ref()?;
            return b.list.place(*key, b.options.len(), b.list.focused(cx, b.options.len()));
        }
        if *key >= NAV_BASE {
            let n = self.nav_items(cx).len();
            return self.nav.place(*key, n, self.nav.focused(cx, n));
        }
        if *key >= BAR_BASE {
            let r = *self.bar_rects.borrow().get((*key - BAR_BASE) as usize)?;
            return Some(Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(*key - BAR_BASE) });
        }
        if self.pv.owns(*key) {
            return self.pv.place(*key);
        }
        self.flow.place(*key)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let want = match self.focus_req.take() {
            Some(e) => FocusKey { entry: self.entry, elem: e },
            None => want,
        };
        if let Some((_, b)) = &self.choice {
            if b.owns(want.elem) {
                return want;
            }
            return b.list.key(b.current);
        }
        if self.auto_focus.get() && self.flow.len() > 0 && (BAR_BASE..NAV_BASE).contains(&want.elem) {
            self.auto_focus.set(false);
            return self.flow.key(0);
        }
        let valid = if want.elem >= CHOICE_BASE {
            false
        } else if want.elem >= NAV_BASE {
            self.root && self.nav.index(want.elem, self.nav_items(cx).len()).is_some()
        } else if want.elem >= BAR_BASE {
            ((want.elem - BAR_BASE) as usize) < self.bar_items(cx).len()
        } else if self.pv.owns(want.elem) {
            self.layout == Layout::Split && self.flow.len() > 0 && self.pv.index(want.elem).is_some()
        } else {
            self.flow.index(want.elem).is_some()
        };
        if valid && (want.elem < NAV_BASE || self.nav_open.get()) {
            return want;
        }
        if self.flow.len() > 0 {
            let first = self.flow.visible().start.min(self.flow.len() - 1);
            return self.flow.key(first);
        }
        FocusKey { entry: self.entry, elem: BAR_BASE }
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        match g {
            G_POSTS if self.flow.len() > 0 => self.flow.key(self.flow.visible().start.min(self.flow.len() - 1)),
            G_PREVIEW => self.pv.key(0),
            G_NAV => self.nav.key(0),
            _ => FocusKey { entry: self.entry, elem: BAR_BASE },
        }
    }
}

impl FeedScreen {
    fn draw_header(&self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let cx = f.cx;
        let st = cx.views.store;
        let hh = header_h();
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, hh + 10.0));
        let ts = ui::sz(44);
        let title = match &self.key.feed {
            Feed::Front if !st.configured() => "rdtnative".to_string(),
            f => f.title(),
        };
        let x = if self.root { LEFT + 50.0 } else { LEFT };
        if self.root {
            // The side menu's handle.
            ui::icon::draw(p, Icon::List, Rect::new(LEFT - 20.0, 34.0 + (ts as f32 * 1.3 - 40.0) / 2.0, 40.0, 40.0), ui::FAINT, false);
        }
        // Buttons right, title left in the room that is left.
        let items = self.bar_items(cx);
        let labels: Vec<(String, Option<Icon>, bool)> = items.iter().map(|b| self.bar_label(b)).collect();
        let bh = ui::ls(58.0).max(52.0);
        let right = 1920.0 - LEFT + if st.prefs.on("clock") { -130.0 } else { 0.0 };
        let rects = common::button_row(f.measure, &labels.iter().map(|(l, i, _)| (l.as_str(), i.is_some())).collect::<Vec<_>>(), 0.0, 34.0, bh, 14.0, Some(right));
        let bar_left = rects.first().map_or(right, |r| r.x);
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for (i, (r, (l, icon, active))) in rects.iter().zip(labels.iter()).enumerate() {
            common::button(p, f.measure, *r, l, *icon, focus == Some(BAR_BASE + i as u32), *active, false);
        }
        *self.bar_rects.borrow_mut() = rects.clone();
        // Reloading a feed whose posts are on screen: a small spinner under Refresh (a next page
        // has its own at the end of the list).
        if st.feed(&self.key).is_some_and(|f| f.loading && f.reloading) && !self.items(cx).is_empty() {
            if let Some(r) = items.iter().position(|b| *b == BarItem::Refresh).and_then(|i| rects.get(i)) {
                ui::spinner_at(p, cx.views.now_ms, r.x + r.w / 2.0, r.y + r.h + 18.0, 10.0);
            }
        }
        // Cut short rather than painted over the buttons.
        let title_room = (bar_left - x - 30.0).max(0.0);
        ui::text_cut(p, f.measure, &title, x, 30.0, title_room, ts, ui::WHITE, ui::ALIGN_LEFT, true);
        // Under the title: the subreddit's members, or the sort.
        let sub_line = match self.key.feed.sub().and_then(|s| st.about.get(&s.to_ascii_lowercase())) {
            Some(a) => {
                let mut s = format!("{} members", common::short_count(a.sub.subscribers));
                if a.active > 0 {
                    s.push_str(&format!("  \u{b7}  {} online", common::short_count(a.active)));
                }
                if !a.sub.title.is_empty() {
                    s.push_str(&format!("  \u{b7}  {}", a.sub.title));
                }
                s
            }
            None if st.configured() => {
                let mut s = self.key.sort.label_in(&self.key.feed).to_string();
                if self.key.sort.timed() {
                    s.push_str(&format!(" \u{b7} {}", self.key.time.label()));
                }
                s
            }
            None => String::new(),
        };
        if !sub_line.is_empty() {
            ui::text_cut(p, f.measure, &sub_line, x, 30.0 + ts as f32 * 1.3, title_room, ui::sz(22), ui::FAINT, ui::ALIGN_LEFT, false);
        }
        if st.prefs.on("clock") {
            common::clock(p, 1920.0 - LEFT, 40.0);
        }
        for (i, r) in rects.iter().enumerate() {
            if self.choice.is_none() {
                ui::stop(f, FocusKey { entry: self.entry, elem: BAR_BASE + i as u32 }, *r, Rect::new(0.0, 0.0, 1920.0, 1080.0));
            }
        }
    }

    fn pic_for(&self, cx: &Cx<'_, RdHost>, it: &PostItem, w: f32) -> Option<(u16, String)> {
        if !Self::thumbs_on(cx) {
            return None;
        }
        let prefs = &cx.views.store.prefs;
        let blur = (it.post.over_18 && prefs.get("nsfw") == "blur") || (it.post.spoiler && prefs.get("spoilers") == "blur");
        let want = if blur { 140 } else { prefs.pic_width(w) };
        let url = rd_core::media::preview(&it.post, want).map(|p| p.url).or_else(|| match &it.media {
            Media::Image(p) => Some(p.url.clone()),
            Media::Gallery(g) => g.first().map(|i| i.pic.url.clone()),
            Media::Video(v) => v.poster.as_ref().map(|p| p.url.clone()),
            Media::Link { preview: Some(p), .. } => Some(p.url.clone()),
            Media::YouTube { id, .. } => Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
            _ => None,
        })?;
        Some((if blur { images::BLUR } else { images::THUMB }, url))
    }

    /// A post's picture (or a stand-in) in `r`, with its badges.
    fn draw_pic(&self, p: Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, RdHost>, it: &PostItem, r: Rect, rad: f32, alpha: f32) {
        let prefs = &cx.views.store.prefs;
        ui::fill(p, r, rad, ui::surface(0.06));
        let tint = [alpha, alpha, alpha, 1.0];
        let mut drew = false;
        if let Some((srv, url)) = self.pic_for(cx, it, r.w) {
            drew = matches!(images::draw(p, srv, &url, r, rad, Fit::Cover, tint), images::Drawn::At(_));
            if srv == images::BLUR {
                ui::fill(p, r, rad, [0.0, 0.0, 0.0, 0.35]);
                let label = if it.post.over_18 { "NSFW" } else { "SPOILER" };
                let s = ui::sz(26);
                let w = ui::text_width(m, label, s, true) + 40.0;
                let br = Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - s as f32 * 1.8) / 2.0, w, s as f32 * 1.8);
                ui::fill(p, br, br.h / 2.0, if it.post.over_18 { common::NSFW_RED } else { common::SPOILER_GRAY });
                ui::text_raw(p, label, br.x + w / 2.0, br.y + (br.h - s as f32 * 1.3) / 2.0, s, ui::WHITE, ui::ALIGN_CENTER, true);
            }
        }
        if !drew {
            // Text posts: the start of the text; links: the site; others: an icon.
            let pad = 18.0;
            if !it.preview.is_empty() && prefs.on("preview_text") && r.h > 80.0 {
                let size = ui::csz(if r.w < 400.0 { 19 } else { 22 });
                let lines = ((r.h - 2.0 * pad) / (size as f32 * 1.42)).floor().max(1.0) as usize;
                let wrapped = richtext::wrap_title(&it.preview, r.w - 2.0 * pad, size, false, lines);
                ui::fill(p, Rect::new(r.x, r.y, 6.0, r.h), 0.0, ui::with_alpha(ui::accent(), 0.5 * alpha));
                richtext::draw_lines(p, m, &wrapped, r.x + pad + 4.0, r.y + pad, r.w - 2.0 * pad - 4.0, size, [0.85, 0.85, 0.88, alpha], false);
            } else {
                let (icon, label) = match &it.media {
                    Media::Link { url, .. } => (Icon::Leave, rd_core::media::host(url)),
                    Media::Video(_) | Media::YouTube { .. } => (Icon::Play, String::new()),
                    Media::Gallery(_) | Media::Image(_) | Media::Imgur(_) => (Icon::Film, String::new()),
                    Media::None => (Icon::Chat, String::new()),
                };
                let s = (r.h * 0.32).min(90.0);
                ui::icon::draw(p, icon, Rect::new(r.x + (r.w - s) / 2.0, r.y + (r.h - s) / 2.0 - if label.is_empty() { 0.0 } else { 20.0 }, s, s), ui::with_alpha(ui::WHITE, 0.25 * alpha), false);
                if !label.is_empty() {
                    ui::text_cut(p, m, &label, r.x + r.w / 2.0, r.y + r.h / 2.0 + s / 2.0 - 6.0, r.w - 40.0, ui::sz(22), ui::FAINT, ui::ALIGN_CENTER, true);
                }
            }
        }
        // Badges: media kind (bottom right), NSFW / spoiler / pinned (top left).
        let bs = ui::sz(20);
        // A link's site name does not fit a small thumbnail.
        if let Some(b) = common::media_badge(it).filter(|b| r.w >= 240.0 || !b.starts_with('\u{2197}')) {
            let w = ui::text_width(m, &b, bs, true) + bs as f32 * 0.9;
            common::badge(p, m, (r.x + r.w - w - 10.0).max(r.x + 10.0), r.y + r.h - bs as f32 * 1.45 - 10.0, &b, common::BADGE_BG, ui::WHITE, bs);
        }
        let mut bx = r.x + 10.0;
        if it.post.stickied {
            bx += common::badge(p, m, bx, r.y + 10.0, "PINNED", [0.1, 0.55, 0.3, 0.95], ui::WHITE, bs) + 6.0;
        }
        if it.post.over_18 && !(drew && self.pic_for(cx, it, r.w).is_some_and(|(s, _)| s == images::BLUR)) {
            bx += common::badge(p, m, bx, r.y + 10.0, "NSFW", common::NSFW_RED, ui::WHITE, bs) + 6.0;
        }
        if it.post.spoiler && !it.post.over_18 {
            common::badge(p, m, bx, r.y + 10.0, "SPOILER", common::SPOILER_GRAY, ui::WHITE, bs);
        }
    }

    fn draw_item(&self, f: &mut DrawFrame<'_, '_, RdHost>, it: &PostItem, r: Rect, focused: bool) {
        let p = f.painter;
        let m = f.measure;
        let cx = f.cx;
        let prefs = &cx.views.store.prefs;
        let read = prefs.on("dim_read") && cx.views.store.read.contains(&it.post.id) && !focused;
        let alpha = if read { 0.55 } else { 1.0 };
        let ink = [1.0, 1.0, 1.0, alpha];
        let dim = [1.0, 1.0, 1.0, 0.6 * alpha];
        let lines = prefs.num("title_lines").clamp(1, 4) as usize;
        let ts = self.title_size();
        let ms = self.meta_size();
        let pad = 18.0;
        let rad = 18.0;
        let with_sub = self.key.feed.sub().is_none();
        let show_scores = prefs.on("show_scores");
        // Card ground and focus (easing in when focus lands on it).
        let ft = if focused { self.flow.focus_t(cx.views.now_ms) } else { 0.0 };
        let r = if focused && matches!(self.layout, Layout::Cards | Layout::Large) {
            let k = 0.018 * ft;
            Rect::new(r.x - r.w * k / 2.0, r.y - r.h * k / 2.0, r.w * (1.0 + k), r.h * (1.0 + k))
        } else {
            r
        };
        if focused {
            p.shadow(r, rad, 34.0, 10.0, [0.0, 0.0, 0.0, 0.6]);
            ui::fill(p, r, rad, ui::surface(0.17));
        } else if !matches!(self.layout, Layout::Compact | Layout::Dense | Layout::Split) {
            ui::fill(p, r, rad, ui::surface(0.07));
        }
        let meta = common::post_meta(it, cx, with_sub);
        let flair = prefs.on("show_flair").then(|| it.post.link_flair_text.clone()).flatten();
        match self.layout {
            Layout::Cards => {
                let mut y = r.y;
                if Self::thumbs_on(cx) {
                    let img = Rect::new(r.x, r.y, r.w, (r.w * 0.6).round());
                    self.draw_pic(p, m, cx, it, img, rad, alpha);
                    y += img.h;
                }
                y += pad;
                let wrapped = richtext::wrap_title(&it.post.title, r.w - 2.0 * pad, ts, true, lines);
                richtext::draw_lines(p, m, &wrapped, r.x + pad, y, r.w - 2.0 * pad, ts, ink, true);
                y += richtext::title_h(lines.max(wrapped.len()), ts) + 8.0;
                let mut mx = r.x + pad;
                if let Some(fl) = &flair {
                    let w = common::badge(p, m, mx, y + 2.0, fl, ui::with_alpha(ui::accent(), 0.25), ui::WHITE, (ms as f32 * 0.85) as i32);
                    mx += w + 8.0;
                }
                ui::text_cut(p, m, &meta, mx, y, r.x + r.w - pad - mx, ms, dim, ui::ALIGN_LEFT, false);
                y += ms as f32 * 1.42;
                common::score_line(p, m, it, r.x + pad, y, ms, dim, show_scores);
            }
            Layout::List => {
                let mut tx = r.x + pad;
                if Self::thumbs_on(cx) {
                    let tw = list_thumb_w();
                    let th = tw * 0.62;
                    let img = Rect::new(r.x + pad, r.y + (r.h - th) / 2.0, tw, th);
                    self.draw_pic(p, m, cx, it, img, 12.0, alpha);
                    tx = img.x + tw + 28.0;
                }
                let tw = r.x + r.w - pad - tx;
                let mut y = r.y + pad;
                let wrapped = richtext::wrap_title(&it.post.title, tw, ts, true, lines);
                richtext::draw_lines(p, m, &wrapped, tx, y, tw, ts, ink, true);
                y += richtext::title_h(wrapped.len().max(1), ts) + 10.0;
                let mut mx = tx;
                if let Some(fl) = &flair {
                    mx += common::badge(p, m, mx, y + 2.0, fl, ui::with_alpha(ui::accent(), 0.25), ui::WHITE, (ms as f32 * 0.85) as i32) + 8.0;
                }
                ui::text_cut(p, m, &meta, mx, y, tx + tw - mx, ms, dim, ui::ALIGN_LEFT, false);
                y += ms as f32 * 1.42;
                common::score_line(p, m, it, tx, y, ms, dim, show_scores);
                y += ms as f32 * 1.42 + 6.0;
                if prefs.on("preview_text") && !it.preview.is_empty() {
                    let size = ui::csz(22);
                    let wrapped = richtext::wrap_title(&it.preview, tw, size, false, 2);
                    richtext::draw_lines(p, m, &wrapped, tx, y, tw, size, [0.8, 0.8, 0.84, 0.85 * alpha], false);
                }
            }
            Layout::Large => {
                let mut y = r.y + pad;
                let mut mx = r.x + pad;
                if let Some(fl) = &flair {
                    mx += common::badge(p, m, mx, y + 2.0, fl, ui::with_alpha(ui::accent(), 0.25), ui::WHITE, (ms as f32 * 0.85) as i32) + 8.0;
                }
                ui::text_cut(p, m, &meta, mx, y, r.x + r.w - pad - mx, ms, dim, ui::ALIGN_LEFT, false);
                y += ms as f32 * 1.42 + 4.0;
                let wrapped = richtext::wrap_title(&it.post.title, r.w - 2.0 * pad, ts, true, 3);
                richtext::draw_lines(p, m, &wrapped, r.x + pad, y, r.w - 2.0 * pad, ts, ink, true);
                y += richtext::title_h(wrapped.len().max(1), ts) + 12.0;
                let media_h = r.y + r.h - pad - ms as f32 * 1.42 - 14.0 - y;
                if media_h > 20.0 {
                    let mr = Rect::new(r.x, y, r.w, media_h);
                    if self.pic_for(cx, it, r.w).is_some() {
                        self.draw_pic(p, m, cx, it, mr, 0.0, alpha);
                    } else if !it.post.selftext.is_empty() {
                        let laid = richtext::cached(&format!("prev:{}", it.post.id), &it.post.selftext, r.w - 2.0 * pad - 20.0, ui::csz(26));
                        let clip = Rect::new(r.x, y, r.w, media_h);
                        let _scope = plx_ui::screen::ClipScope::open_in(p, clip);
                        richtext::draw(p, &laid, r.x + pad + 10.0, y + 10.0, clip, [0.88, 0.88, 0.9, alpha], false);
                    }
                    y += media_h + 14.0;
                }
                common::score_line(p, m, it, r.x + pad, y, ms, dim, show_scores);
            }
            Layout::Dense => {
                let size = ts;
                let y = r.y + 7.0;
                if !focused {
                    ui::fill(p, Rect::new(r.x, r.y + r.h + 1.0, r.w, 1.0), 0.0, [1.0, 1.0, 1.0, 0.05]);
                }
                let score_w = ui::ls(100.0);
                let mut tx = r.x + 16.0;
                if show_scores {
                    let col = match it.post.likes {
                        Some(true) => ui::UPVOTE,
                        Some(false) => ui::DOWNVOTE,
                        None => dim,
                    };
                    ui::text_raw(p, &common::short_count(it.post.score), r.x + score_w - 16.0, y + 2.0, ms, col, ui::ALIGN_RIGHT, true);
                    tx = r.x + score_w;
                }
                // Right side: subreddit, comments, age.
                let side = format!(
                    "{}{}  \u{b7}  {} \u{1F4AC}  \u{b7}  {}",
                    if with_sub { format!("r/{}", it.post.subreddit) } else { format!("u/{}", it.post.author) },
                    common::media_badge(it).map(|b| format!("  \u{b7}  {b}")).unwrap_or_default(),
                    format!("{}{}", common::short_count(it.post.num_comments), common::grown(it)),
                    rd_core::listing::age(it.post.created_utc, rd_core::now_secs() as i64)
                );
                let side_w = ui::text_width(m, &side, ms, false).min(r.w * 0.42);
                ui::text_cut(p, m, &side, r.x + r.w - 16.0 - side_w, y + 3.0, side_w, ms, dim, ui::ALIGN_LEFT, false);
                let mut title = it.post.title.clone();
                if it.post.over_18 {
                    title = format!("[NSFW] {title}");
                }
                ui::text_cut(p, m, &title, tx, y, r.x + r.w - 40.0 - side_w - tx, size, ink, ui::ALIGN_LEFT, focused);
            }
            Layout::Split => {
                let pad = 14.0;
                let mut tx = r.x + pad;
                // Text posts have no picture: their title takes the row.
                if Self::thumbs_on(cx) && self.pic_for(cx, it, split_thumb().0).is_some() {
                    let (tw, th) = split_thumb();
                    let img = Rect::new(r.x + pad, r.y + (r.h - th) / 2.0, tw, th);
                    self.draw_pic(p, m, cx, it, img, 10.0, alpha);
                    tx = img.x + tw + 18.0;
                }
                let tw = r.x + r.w - pad - tx;
                let lines = lines.min(2);
                let wrapped = richtext::wrap_title(&it.post.title, tw, ts, true, lines);
                let th = richtext::title_h(wrapped.len().max(1), ts) + 4.0 + ms as f32 * 1.42;
                let mut y = r.y + (r.h - th) / 2.0;
                richtext::draw_lines(p, m, &wrapped, tx, y, tw, ts, ink, true);
                y += richtext::title_h(wrapped.len().max(1), ts) + 4.0;
                let mut meta2 = format!(
                    "{}  \u{b7}  {} pts  \u{b7}  {}  \u{b7}  {}",
                    if with_sub { format!("r/{}", it.post.subreddit) } else { format!("u/{}", it.post.author) },
                    common::short_count(it.post.score),
                    common::comments_of(it),
                    rd_core::listing::age(it.post.created_utc, rd_core::now_secs() as i64)
                );
                if it.post.over_18 {
                    meta2 = format!("NSFW  \u{b7}  {meta2}");
                }
                let col = match it.post.likes {
                    Some(true) => ui::with_alpha(ui::UPVOTE, alpha),
                    Some(false) => ui::with_alpha(ui::DOWNVOTE, alpha),
                    None => dim,
                };
                ui::text_cut(p, m, &meta2, tx, y, tw, ms, col, ui::ALIGN_LEFT, false);
            }
            Layout::Compact => {
                let y = r.y + 12.0;
                if focused {
                    // (ground drawn above)
                } else {
                    ui::fill(p, Rect::new(r.x, r.y + r.h + 3.0, r.w, 1.0), 0.0, [1.0, 1.0, 1.0, 0.06]);
                }
                let score_w = ui::ls(110.0);
                if show_scores {
                    let col = match it.post.likes {
                        Some(true) => ui::UPVOTE,
                        Some(false) => ui::DOWNVOTE,
                        None => dim,
                    };
                    ui::text_raw(p, &common::short_count(it.post.score), r.x + score_w - 20.0, y + 4.0, ms, col, ui::ALIGN_RIGHT, true);
                }
                let tx = r.x + if show_scores { score_w } else { pad };
                let tw = r.x + r.w - pad - tx;
                let mut title = it.post.title.clone();
                if it.post.over_18 {
                    title = format!("[NSFW] {title}");
                }
                ui::text_cut(p, m, &title, tx, y, tw, ts, ink, ui::ALIGN_LEFT, true);
                let mut meta2 = meta;
                meta2.push_str(&format!("  \u{b7}  {}", common::comments_of(it)));
                // A link's site is already in the meta line.
                if let Some(b) = common::media_badge(it).filter(|b| !b.starts_with('\u{2197}')) {
                    meta2.push_str(&format!("  \u{b7}  {b}"));
                }
                ui::text_cut(p, m, &meta2, tx, y + ts as f32 * 1.42, tw, ms, dim, ui::ALIGN_LEFT, false);
            }
        }
        if focused {
            let acc = ui::with_alpha(ui::accent(), 0.35 + 0.65 * ft);
            if matches!(self.layout, Layout::Dense | Layout::Split) {
                let h = (r.h - 16.0) * (0.4 + 0.6 * ft);
                ui::fill(p, Rect::new(r.x, r.y + (r.h - h) / 2.0, 6.0, h), 3.0, acc);
            } else {
                p.rring(r, rad, 4.0, acc);
            }
        }
    }

    /// The two-pane layout's right side: the focused post, its picture or text, and the top
    /// comments once they are loaded.
    fn draw_preview(&self, f: &mut DrawFrame<'_, '_, RdHost>, it: &PostItem) {
        let p = f.painter;
        let m = f.measure;
        let cx = f.cx;
        let prefs = &cx.views.store.prefs;
        let now = cx.views.now_ms;
        let pane = preview_pane();
        ui::fill(p, pane, 22.0, ui::surface(0.05));
        let focus = self.pv.focused(cx);
        if focus == Some(0) {
            p.rring(pane, 22.0, 4.0, ui::accent());
        }
        // A new selection fades in (and slides a little), instead of popping.
        {
            let mut shown = self.preview_shown.borrow_mut();
            if shown.0 != it.post.id {
                *shown = (it.post.id.clone(), now);
            }
        }
        let t = if crate::flow::smooth() { now.wrapping_sub(self.preview_shown.borrow().1) as f32 / 180.0 } else { 1.0 };
        if t < 1.0 {
            ui::animating();
        }
        let a = ui::ease_out(t);
        let p = p.alpha(a).translate(0.0, (1.0 - a) * 14.0);
        let _scope = plx_ui::screen::ClipScope::open_in(p, pane);
        *self.preview_media.borrow_mut() = None;
        let inner = self.pv.rect;
        let thumbs = Self::thumbs_on(cx);
        let head = head_layout(it, inner.w, inner.h, thumbs);
        let Some(r0) = self.pv.screen_rect(0) else { return };
        // What scrolls is cut at the padding, out of the pane's rounded edge.
        let inner_clip = plx_ui::screen::ClipScope::open_in(p, Rect::new(pane.x, inner.y - 6.0, pane.w, inner.h + 12.0));
        let (x, w) = (r0.x, r0.w);
        if r0.y + r0.h > pane.y {
            let mut y = r0.y;
            let ms = ui::sz(21);
            let meta = common::post_meta(it, cx, true);
            ui::text_cut(p, m, &meta, x, y, w, ms, ui::DIM, ui::ALIGN_LEFT, false);
            y += ms as f32 * 1.42 + 6.0;
            let ts = ui::sz(32);
            y = richtext::draw_lines(p, m, &head.title, x, y, w, ts, ui::WHITE, true) + 8.0;
            let w_score = common::score_line(p, m, it, x, y, ms, ui::DIM, prefs.on("show_scores"));
            if let (Some(fl), true) = (&it.post.link_flair_text, prefs.on("show_flair")) {
                common::badge(p, m, x + w_score + 20.0, y + 2.0, fl, ui::with_alpha(ui::accent(), 0.3), ui::WHITE, (ms as f32 * 0.85) as i32);
            }
            y += ms as f32 * 1.42 + 16.0;
            let blur = (it.post.over_18 && prefs.get("nsfw") == "blur") || (it.post.spoiler && prefs.get("spoilers") == "blur");
            if let (Some(h), Some((url, _, _))) = (head.pic_h, preview_pic(it)) {
                let r = Rect::new(x, y, w, h);
                *self.preview_media.borrow_mut() = Some((r, it.post.id.clone()));
                ui::fill(p, r, 14.0, [0.0, 0.0, 0.0, 0.4]);
                let drawn = if blur {
                    let small = rd_core::media::preview(&it.post, 140).map_or(url.clone(), |p| p.url);
                    images::draw(p, images::BLUR, &small, r, 14.0, Fit::Cover, ui::WHITE)
                } else {
                    // The list's copy shows at once; the sharper one replaces it when it arrives.
                    // A sharp copy that failed keeps the list's copy rather than an empty box.
                    match images::draw(p, images::FULL, &url, r, 14.0, Fit::Contain, ui::WHITE) {
                        full @ (images::Drawn::Loading | images::Drawn::Failed) => {
                            let small = rd_core::media::preview(&it.post, prefs.pic_width(split_thumb().0)).map_or(url.clone(), |p| p.url);
                            match images::draw(p, images::THUMB, &small, r, 14.0, Fit::Contain, ui::WHITE) {
                                images::Drawn::Loading if matches!(full, images::Drawn::Failed) => images::Drawn::Failed,
                                d => d,
                            }
                        }
                        d => d,
                    }
                };
                if let images::Drawn::Loading = drawn {
                    ui::spinner(p, now, r.x + r.w / 2.0, r.y + r.h / 2.0);
                }
                if let Some(b) = common::media_badge(it) {
                    let bs = ui::sz(20);
                    common::badge(p, m, r.x + 12.0, r.y + r.h - bs as f32 * 1.45 - 12.0, &b, common::BADGE_BG, ui::WHITE, bs);
                }
                if blur {
                    common::badge(p, m, r.x + 12.0, r.y + 12.0, if it.post.over_18 { "NSFW" } else { "SPOILER" }, common::NSFW_RED, ui::WHITE, ui::sz(20));
                }
            } else if let Some(laid) = &head.text {
                richtext::draw(p, laid, x, y, pane, [0.9, 0.9, 0.92, 1.0], false);
            } else if head.link {
                if let Media::Link { url, .. } = &it.media {
                    ui::text_cut(p, m, url, x, y, w, ui::sz(21), ui::accent(), ui::ALIGN_LEFT, false);
                }
            }
        }
        // The comments under the post.
        let thread = cx.views.store.thread(&it.post.id);
        let rows = self.pv_rows.borrow();
        if rows.is_empty() {
            let y = r0.y + r0.h + 40.0;
            if thread.is_some_and(|t| t.loading) && y < pane.y + pane.h - 20.0 {
                ui::spinner(p, now, x + w / 2.0, y);
            }
        } else {
            let sep = r0.y + r0.h - 8.0;
            if sep > pane.y && sep < pane.y + pane.h {
                ui::fill(p, Rect::new(x, sep, w, 1.5), 0.0, [1.0, 1.0, 1.0, 0.1]);
            }
        }
        for i in self.pv.visible() {
            let (Some(r), Some(c)) = (self.pv.screen_rect(i), i.checked_sub(1).and_then(|j| rows.get(j))) else { continue };
            let look = ctree::Look {
                focus: (focus == Some(i)).then(|| self.pv.focus_t(now)),
                glow: 0.0,
                target: false,
                revealed: matches!(c, CItem::Comment { name, .. } if self.pv_revealed.borrow().contains(name)),
                loading: matches!(c, CItem::More { id, .. } if thread.is_some_and(|t| t.loading_more.contains(id))),
                clip: pane,
                now_ms: now,
            };
            ctree::draw(p, m, prefs, c, r, &look);
        }
        drop(inner_clip);
        // More under the bottom edge (a picture post's comments): a faint arrow says so.
        if self.pv.len() > 1 && self.pv.scroll() < self.pv.max_scroll() - 4.0 {
            ui::text(p, "\u{25BC}", pane.x + pane.w / 2.0, pane.y + pane.h - 23.0, 18, ui::FAINT, ui::ALIGN_CENTER, false);
        }
    }

    /// OK on comment row `r` of the preview: reveal its spoilers, open or close it, or load more.
    fn activate_comment(&self, r: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some(id) = self.preview_post(cx).map(|it| it.post.id.clone()) else { return };
        let row = self.pv_rows.borrow().get(r).cloned();
        match row {
            Some(CItem::Comment { name, body, .. }) => {
                if body.contains(">!") && !self.pv_revealed.borrow().contains(&name) {
                    self.pv_revealed.borrow_mut().insert(name);
                } else {
                    fx.push(Fx::App(RdFx::ToggleComment { post: id, name }));
                }
            }
            Some(CItem::More { id: more_id, cont, parent, .. }) => {
                if cont {
                    // "Continue this thread": that comment's thread on the post page.
                    let c = parent.trim_start_matches("t1_").to_string();
                    fx.push(Fx::Nav(NavOp::Push(RdArg::Post { id, comment: Some(c) })));
                } else {
                    fx.push(Fx::App(RdFx::MoreComments { post: id, more_id }));
                }
            }
            None => {}
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// "Updated 2h ago · Refresh" over the top of a list that is out of date (a click refreshes).
    fn draw_pill(&self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let now = rd_core::now_secs();
        let Some(fd) = f.cx.views.store.feed(&self.key).filter(|fd| fd.stale(now) && !fd.loading && !fd.ids.is_empty() && self.choice.is_none()) else {
            self.pill.set(None);
            return;
        };
        let age = rd_core::listing::age(fd.fetched_at as i64, now as i64);
        let text = format!("{}  \u{b7}  {}", tr("Updated {} ago").replace("{}", &age), tr("Refresh"));
        let size = ui::sz(22);
        let h = size as f32 * 1.9;
        let w = ui::text_width(f.measure, &text, size, true) + h;
        let r = Rect::new(self.flow.rect.x + (self.flow.rect.w - w) / 2.0, header_h() + 10.0, w, h);
        let p = f.painter;
        p.shadow(r, h / 2.0, 20.0, 6.0, [0.0, 0.0, 0.0, 0.5]);
        ui::fill(p, r, h / 2.0, ui::with_alpha(ui::accent(), 0.92));
        ui::text_raw(p, &text, r.x + r.w / 2.0, r.y + (h - size as f32 * 1.3) / 2.0, size, ui::WHITE, ui::ALIGN_CENTER, true);
        self.pill.set(Some(r));
    }

    fn draw_nav(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        if !self.root {
            return;
        }
        let cx = f.cx;
        let open = self.nav_open.get() && self.on_nav(cx);
        let now = cx.views.now_ms;
        let dt = now.wrapping_sub(self.nav_ms).min(100) as f32;
        self.nav_ms = now;
        let target = if open { 1.0 } else { 0.0 };
        self.nav_t += (target - self.nav_t) * (1.0 - (-dt / 60.0).exp());
        if (target - self.nav_t).abs() < 0.004 {
            self.nav_t = target;
        } else {
            ui::animating();
        }
        let t = self.nav_t;
        if t <= 0.0 {
            return;
        }
        let p = f.painter;
        ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.5 * t]);
        let p = p.translate(-NAV_W * (1.0 - t), 0.0);
        let bg = ui::surface(0.05);
        p.shadow(Rect::new(0.0, 0.0, NAV_W, 1080.0), 0.0, 40.0, 0.0, [0.0, 0.0, 0.0, 0.6 * t]);
        ui::fill(p, Rect::new(0.0, 0.0, NAV_W, 1080.0), 0.0, bg);
        common::logo(p, 44.0, 52.0);
        let items = self.nav_items(cx);
        let rows: Vec<Row> = items.iter().map(|i| self.nav_row(i, cx)).collect();
        let mut nav = self.nav;
        nav.text_size = ui::sz(27);
        nav.row_h = ui::ls(62.0).max(54.0);
        self.nav = nav;
        let focus = self.nav.focused(cx, rows.len());
        // Favourites under a small heading.
        self.nav.draw(p, f.measure, &rows, focus);
        if t > 0.9 {
            self.nav.stops(f, rows.len());
        }
    }
}

impl Screen<RdHost> for FeedScreen {
    fn name(&self) -> &'static str {
        "feed"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let cx = f.cx;
        let st = cx.views.store;
        if layout_of(cx) != self.layout {
            self.configure_flow(cx);
        }
        self.relayout(cx);
        self.flow.animate(cx.views.now_ms);
        let p = f.painter;
        let area = Rect::new(LEFT, header_h(), 1920.0 - 2.0 * LEFT, 1080.0 - header_h());
        let feed = st.feed(&self.key);
        let items = self.items(cx);
        let stops_before = f.stops().len();
        if !st.configured() {
            common::message(
                p,
                f.measure,
                area,
                "Welcome to rdtnative",
                "rdtnative talks to Reddit with your own Reddit app. Choose \u{201c}Set up Reddit API\u{201d} above and enter its client ID: type it with the remote, or scan a QR code and paste it on your phone.",
            );
        } else if items.is_empty() {
            match feed {
                Some(fd) if fd.loading || (fd.ids.is_empty() && fd.error.is_none() && !fd.done) => ui::spinner(p, cx.views.now_ms, 960.0, 500.0),
                Some(fd) if fd.error.is_some() => {
                    let detail = format!("{}  {}", fd.error.as_deref().unwrap_or_default(), tr("OK on Refresh tries again."));
                    common::message(p, f.measure, area, "Couldn't load this feed", &detail);
                }
                Some(fd) if fd.gated && !st.signed_in() => common::message(
                    p,
                    f.measure,
                    area,
                    "Sign in to see this subreddit",
                    "This is an NSFW (18+) subreddit. Reddit shows its posts to apps only for a signed-in account that allows mature content. Choose \u{201c}Sign in\u{201d} above.",
                ),
                Some(fd) if fd.gated => common::message(
                    p,
                    f.measure,
                    area,
                    "Reddit withholds this subreddit",
                    "This is an NSFW (18+) subreddit. Turn on \u{201c}Show mature content\u{201d} in your Reddit account's preferences (on reddit.com), then refresh.",
                ),
                Some(fd) if !fd.ids.is_empty() => common::message(
                    p,
                    f.measure,
                    area,
                    "Everything here is filtered out",
                    tr("Your filters, NSFW or read-post settings hide every post of this feed (Settings \u{203a} Content)."),
                ),
                Some(_) => match &self.key.feed {
                    Feed::Search { q, .. } => common::message(p, f.measure, area, &format!("{} \u{201c}{q}\u{201d}", tr("No results for")), tr("Try other words, or another sort.")),
                    _ => common::message(p, f.measure, area, "Nothing here", "This feed has no posts."),
                },
                None => ui::spinner(p, cx.views.now_ms, 960.0, 500.0),
            }
        } else {
            let focus = self.flow.focused(cx);
            let range = self.flow.visible();
            {
                let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(0.0, header_h() - 2.0, 1920.0, 1082.0 - header_h()));
                // The focused card last, so its shadow and ring lie over its neighbours.
                for i in range.clone() {
                    if Some(i) == focus {
                        continue;
                    }
                    if let (Some(r), Some(it)) = (self.flow.screen_rect(i), items.get(i)) {
                        self.draw_item(f, it, r, false);
                    }
                }
                if let Some(i) = focus.filter(|i| range.contains(i)) {
                    if let (Some(r), Some(it)) = (self.flow.screen_rect(i), items.get(i)) {
                        self.draw_item(f, it, r, true);
                    }
                }
            }
            // The end of the feed: loading more, or the end (in the room the list keeps for it).
            if let Some(end) = self.flow.end_y() {
                let y = end + self.flow.pad_bottom;
                if y + footer_h() <= self.flow.rect.y + self.flow.rect.h + 1.0 {
                    match feed {
                        Some(fd) if fd.loading => ui::spinner(p, cx.views.now_ms, 960.0, y + 50.0),
                        Some(fd) if fd.error.is_some() => {
                            let mid = self.flow.rect.x + self.flow.rect.w / 2.0;
                            ui::text(p, "Couldn't load more \u{b7} press Down to try again", mid, y + 20.0, ui::sz(24), ui::DIM, ui::ALIGN_CENTER, true);
                            ui::text_cut(p, f.measure, fd.error.as_deref().unwrap_or_default(), mid, y + 20.0 + ui::sz(24) as f32 * 1.5, self.flow.rect.w, ui::sz(22), ui::FAINT, ui::ALIGN_CENTER, false);
                        }
                        Some(fd) if fd.done => {
                            ui::text(p, "You have reached the end", 960.0, y + 20.0, ui::sz(24), ui::FAINT, ui::ALIGN_CENTER, false);
                        }
                        _ => {}
                    }
                }
            }
            if self.choice.is_none() && !self.nav_open.get() {
                self.flow.stops(f);
            }
            if self.layout == Layout::Split {
                self.pv.rect = preview_inner();
                self.relayout_preview(cx);
                self.pv.animate(cx.views.now_ms);
                if let Some(it) = self.preview_post(cx) {
                    self.draw_preview(f, it);
                    if self.choice.is_none() && !self.nav_open.get() {
                        self.pv.stops(f);
                    }
                }
            }
            if focus.is_some() || self.on_preview(cx) {
                common::key_hints(f.painter, f.measure, cx, 1080.0, common::FEED_HINTS);
            }
        }
        self.draw_pill(f);
        if self.nav_open.get() {
            f.drop_stops_since(stops_before);
        }
        self.draw_header(f);
        self.draw_nav(f);
        if let Some((_, b)) = &self.choice {
            ui::fill(f.painter, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.35]);
            b.draw(f);
        }
        common::toast(f.painter, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_feed_comes_back() {
        let favs = vec!["pics".to_string(), "aww".to_string()];
        for f in [Feed::Front, Feed::Popular, Feed::All, Feed::Sub("Pics".into()), Feed::Multi(favs.clone())] {
            let code = feed_code(&f).unwrap();
            assert_eq!(feed_of_code(&code, &favs, None), Some(f), "{code}");
        }
        let saved = Feed::User("me".into(), UserTab::Saved);
        assert_eq!(feed_of_code(&feed_code(&saved).unwrap(), &favs, Some("me")), Some(saved.clone()));
        // Signed out, or the favourites gone: not a feed to start with.
        assert_eq!(feed_of_code(&feed_code(&saved).unwrap(), &favs, None), None);
        assert_eq!(feed_of_code("favorites", &[], None), None);
        assert_eq!(feed_of_code("r/", &favs, None), None);
        assert_eq!(feed_code(&Feed::Search { q: "cats".into(), sub: None }), None);
    }

    #[test]
    fn near_the_top_is_the_first_three_posts_or_off_the_list() {
        assert!(near_top(POST_BASE) && near_top(POST_BASE + 2));
        assert!(!near_top(POST_BASE + 3));
        assert!(near_top(BAR_BASE + 1) && near_top(PREVIEW_ELEM));
    }
}
