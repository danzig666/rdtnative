//! The feed: a page of posts in the layout the user picked (cards, list, large, compact), with a
//! header of buttons (sort, time range, layout, search, favourite, join, refresh). The start page
//! (`FeedScreen::home`) adds the side menu (Search, Home, Popular, All, Subreddits, the user's
//! pages, favourite subreddits, Settings, Account), which slides in with Left or BACK.
//!
//! OK opens a post; holding OK opens its menu; the colour buttons vote, save and open comments.

use super::common::{self, Color, State};
use crate::flow::Flow;
use crate::host::{MenuKind, RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::images::{self, Fit};
use crate::richtext;
use crate::store::{FeedKey, PostItem};
use crate::ui::{self, icon::Kind as Icon, ChoiceBox, Row, VList};
use plx_machine::machine::{By, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::listing::{Feed, Sort, Time, UserTab};
use rd_core::media::Media;
use std::borrow::Cow;
use std::cell::Cell;

const G_POSTS: GroupId = GroupId(1);
const G_BAR: GroupId = GroupId(2);
const G_NAV: GroupId = GroupId(3);
const POST_BASE: u32 = 0x1000_0000;
const BAR_BASE: u32 = 0x3000_0000;
const NAV_BASE: u32 = 0x4000_0000;
const CHOICE_BASE: u32 = ui::CHOICE_BASE;

const LEFT: f32 = 80.0;
const NAV_W: f32 = 440.0;
const T_BACK: plx_machine::machine::TimerId = plx_machine::machine::TimerId(41);

#[derive(Clone, Debug, PartialEq)]
enum BarItem {
    Setup,
    Sort,
    Time,
    Layout,
    Search,
    Favorite,
    Join,
    Refresh,
    Menu,
}

#[derive(Clone, Debug, PartialEq)]
enum NavItem {
    Search,
    Feed(Feed),
    Subs,
    Inbox,
    Settings,
    Account,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    Cards,
    List,
    Large,
    Compact,
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
    state: State,
}

fn layout_of(cx: &Cx<'_, RdHost>) -> Layout {
    match cx.views.store.prefs.get("layout") {
        "list" => Layout::List,
        "large" => Layout::Large,
        "compact" => Layout::Compact,
        _ => Layout::Cards,
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
        _ => Feed::Front,
    }
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
        };
        s.configure_flow(cx);
        s
    }

    /// Sets the flow's geometry for the current layout and sizes.
    fn configure_flow(&mut self, cx: &Cx<'_, RdHost>) {
        self.layout = layout_of(cx);
        let top = header_h();
        let prefs = &cx.views.store.prefs;
        let f = &mut self.flow;
        f.pad_top = 8.0;
        f.margin = 40.0;
        match self.layout {
            Layout::Cards => {
                f.cols = prefs.num("columns").clamp(2, 6) as usize;
                f.rect = Rect::new(LEFT, top, 1920.0 - 2.0 * LEFT, 1080.0 - top);
                f.gap = 28.0;
                f.row_gap = 32.0;
            }
            Layout::List | Layout::Compact => {
                f.cols = 1;
                f.rect = Rect::new(LEFT + 40.0, top, 1920.0 - 2.0 * LEFT - 80.0, 1080.0 - top);
                f.gap = 0.0;
                f.row_gap = if self.layout == Layout::Compact { 8.0 } else { 18.0 };
            }
            Layout::Large => {
                f.cols = 1;
                let w = 1280.0;
                f.rect = Rect::new((1920.0 - w) / 2.0, top, w, 1080.0 - top);
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
            | (st.read.contains("") as u64) << 38;
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
        }
    }

    fn meta_size(&self) -> i32 {
        match self.layout {
            Layout::Cards if self.flow.cols >= 5 => ui::sz(19),
            Layout::Cards => ui::sz(21),
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
        }
    }

    fn bar_items(&self, cx: &Cx<'_, RdHost>) -> Vec<BarItem> {
        let st = cx.views.store;
        if !st.configured() {
            return vec![BarItem::Setup];
        }
        let mut v = Vec::new();
        if !self.key.feed.sorts(st.signed_in()).is_empty() {
            v.push(BarItem::Sort);
            if self.key.sort.timed() || (matches!(self.key.feed, Feed::Search { .. }) && self.key.sort == Sort::Top) {
                v.push(BarItem::Time);
            }
        }
        v.push(BarItem::Layout);
        v.push(BarItem::Search);
        if let Some(_sub) = self.key.feed.sub() {
            v.push(BarItem::Favorite);
            if st.signed_in() {
                v.push(BarItem::Join);
            }
            v.push(BarItem::Menu);
        }
        v.push(BarItem::Refresh);
        v
    }

    fn bar_label(&self, b: &BarItem, cx: &Cx<'_, RdHost>) -> (String, Option<Icon>, bool) {
        let st = cx.views.store;
        match b {
            BarItem::Setup => ("Set up Reddit API".into(), Some(Icon::Key), false),
            BarItem::Sort => (self.key.sort.label().into(), Some(Icon::Trending), false),
            BarItem::Time => (self.key.time.label().into(), Some(Icon::Clock), false),
            BarItem::Layout => (
                match self.layout {
                    Layout::Cards => "Cards",
                    Layout::List => "List",
                    Layout::Large => "Large",
                    Layout::Compact => "Compact",
                }
                .into(),
                Some(Icon::List),
                false,
            ),
            BarItem::Search => ("Search".into(), Some(Icon::Search), false),
            BarItem::Favorite => {
                let on = self.key.feed.sub().is_some_and(|s| st.prefs.is_favorite(s));
                (if on { "Favourite" } else { "Add to favourites" }.into(), Some(Icon::Star), on)
            }
            BarItem::Join => {
                let on = self.key.feed.sub().and_then(|s| st.about.get(&s.to_ascii_lowercase())).is_some_and(|a| a.subscribed);
                (if on { "Joined" } else { "Join" }.into(), Some(Icon::Check), on)
            }
            BarItem::Refresh => ("Refresh".into(), Some(Icon::Repeat), false),
            BarItem::Menu => ("More".into(), Some(Icon::Info), false),
        }
    }

    fn nav_items(&self, cx: &Cx<'_, RdHost>) -> Vec<NavItem> {
        let st = cx.views.store;
        let mut v = vec![NavItem::Search, NavItem::Feed(Feed::Front), NavItem::Feed(Feed::Popular), NavItem::Feed(Feed::All), NavItem::Subs];
        if let Some(u) = st.username() {
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
            NavItem::Inbox => {
                let r = Row::new("Inbox").icon(Icon::Bell);
                if st.unread > 0 { r.value(st.unread.to_string()) } else { r }
            }
            NavItem::Settings => Row::new("Settings").icon(Icon::Gear),
            NavItem::Account => match st.username() {
                Some(u) => Row::new(format!("u/{u}")).icon(Icon::User).raw(),
                None => Row::new("Sign in").icon(Icon::User),
            },
        }
    }

    fn switch_feed(&mut self, feed: Feed, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if feed == self.key.feed {
            fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload: true }));
            return;
        }
        self.key = cx.views.store.key_for(feed);
        self.flow.to_top();
        self.sig.set((u32::MAX, 0, 0, 0));
        self.content_last.set(POST_BASE);
        self.asked_more_at.set(usize::MAX);
        self.load(cx, fx, false);
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn load(&self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>, reload: bool) {
        if !cx.views.store.configured() {
            return;
        }
        fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload }));
        if let Some(sub) = self.key.feed.sub() {
            fx.push(Fx::App(RdFx::LoadAbout(sub.to_string())));
        }
    }

    fn set_key(&mut self, key: FeedKey, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if let (Some(sub), true) = (key.feed.sub(), cx.views.store.prefs.on("remember_sort")) {
            let v = format!("{}|{}:{}", sub.to_ascii_lowercase(), key.sort.key(), key.time.key());
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
        self.focus_req.set(Some(POST_BASE));
        self.load(cx, fx, false);
    }

    fn open_choice(&mut self, c: Choice, anchor: Rect, cx: &Cx<'_, RdHost>) {
        let (options, current): (Vec<String>, usize) = match c {
            Choice::Sort => {
                let sorts = self.key.feed.sorts(cx.views.store.signed_in());
                (sorts.iter().map(|s| s.label().to_string()).collect(), sorts.iter().position(|s| *s == self.key.sort).unwrap_or(0))
            }
            Choice::Time => (Time::ALL.iter().map(|t| t.label().to_string()).collect(), Time::ALL.iter().position(|t| *t == self.key.time).unwrap_or(0)),
            Choice::Layout => {
                let o = crate::prefs::opt("layout").unwrap();
                let cur = cx.views.store.prefs.get("layout");
                (o.values.iter().map(|(_, l)| l.to_string()).collect(), o.values.iter().position(|(v, _)| *v == cur).unwrap_or(0))
            }
        };
        let b = ChoiceBox::new(self.entry, 0, options, current, anchor);
        self.focus_req.set(Some(b.list.elem(current)));
        ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.choice = Some((c, b));
    }

    fn close_choice(&mut self) {
        if self.choice.take().is_some() {
            ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        let i = self.bar_items_cache_index();
        self.focus_req.set(Some(BAR_BASE + i));
    }

    fn bar_items_cache_index(&self) -> u32 {
        0
    }

    fn pick_choice(&mut self, idx: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some((c, _)) = self.choice.as_ref() else { return };
        let c = *c;
        if let Some((_, b)) = self.choice.take() {
            let _ = b;
            ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
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
        let st = cx.views.store;
        match b {
            BarItem::Setup => fx.push(Fx::Nav(NavOp::Push(RdArg::ApiSettings))),
            BarItem::Sort => self.open_choice(Choice::Sort, anchor, cx),
            BarItem::Time => self.open_choice(Choice::Time, anchor, cx),
            BarItem::Layout => self.open_choice(Choice::Layout, anchor, cx),
            BarItem::Search => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::Search { sub: self.key.feed.sub().map(str::to_string) })))),
            BarItem::Favorite => {
                if let Some(sub) = self.key.feed.sub() {
                    let on = st.prefs.is_favorite(sub);
                    fx.push(Fx::App(RdFx::EditPrefs(
                        |p, s| {
                            p.toggle_favorite(s);
                        },
                        sub.to_string(),
                    )));
                    fx.push(Fx::App(RdFx::Toast(if on { format!("Removed r/{sub} from favourites") } else { format!("Added r/{sub} to favourites") })));
                }
            }
            BarItem::Join => {
                if let Some(sub) = self.key.feed.sub() {
                    let on = st.about.get(&sub.to_ascii_lowercase()).is_some_and(|a| a.subscribed);
                    fx.push(Fx::App(RdFx::Subscribe { sub: sub.to_string(), on: !on }));
                }
            }
            BarItem::Refresh => {
                self.flow.to_top();
                fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload: true }));
            }
            BarItem::Menu => {
                if let Some(sub) = self.key.feed.sub() {
                    fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Sub(sub.to_string())))));
                }
            }
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
            NavItem::Inbox => fx.push(Fx::Nav(NavOp::Push(RdArg::Inbox))),
            NavItem::Settings => fx.push(Fx::Nav(NavOp::Push(RdArg::Settings))),
            NavItem::Account => fx.push(Fx::Nav(NavOp::Push(if cx.views.store.signed_in() { RdArg::Settings } else { RdArg::SignIn }))),
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

    fn on_nav(&self, cx: &Cx<'_, RdHost>) -> bool {
        cx.focus.current.is_some_and(|k| k.entry == self.entry && (NAV_BASE..CHOICE_BASE).contains(&k.elem))
    }
}

fn list_thumb_w() -> f32 {
    ui::ls(300.0).clamp(220.0, 440.0)
}

impl Machine<RdHost> for FeedScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        // Holding OK on a post: its menu.
        if let Some(e) = common::press_hold(ev, cx) {
            if let Some(i) = self.flow.index(e) {
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
            if cx.views.store.prefs.on("color_keys") {
                if let Some(it) = self.focused_post(cx) {
                    let name = it.post.name.clone();
                    match c {
                        Color::Green => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(true) { 0 } else { 1 } })),
                        Color::Red => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(false) { 0 } else { -1 } })),
                        Color::Yellow => fx.push(Fx::App(RdFx::Save { fullname: name, on: !it.post.saved })),
                        Color::Blue => self.open_post(it, fx),
                    }
                }
            }
            return Handled::Yes;
        }
        // BACK: closes the side menu; on the start page it opens it, and (twice) leaves the app.
        if common::back_any(ev) && self.root {
            let down = common::back_down(ev);
            if self.on_nav(cx) && self.nav_open.get() {
                if down {
                    let now = cx.views.now_ms;
                    let twice = cx.views.store.prefs.get("back_exit") == "twice";
                    if !twice || (self.back_at != 0 && now.wrapping_sub(self.back_at) < 700) {
                        self.exit_armed = true;
                        return Handled::No;
                    }
                    self.back_at = now;
                    fx.push(Fx::CancelTimer(T_BACK));
                    fx.push(Fx::Timer { id: T_BACK, after_ms: 700 });
                    fx.push(Fx::App(RdFx::Toast("Press BACK again to leave".into())));
                    return Handled::Yes;
                }
                return if self.exit_armed { Handled::No } else { Handled::Yes };
            }
            if down {
                self.nav_open.set(true);
                let items = self.nav_items(cx);
                let i = items.iter().position(|n| matches!(n, NavItem::Feed(f) if *f == self.key.feed)).unwrap_or(1);
                self.focus_req.set(Some(self.nav.elem(i)));
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        if let ScreenEvent::Input(InputEvent { kind: InputKind::Click { x, .. }, .. }) = ev {
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
            }
            ScreenEvent::Uncover => {
                if layout_of(cx) != self.layout || self.sig.get().3 != self.signature(cx, 0).3 {
                    self.configure_flow(cx);
                }
                if cx.views.store.feed(&self.key).is_none() {
                    self.key = cx.views.store.key_for(self.key.feed.clone());
                    self.load(cx, fx, false);
                }
                fx.invalidate(plx_machine::present::Provenance::Nav);
            }
            ScreenEvent::Timer(T_BACK) => {
                self.back_at = 0;
                if self.on_nav(cx) && self.nav_open.get() {
                    self.nav_open.set(false);
                    self.focus_req.set(Some(self.content_last.get()));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
            }
            ScreenEvent::Async(_, RdMsg::Changed) => {
                // Settings may have changed the layout.
                if layout_of(cx) != self.layout {
                    self.configure_flow(cx);
                }
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if let Some(i) = self.flow.index(to.elem) {
                    if self.nav_open.get() && *by == By::Pointer {
                        // Hovering a card behind the open menu: keep the menu.
                        return Handled::Yes;
                    }
                    self.nav_open.set(false);
                    self.content_last.set(to.elem);
                    self.flow.on_focus(i, *by == By::Pointer);
                    // Near the end: the next page.
                    let n = self.flow.len();
                    if i + self.flow.cols * 3 >= n && self.asked_more_at.get() != n {
                        self.asked_more_at.set(n);
                        fx.push(Fx::App(RdFx::MoreFeed(self.key.clone())));
                    }
                } else if (BAR_BASE..NAV_BASE).contains(&to.elem) {
                    self.nav_open.set(false);
                    self.content_last.set(to.elem);
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
        let valid = if want.elem >= CHOICE_BASE {
            false
        } else if want.elem >= NAV_BASE {
            self.root && self.nav.index(want.elem, self.nav_items(cx).len()).is_some()
        } else if want.elem >= BAR_BASE {
            ((want.elem - BAR_BASE) as usize) < self.bar_items(cx).len()
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
        let labels: Vec<(String, Option<Icon>, bool)> = items.iter().map(|b| self.bar_label(b, cx)).collect();
        let bh = ui::ls(58.0).max(52.0);
        let right = 1920.0 - LEFT + if st.prefs.on("clock") { -130.0 } else { 0.0 };
        let rects = common::button_row(f.measure, &labels.iter().map(|(l, i, _)| (l.as_str(), i.is_some())).collect::<Vec<_>>(), 0.0, 34.0, bh, 14.0, Some(right));
        let bar_left = rects.first().map_or(right, |r| r.x);
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for (i, (r, (l, icon, active))) in rects.iter().zip(labels.iter()).enumerate() {
            common::button(p, f.measure, *r, l, *icon, focus == Some(BAR_BASE + i as u32), *active, false);
        }
        *self.bar_rects.borrow_mut() = rects.clone();
        let title_room = (bar_left - x - 30.0).max(200.0);
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
                let mut s = self.key.sort.label().to_string();
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
        let bs = ui::sz(17);
        if let Some(b) = common::media_badge(it) {
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
        // Card ground and focus.
        if focused {
            p.shadow(r, rad, 34.0, 10.0, [0.0, 0.0, 0.0, 0.6]);
            ui::fill(p, r, rad, ui::surface(0.17));
        } else if self.layout != Layout::Compact {
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
                meta2.push_str(&format!("  \u{b7}  {} comments", common::short_count(it.post.num_comments)));
                if let Some(b) = common::media_badge(it) {
                    meta2.push_str(&format!("  \u{b7}  {b}"));
                }
                ui::text_cut(p, m, &meta2, tx, y + ts as f32 * 1.42, tw, ms, dim, ui::ALIGN_LEFT, false);
            }
        }
        if focused {
            p.rring(r, rad, 4.0, ui::accent());
        }
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
                    common::message(p, f.measure, area, "Couldn't load this feed", fd.error.as_deref().unwrap_or_default());
                }
                Some(fd) if !fd.ids.is_empty() => common::message(p, f.measure, area, "Everything here is filtered out", "Your filters, NSFW or read-post settings hide every post of this feed."),
                Some(_) => common::message(p, f.measure, area, "Nothing here", "This feed has no posts."),
                None => ui::spinner(p, cx.views.now_ms, 960.0, 500.0),
            }
        } else {
            let focus = self.flow.focused(cx);
            let range = self.flow.visible();
            {
                let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(0.0, header_h() - 20.0, 1920.0, 1100.0 - header_h()));
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
            // The end of the feed: loading more, or the end.
            if let Some(last) = self.flow.screen_rect(items.len() - 1) {
                let y = last.y + last.h + 40.0;
                if y < 1080.0 {
                    match feed {
                        Some(fd) if fd.loading => ui::spinner(p, cx.views.now_ms, 960.0, y + 50.0),
                        Some(fd) if fd.error.is_some() => {
                            ui::text_raw(p, fd.error.as_deref().unwrap_or_default(), 960.0, y + 20.0, ui::sz(24), ui::DIM, ui::ALIGN_CENTER, false);
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
        }
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
