//! A user's profile: their icon, name, karma and cake day (and a "You" tag on the user's own)
//! over their posts and comments, in three tabs (Overview with both, Posts, Comments) with a sort
//! and Refresh. OK on a post opens it, on a comment opens its post at that comment; holding OK on a
//! post opens its menu; the colour buttons act on posts as in a feed, and blue opens a comment too.

use super::common::{self, Color, State};
use crate::flow::Flow;
use crate::host::{MenuKind, RdArg, RdFx, RdHost, RdMsg};
use crate::images::{self, Fit};
use crate::richtext;
use crate::store::{FeedKey, PostItem, UserItem, UserPage};
use crate::ui::{self, icon::Kind as Icon, ChoiceBox};
use plx_machine::machine::{By, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::comments::UserComment;
use rd_core::listing::{Feed, Sort, Time, UserTab};
use rd_core::media::Media;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};

const G_BAR: GroupId = GroupId(1);
const G_LIST: GroupId = GroupId(2);
const LIST_BASE: u32 = 0x1000_0000;
const BAR_BASE: u32 = 0x3000_0000;

const LEFT: f32 = 160.0;
const PAD: f32 = 18.0;
const FULL: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);
/// A comment's text is cut after this many lines (OK opens it whole, in its thread).
const COMMENT_LINES: f32 = 4.0;

/// The tabs, in order.
const TABS: [UserTab; 3] = [UserTab::Overview, UserTab::Submitted, UserTab::Comments];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bar {
    Tab(UserTab),
    Sort,
    Refresh,
}

fn bar_items() -> Vec<Bar> {
    TABS.iter().map(|t| Bar::Tab(*t)).chain([Bar::Sort, Bar::Refresh]).collect()
}

fn tab_label(t: UserTab) -> &'static str {
    match t {
        UserTab::Submitted => "Posts",
        UserTab::Comments => "Comments",
        _ => "Overview",
    }
}

fn name_size() -> i32 {
    ui::sz(44)
}

/// Where the buttons sit: under the name, the karma line, the cake day and the bio.
fn bar_y() -> f32 {
    36.0 + name_size() as f32 * 1.3 + ui::sz(24) as f32 * 1.42 + 2.0 * ui::sz(22) as f32 * 1.42 + 18.0
}

fn bar_h() -> f32 {
    ui::ls(58.0).max(52.0)
}

fn header_h() -> f32 {
    bar_y() + bar_h() + 22.0
}

fn thumb() -> (f32, f32) {
    let w = ui::ls(140.0).clamp(110.0, 200.0);
    (w, (w * 0.66).round())
}

fn prefs_sig() -> u64 {
    (ui::text_scale() * 100.0) as u64 | ((ui::content_scale() * 100.0) as u64) << 12 | (ui::font_code(false) as u64) << 24 | (common::hint_band() as u64) << 32
}

/// "1 point", "12 points", "1.2k points".
fn points(n: i64) -> String {
    match n {
        1 => crate::i18n::tr("1 point").to_string(),
        n => crate::i18n::tr("{} points").replace("{}", &common::short_count(n)),
    }
}

/// How much of `laid` shows when it is cut to `max`: down to its last whole line in that room.
fn cut_h(laid: &richtext::Laid, max: f32) -> f32 {
    if laid.h <= max {
        return laid.h;
    }
    let bottom = laid
        .items
        .iter()
        .filter_map(|it| match it {
            richtext::Item::Text { y, size, .. } => Some(y + (*size as f32 * 1.42).round()),
            _ => None,
        })
        .filter(|b| *b <= max)
        .fold(0.0, f32::max);
    if bottom > 0.0 { bottom } else { max }
}

/// A post's thumbnail: (picture server, url), blurred for NSFW or spoilers as the settings ask.
fn post_pic(cx: &Cx<'_, RdHost>, it: &PostItem) -> Option<(u16, String)> {
    let prefs = &cx.views.store.prefs;
    if !prefs.on("show_thumbs") {
        return None;
    }
    let blur = (it.post.over_18 && prefs.get("nsfw") == "blur") || (it.post.spoiler && prefs.get("spoilers") == "blur");
    let want = if blur { 140 } else { prefs.pic_width(thumb().0) };
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

pub struct ProfileScreen {
    entry: EntryId,
    name: String,
    tab: UserTab,
    sort: Sort,
    flow: Flow,
    /// The sizes the list was placed for.
    rect_sig: u64,
    /// (page gen, store gen, sizes) the rows were measured for; a new tab or sort resets it.
    sig: Cell<(u32, u32, u64)>,
    /// The entries shown (the page after the user's filters).
    rows: RefCell<Vec<UserItem>>,
    bar_rects: RefCell<Vec<Rect>>,
    choice: Option<ChoiceBox>,
    focus_req: Cell<Option<u32>>,
    asked_more_at: Cell<usize>,
    /// Down on the last entry after a page failed: ask again (on the next tick).
    retry_more: Cell<bool>,
    /// Until the user moves, focus goes to the first entry when the list arrives.
    auto_focus: Cell<bool>,
    /// The page failed to load and focus went to Refresh.
    error_seated: Cell<bool>,
    state: State,
}

impl ProfileScreen {
    pub fn new(entry: EntryId, name: String) -> Self {
        let mut flow = Flow::new(entry, G_LIST, LIST_BASE, FULL);
        flow.row_gap = 12.0;
        flow.pad_top = 6.0;
        flow.margin = 40.0;
        let mut s = ProfileScreen {
            entry,
            state: State(format!("profile {name}")),
            name,
            tab: UserTab::Overview,
            sort: Sort::New,
            flow,
            rect_sig: u64::MAX,
            sig: Cell::new((u32::MAX, 0, 0)),
            rows: Default::default(),
            bar_rects: Default::default(),
            choice: None,
            focus_req: Cell::new(None),
            asked_more_at: Cell::new(usize::MAX),
            retry_more: Cell::new(false),
            auto_focus: Cell::new(true),
            error_seated: Cell::new(false),
        };
        s.place_list();
        s
    }

    fn place_list(&mut self) {
        let sig = prefs_sig();
        if sig == self.rect_sig {
            return;
        }
        self.rect_sig = sig;
        let top = header_h();
        // The key hints' band is under the list.
        self.flow.rect = Rect::new(LEFT, top, 1920.0 - 2.0 * LEFT, 1080.0 - top - common::hint_band());
        self.sig.set((u32::MAX, 0, 0));
    }

    fn key(&self) -> FeedKey {
        FeedKey { feed: Feed::User(self.name.clone(), self.tab), sort: self.sort, time: Time::All }
    }

    fn page<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a UserPage> {
        cx.views.store.user_pages.get(&self.key())
    }

    fn profile<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a crate::store::Profile> {
        cx.views.store.profiles.get(&self.name.to_ascii_lowercase())
    }

    fn is_me(&self, cx: &Cx<'_, RdHost>) -> bool {
        cx.views.store.username().is_some_and(|u| u.eq_ignore_ascii_case(&self.name))
    }

    fn sorts(&self) -> Vec<Sort> {
        Feed::User(self.name.clone(), self.tab).sorts(true)
    }

    fn load(&self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>, reload: bool) {
        if !cx.views.store.configured() {
            return;
        }
        fx.push(Fx::App(RdFx::LoadProfile { name: self.name.clone(), reload }));
        fx.push(Fx::App(RdFx::LoadUserItems { key: self.key(), reload }));
    }

    /// The page's entries the user's NSFW setting and word filters let through.
    fn shown(&self, cx: &Cx<'_, RdHost>) -> Vec<UserItem> {
        let st = cx.views.store;
        let Some(page) = self.page(cx) else { return Vec::new() };
        // Read or pinned posts stay: this is the user's page, not a feed being browsed.
        page.items
            .iter()
            .filter(|i| match i {
                UserItem::Post(id) => st.posts.get(id).is_some_and(|p| st.prefs.keeps("", &p.post.title, p.post.over_18, false, false)),
                UserItem::Comment(c) => st.prefs.keeps("", &c.link_title, c.over_18, false, false),
            })
            .cloned()
            .collect()
    }

    fn comment_laid(c: &UserComment, w: f32) -> std::rc::Rc<richtext::Laid> {
        richtext::cached(&format!("uc:{}", c.comment.name), &c.comment.body, w - 2.0 * PAD - 44.0, ui::csz(24))
    }

    fn item_h(&self, cx: &Cx<'_, RdHost>, item: &UserItem, w: f32) -> f32 {
        let ms = ui::sz(21);
        match item {
            UserItem::Post(id) => {
                let Some(it) = cx.views.store.posts.get(id) else { return 0.0 };
                let ts = ui::sz(26);
                let (tw, th) = thumb();
                let pic = post_pic(cx, it).is_some();
                let room = w - 2.0 * PAD - if pic { tw + 20.0 } else { 0.0 };
                let n = richtext::wrap_title(&it.post.title, room, ts, true, 2).len().max(1);
                (richtext::title_h(n, ts) + 6.0 + ms as f32 * 1.42).max(if pic { th } else { 0.0 }) + 2.0 * PAD
            }
            UserItem::Comment(c) => {
                let body = cut_h(&Self::comment_laid(c, w), ui::csz(24) as f32 * 1.42 * COMMENT_LINES);
                PAD + ms as f32 * 1.42 + 8.0 + body + 8.0 + ms as f32 * 1.42 + PAD
            }
        }
    }

    /// Rebuilds the rows and their heights when the page, the store or the sizes changed.
    fn relayout(&self, cx: &Cx<'_, RdHost>) {
        // Filters and votes bump the store's gen.
        let sig = (self.page(cx).map_or(0, |p| p.gen), cx.views.store.gen, prefs_sig());
        if sig == self.sig.get() {
            return;
        }
        self.sig.set(sig);
        let rows = self.shown(cx);
        let w = self.flow.col_w();
        let heights: Vec<f32> = rows.iter().map(|r| self.item_h(cx, r, w)).collect();
        self.flow.set_heights(&heights);
        *self.rows.borrow_mut() = rows;
    }

    fn row(&self, i: usize) -> Option<UserItem> {
        self.rows.borrow().get(i).cloned()
    }

    fn open(&self, item: &UserItem, fx: &mut Effects<'_, RdHost>) {
        match item {
            UserItem::Post(id) => {
                fx.push(Fx::App(RdFx::MarkRead(id.clone())));
                fx.push(Fx::Nav(NavOp::Push(RdArg::Post { id: id.clone(), comment: None })));
            }
            UserItem::Comment(c) => fx.push(Fx::Nav(NavOp::Push(RdArg::Post { id: c.post_id().to_string(), comment: Some(c.comment.id.clone()) }))),
        }
    }

    fn tab_key(&self) -> FocusKey<u32> {
        let i = TABS.iter().position(|t| *t == self.tab).unwrap_or(0);
        FocusKey { entry: self.entry, elem: BAR_BASE + i as u32 }
    }

    /// A new tab or sort: from the top, focus staying on the bar.
    fn show(&mut self, tab: UserTab, sort: Sort, fx: &mut Effects<'_, RdHost>) {
        let same = tab == self.tab && sort == self.sort;
        self.tab = tab;
        self.sort = sort;
        self.flow.to_top();
        self.sig.set((u32::MAX, 0, 0));
        self.asked_more_at.set(usize::MAX);
        self.auto_focus.set(false);
        // Choosing the tab already shown loads it again.
        fx.push(Fx::App(RdFx::LoadUserItems { key: self.key(), reload: same }));
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn activate_bar(&mut self, i: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some(b) = bar_items().get(i).copied() else { return };
        match b {
            Bar::Tab(t) => {
                // A sort the new tab does not offer falls back to New.
                let sort = if Feed::User(self.name.clone(), t).sorts(true).contains(&self.sort) { self.sort } else { Sort::New };
                self.show(t, sort, fx);
            }
            Bar::Sort => {
                let anchor = self.bar_rects.borrow().get(i).copied().unwrap_or(Rect::new(900.0, bar_y(), 200.0, bar_h()));
                let sorts = self.sorts();
                let feed = Feed::User(self.name.clone(), self.tab);
                let current = sorts.iter().position(|s| *s == self.sort).unwrap_or(0);
                let b = ChoiceBox::new(self.entry, i, sorts.iter().map(|s| s.label_in(&feed).to_string()).collect(), current, anchor);
                self.focus_req.set(Some(b.list.elem(current)));
                ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.choice = Some(b);
            }
            Bar::Refresh => {
                self.flow.to_top();
                self.asked_more_at.set(usize::MAX);
                self.load(cx, fx, true);
            }
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn close_choice(&mut self) {
        if let Some(b) = self.choice.take() {
            ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            self.focus_req.set(Some(BAR_BASE + b.row as u32));
        }
    }

    /// The page failed with nothing to show.
    fn failed(&self, cx: &Cx<'_, RdHost>) -> bool {
        let page_failed = self.page(cx).is_some_and(|p| p.items.is_empty() && p.error.is_some() && !p.loading);
        let profile_failed = self.profile(cx).is_some_and(|p| p.info.is_none() && p.error.is_some() && !p.loading);
        page_failed || profile_failed
    }
}

impl Machine<RdHost> for ProfileScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        self.place_list();
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        if let Some(dy) = common::wheel_dy(ev) {
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
            if let Some(UserItem::Post(id)) = self.flow.index(e).and_then(|i| self.row(i)) {
                fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Post(id)))));
            }
            return Handled::Yes;
        }
        if self.choice.is_some() && common::back_any(ev) {
            if common::back_down(ev) {
                self.close_choice();
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        if let Some(c) = common::color_key(ev) {
            if cx.views.store.prefs.on("color_keys") {
                match self.flow.focused(cx).and_then(|i| self.row(i)) {
                    // Signed out, the vote and save buttons offer to sign in, as on a feed.
                    Some(UserItem::Post(_)) if common::sign_in_first(c, cx, fx) => {}
                    Some(UserItem::Post(id)) => {
                        if let Some(it) = cx.views.store.posts.get(&id) {
                            let name = it.post.name.clone();
                            match c {
                                Color::Green => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(true) { 0 } else { 1 } })),
                                Color::Red => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(false) { 0 } else { -1 } })),
                                Color::Yellow => fx.push(Fx::App(RdFx::Save { fullname: name, on: !it.post.saved })),
                                Color::Blue => self.open(&UserItem::Post(id), fx),
                            }
                        }
                    }
                    Some(item @ UserItem::Comment(_)) if c == Color::Blue => self.open(&item, fx),
                    _ => {}
                }
            }
            return Handled::Yes;
        }
        match ev {
            ScreenEvent::Mount => self.load(cx, fx, false),
            ScreenEvent::Uncover => {
                // Signing in or out meanwhile dropped what this page showed: load it again.
                if self.page(cx).is_none() || self.profile(cx).is_none() {
                    self.load(cx, fx, false);
                }
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            ScreenEvent::Async(_, RdMsg::Changed) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::Tick(_) => {
                if self.retry_more.take() {
                    fx.push(Fx::App(RdFx::MoreUserItems(self.key())));
                }
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if matches!(by, By::Dir | By::Pointer) {
                    self.auto_focus.set(false);
                }
                if let Some(i) = self.flow.index(to.elem) {
                    self.flow.on_focus(i, *by == By::Pointer);
                    // Near the end: the next page.
                    let n = self.flow.len();
                    if i + 4 >= n && self.asked_more_at.get() != n {
                        self.asked_more_at.set(n);
                        fx.push(Fx::App(RdFx::MoreUserItems(self.key())));
                    }
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Activate(e) => {
                let e = *e;
                if e >= ui::CHOICE_BASE {
                    if let Some(b) = self.choice.take() {
                        ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        self.focus_req.set(Some(BAR_BASE + b.row as u32));
                        if let Some(s) = b.list.index(e, b.options.len()).and_then(|i| self.sorts().get(i).copied()) {
                            self.show(self.tab, s, fx);
                        }
                    }
                } else if e >= BAR_BASE {
                    self.activate_bar((e - BAR_BASE) as usize, cx, fx);
                } else if let Some(item) = self.flow.index(e).and_then(|i| self.row(i)) {
                    self.open(&item, fx);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<RdHost> for ProfileScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        if let Some(b) = &self.choice {
            out.push(b.list.spec(b.options.len()));
            return;
        }
        self.relayout(cx);
        out.push(GroupSpec {
            id: G_BAR,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(LEFT, bar_y(), 1920.0 - 2.0 * LEFT, bar_h()),
            len: bar_items().len(),
            elem: ElemKind::Control,
        });
        if self.flow.len() > 0 {
            out.push(self.flow.spec());
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if *key >= ui::CHOICE_BASE {
            ui::CHOICE_GROUP
        } else if *key >= BAR_BASE {
            G_BAR
        } else {
            G_LIST
        })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        if let Some(b) = &self.choice {
            return b.list.neighbour(key, dir, b.options.len());
        }
        let k = |e: u32| Step::Move(FocusKey { entry: self.entry, elem: e });
        if key.elem >= BAR_BASE {
            let i = key.elem - BAR_BASE;
            let n = bar_items().len() as u32;
            return match dir {
                Dir::Left if i > 0 => k(key.elem - 1),
                Dir::Right if i + 1 < n => k(key.elem + 1),
                Dir::Down if self.flow.len() > 0 => {
                    let first = self.flow.visible().start.min(self.flow.len() - 1);
                    self.flow.reveal(first);
                    Step::Move(self.flow.key(first))
                }
                _ => Step::Edge,
            };
        }
        match self.flow.neighbour(key, dir) {
            Step::Edge if dir == Dir::Up => {
                self.flow.to_top();
                Step::Move(self.tab_key())
            }
            Step::Edge if dir == Dir::Down => {
                // The last entry: after a failed page, Down asks for it again.
                if self.page(cx).is_some_and(|p| p.error.is_some() && !p.loading && !p.done) {
                    self.retry_more.set(true);
                }
                Step::Edge
            }
            s => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        if *key >= ui::CHOICE_BASE {
            let b = self.choice.as_ref()?;
            return b.list.place(*key, b.options.len(), b.list.focused(cx, b.options.len()));
        }
        if *key >= BAR_BASE {
            let r = *self.bar_rects.borrow().get((*key - BAR_BASE) as usize)?;
            return Some(Placed { rect: r, rest_rect: r, clip: FULL, index: Some(*key - BAR_BASE) });
        }
        self.flow.place(*key)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let want = self.focus_req.take().map_or(want, |e| FocusKey { entry: self.entry, elem: e });
        if let Some(b) = &self.choice {
            return if b.owns(want.elem) { want } else { b.list.key(b.current) };
        }
        let refresh = FocusKey { entry: self.entry, elem: BAR_BASE + bar_items().len() as u32 - 1 };
        // Nothing could be loaded: Refresh, which tries again.
        let failed = self.failed(cx);
        if failed && !self.error_seated.replace(true) {
            return refresh;
        }
        if !failed {
            self.error_seated.set(false);
        }
        if self.auto_focus.get() && self.flow.len() > 0 && want.elem >= BAR_BASE {
            self.auto_focus.set(false);
            return self.flow.key(0);
        }
        let valid = if want.elem >= ui::CHOICE_BASE {
            false
        } else if want.elem >= BAR_BASE {
            ((want.elem - BAR_BASE) as usize) < bar_items().len()
        } else {
            self.flow.index(want.elem).is_some()
        };
        if valid {
            return want;
        }
        if self.flow.len() > 0 {
            return self.flow.key(self.flow.visible().start.min(self.flow.len() - 1));
        }
        self.tab_key()
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if g == G_LIST && self.flow.len() > 0 {
            return self.flow.key(self.flow.visible().start.min(self.flow.len() - 1));
        }
        self.tab_key()
    }
}

impl ProfileScreen {
    fn draw_header(&self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let m = f.measure;
        let cx = f.cx;
        let tr = crate::i18n::tr;
        let info = self.profile(cx).and_then(|p| p.info.as_ref());
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, header_h()));
        let ns = name_size();
        let (ks, cs) = (ui::sz(24), ui::sz(22));
        let isz = (bar_y() - 18.0 - 36.0).round();
        let ir = Rect::new(LEFT, 36.0, isz, isz);
        ui::fill(p, ir, isz / 2.0, ui::with_alpha(ui::accent(), 0.85));
        let drew = info.and_then(|i| i.icon.as_ref()).is_some_and(|u| matches!(images::draw(p, images::THUMB, u, ir, isz / 2.0, Fit::Cover, ui::WHITE), images::Drawn::At(_)));
        if !drew {
            ui::text_raw(p, "u/", ir.x + isz / 2.0, ir.y + (isz - ui::sz(40) as f32 * 1.3) / 2.0, ui::sz(40), ui::WHITE, ui::ALIGN_CENTER, true);
        }
        let x = ir.x + isz + 28.0;
        let w = 1920.0 - LEFT - x - if cx.views.store.prefs.on("clock") { 140.0 } else { 0.0 };
        let mut y = 36.0;
        let name = info.map_or(self.name.as_str(), |i| i.name.as_str());
        let nw = ui::text_cut(p, m, &format!("u/{name}"), x, y, w - 240.0, ns, ui::WHITE, ui::ALIGN_LEFT, true);
        let bs = ui::sz(22);
        let mut bx = x + nw + 18.0;
        let by = y + (ns as f32 * 1.3 - bs as f32 * 1.45) / 2.0;
        if self.is_me(cx) {
            bx += common::badge(p, m, bx, by, tr("You"), ui::accent(), ui::WHITE, bs) + 10.0;
        }
        if info.is_some_and(|i| i.suspended) {
            common::badge(p, m, bx, by, tr("Suspended"), common::NSFW_RED, ui::WHITE, bs);
        }
        y += ns as f32 * 1.3;
        if let Some(i) = info.filter(|i| !i.suspended) {
            let karma = format!(
                "{}  \u{b7}  {}",
                tr("{} post karma").replace("{}", &common::short_count(i.link_karma)),
                tr("{} comment karma").replace("{}", &common::short_count(i.comment_karma))
            );
            ui::text_cut(p, m, &karma, x, y, w, ks, ui::DIM, ui::ALIGN_LEFT, false);
            y += ks as f32 * 1.42;
            if i.created_utc > 0 {
                let (yy, mm, dd) = rd_core::listing::utc_date(i.created_utc);
                let age = rd_core::listing::age(i.created_utc, rd_core::now_secs() as i64);
                let cake = format!("{}  ({age})", tr("Cake day: {}").replace("{}", &crate::i18n::full_date(yy, mm, dd)));
                ui::text_cut(p, m, &cake, x, y, w, cs, ui::FAINT, ui::ALIGN_LEFT, false);
            }
            y += cs as f32 * 1.42;
            if !i.bio.trim().is_empty() {
                ui::text_cut(p, m, &i.bio.replace('\n', " "), x, y, w, cs, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
        if cx.views.store.prefs.on("clock") {
            common::clock(p, 1920.0 - LEFT, 40.0);
        }
        // Tabs, sort and Refresh.
        let feed = Feed::User(self.name.clone(), self.tab);
        let labels: Vec<(&str, Icon, bool)> = bar_items()
            .into_iter()
            .map(|b| match b {
                Bar::Tab(t) => (
                    tab_label(t),
                    match t {
                        UserTab::Comments => Icon::Chat,
                        UserTab::Submitted => Icon::List,
                        _ => Icon::User,
                    },
                    t == self.tab,
                ),
                Bar::Sort => (self.sort.label_in(&feed), Icon::Trending, false),
                Bar::Refresh => ("Refresh", Icon::Repeat, false),
            })
            .collect();
        let rects = common::button_row(m, &labels.iter().map(|(l, _, _)| (*l, true)).collect::<Vec<_>>(), LEFT, bar_y(), bar_h(), 14.0, None);
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for (i, (r, (l, icon, active))) in rects.iter().zip(labels.iter()).enumerate() {
            common::button(p, m, *r, l, Some(*icon), focus == Some(BAR_BASE + i as u32), *active, false);
            if self.choice.is_none() {
                ui::stop(f, FocusKey { entry: self.entry, elem: BAR_BASE + i as u32 }, *r, FULL);
            }
        }
        *self.bar_rects.borrow_mut() = rects;
    }

    fn draw_card(p: Painter, r: Rect, focused: bool) {
        if focused {
            p.shadow(r, 16.0, 26.0, 8.0, [0.0, 0.0, 0.0, 0.5]);
            ui::fill(p, r, 16.0, ui::surface(0.16));
            p.rring(r, 16.0, 3.0, ui::with_alpha(ui::accent(), 0.9));
        } else {
            ui::fill(p, r, 16.0, ui::surface(0.06));
        }
    }

    fn draw_post(&self, f: &mut DrawFrame<'_, '_, RdHost>, it: &PostItem, r: Rect, focused: bool) {
        let p = f.painter;
        let m = f.measure;
        let cx = f.cx;
        let (ts, ms) = (ui::sz(26), ui::sz(21));
        Self::draw_card(p, r, focused);
        let mut tx = r.x + PAD;
        if let Some((srv, url)) = post_pic(cx, it) {
            let (tw, th) = thumb();
            let ir = Rect::new(r.x + PAD, r.y + (r.h - th) / 2.0, tw, th);
            ui::fill(p, ir, 10.0, ui::surface(0.08));
            images::draw(p, srv, &url, ir, 10.0, Fit::Cover, ui::WHITE);
            tx = ir.x + tw + 20.0;
        }
        let tw = r.x + r.w - PAD - tx;
        let lines = richtext::wrap_title(&it.post.title, tw, ts, true, 2);
        let th = richtext::title_h(lines.len().max(1), ts) + 6.0 + ms as f32 * 1.42;
        let mut y = r.y + (r.h - th) / 2.0;
        y = richtext::draw_lines(p, m, &lines, tx, y, tw, ts, ui::WHITE, true) + 6.0;
        let mut meta = format!(
            "r/{}  \u{b7}  {}  \u{b7}  {}  \u{b7}  {}",
            it.post.subreddit,
            points(it.post.score),
            common::comments(it.post.num_comments),
            rd_core::listing::age(it.post.created_utc, rd_core::now_secs() as i64)
        );
        if it.post.over_18 {
            meta = format!("NSFW  \u{b7}  {meta}");
        }
        let col = match it.post.likes {
            Some(true) => ui::UPVOTE,
            Some(false) => ui::DOWNVOTE,
            None => ui::DIM,
        };
        ui::text_cut(p, m, &meta, tx, y, tw, ms, col, ui::ALIGN_LEFT, false);
    }

    fn draw_comment(&self, f: &mut DrawFrame<'_, '_, RdHost>, c: &UserComment, r: Rect, focused: bool) {
        let p = f.painter;
        let m = f.measure;
        let ms = ui::sz(21);
        Self::draw_card(p, r, focused);
        // The subreddit and the post the comment is on.
        let x = r.x + PAD;
        let mut y = r.y + PAD;
        let is = ms as f32 * 1.1;
        ui::icon::draw(p, Icon::Chat, Rect::new(x, y + ms as f32 * 0.12, is, is), ui::accent(), false);
        let hx = x + is + 12.0;
        let sub = format!("r/{}", c.subreddit);
        let sw = ui::text_raw(p, &sub, hx, y, ms, ui::accent(), ui::ALIGN_LEFT, true);
        let tx = hx + sw + 14.0;
        ui::text_cut(p, m, &c.link_title, tx, y, r.x + r.w - PAD - tx, ms, ui::DIM, ui::ALIGN_LEFT, false);
        y += ms as f32 * 1.42 + 8.0;
        // The text, cut after a few lines.
        let laid = Self::comment_laid(c, r.w);
        let body_h = cut_h(&laid, ui::csz(24) as f32 * 1.42 * COMMENT_LINES);
        let clip = Rect::new(hx, y, r.x + r.w - PAD - hx, body_h);
        {
            let _scope = plx_ui::screen::ClipScope::open_in(p, clip);
            richtext::draw(p, &laid, hx, y, clip, [0.92, 0.92, 0.94, 1.0], false);
        }
        y += body_h + 8.0;
        let mut meta = vec![
            points(c.comment.score),
            rd_core::listing::age(c.comment.created_utc, rd_core::now_secs() as i64),
        ];
        if c.over_18 {
            meta.insert(0, "NSFW".into());
        }
        let col = match c.comment.likes {
            Some(true) => ui::UPVOTE,
            Some(false) => ui::DOWNVOTE,
            None => ui::FAINT,
        };
        ui::text_cut(p, m, &meta.join("  \u{b7}  "), hx, y, r.x + r.w - PAD - hx, ms, col, ui::ALIGN_LEFT, false);
    }

    /// What the list shows when it has no entries: loading, a failure, or why it is empty.
    fn draw_empty(&self, f: &mut DrawFrame<'_, '_, RdHost>, area: Rect) {
        let p = f.painter;
        let cx = f.cx;
        let tr = crate::i18n::tr;
        let profile = self.profile(cx);
        let page = self.page(cx);
        let retry = tr("OK on Refresh tries again; BACK goes back.");
        if profile.and_then(|p| p.info.as_ref()).is_some_and(|i| i.suspended) {
            return common::message(p, f.measure, area, "This account is suspended", tr("Reddit shows nothing of it."));
        }
        if let Some(e) = profile.filter(|p| p.info.is_none() && !p.loading).and_then(|p| p.error.as_deref()) {
            return common::message(p, f.measure, area, &tr("Couldn't open u/{}").replace("{}", &self.name), &format!("{e}  {retry}"));
        }
        match page {
            Some(pg) if pg.loading => ui::spinner(p, cx.views.now_ms, 960.0, area.y + area.h * 0.4),
            Some(pg) if pg.error.is_some() => common::message(p, f.measure, area, "Couldn't load this page", &format!("{}  {retry}", pg.error.as_deref().unwrap_or_default())),
            Some(pg) if !pg.items.is_empty() => common::message(p, f.measure, area, "Everything here is filtered out", tr("Your NSFW setting or word filters hide all of it.")),
            Some(_) => {
                let what = match self.tab {
                    UserTab::Submitted => "No posts yet",
                    UserTab::Comments => "No comments yet",
                    _ => "No posts or comments yet",
                };
                common::message(p, f.measure, area, what, "")
            }
            None => ui::spinner(p, cx.views.now_ms, 960.0, area.y + area.h * 0.4),
        }
    }
}

impl Screen<RdHost> for ProfileScreen {
    fn name(&self) -> &'static str {
        "profile"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        self.place_list();
        let cx = f.cx;
        self.relayout(cx);
        self.flow.animate(cx.views.now_ms);
        let p = f.painter;
        let focus = self.flow.focused(cx);
        let rows = self.rows.borrow();
        let area = self.flow.rect;
        if rows.is_empty() {
            self.draw_empty(f, area);
        } else {
            {
                let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(0.0, area.y - 8.0, 1920.0, 1088.0 - area.y));
                for i in self.flow.visible() {
                    let (Some(r), Some(item)) = (self.flow.screen_rect(i), rows.get(i)) else { continue };
                    match item {
                        UserItem::Post(id) => {
                            if let Some(it) = cx.views.store.posts.get(id) {
                                self.draw_post(f, it, r, focus == Some(i));
                            }
                        }
                        UserItem::Comment(c) => self.draw_comment(f, c, r, focus == Some(i)),
                    }
                }
            }
            // The end of the list: loading more, a failed page, or the end.
            if let (Some(last), Some(pg)) = (self.flow.screen_rect(rows.len() - 1), self.page(cx)) {
                let y = last.y + last.h + 40.0;
                if y < 1080.0 {
                    if pg.loading {
                        ui::spinner(p, cx.views.now_ms, 960.0, y + 50.0);
                    } else if let Some(e) = &pg.error {
                        let line = format!("{e}  \u{b7}  {}", crate::i18n::tr("Down tries again"));
                        ui::text_cut(p, f.measure, &line, 960.0, y + 20.0, area.w, ui::sz(24), ui::DIM, ui::ALIGN_CENTER, false);
                    } else if pg.done {
                        ui::text(p, "You have reached the end", 960.0, y + 20.0, ui::sz(24), ui::FAINT, ui::ALIGN_CENTER, false);
                    }
                }
            }
            if self.choice.is_none() {
                self.flow.stops(f);
            }
            match focus.and_then(|i| rows.get(i)) {
                Some(UserItem::Post(_)) => common::key_hints(p, f.measure, cx, 1080.0, common::FEED_HINTS),
                Some(UserItem::Comment(_)) => common::key_hints(p, f.measure, cx, 1080.0, common::PROFILE_COMMENT_HINTS),
                None => {}
            }
        }
        self.draw_header(f);
        if let Some(b) = &self.choice {
            ui::fill(f.painter, FULL, 0.0, [0.0, 0.0, 0.0, 0.35]);
            b.draw(f);
        }
        common::toast(f.painter, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
