//! Subreddits: tabs for favourites, the subreddits the user joined, popular ones and search
//! results, plus "Find subreddits" and "Go to r/…". OK opens one; holding OK opens its menu.
//! `InboxScreen` lists the signed-in user's messages and replies.

use super::common::{self, State};
use crate::flow::Flow;
use crate::host::{MenuKind, RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::images::{self, Fit};
use crate::richtext;
use crate::ui::{self, icon::Kind as Icon};
use plx_machine::machine::{By, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::Rect;
use rd_core::listing::Feed;
use rd_core::model::Subreddit;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};

const G_TABS: GroupId = GroupId(1);
const G_LIST: GroupId = GroupId(2);
const TAB_BASE: u32 = 0x3000_0000;
const LIST_BASE: u32 = 0x1000_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Favorites,
    Joined,
    Popular,
    Results,
    Find,
    GoTo,
}

fn header_h() -> f32 {
    ui::ls(140.0).max(130.0)
}

/// One row of the list.
#[derive(Clone)]
struct SubRow {
    name: String,
    title: String,
    desc: String,
    members: i64,
    icon: Option<String>,
    nsfw: bool,
}

impl SubRow {
    fn from(s: &Subreddit) -> SubRow {
        SubRow { name: s.display_name.clone(), title: s.title.clone(), desc: s.public_description.clone(), members: s.subscribers, icon: s.icon.as_ref().map(|i| rd_core::media::unescape(i)), nsfw: s.over18 }
    }
}

pub struct SubsScreen {
    entry: EntryId,
    tab: Tab,
    flow: Flow,
    sig: Cell<(u32, u32, Tab)>,
    rows: RefCell<Vec<SubRow>>,
    tab_rects: RefCell<Vec<Rect>>,
    focus_req: Cell<Option<u32>>,
    state: State,
}

impl SubsScreen {
    pub fn new(entry: EntryId) -> Self {
        let top = header_h();
        let mut flow = Flow::new(entry, G_LIST, LIST_BASE, Rect::new(120.0, top, 1680.0, 1080.0 - top));
        flow.cols = 2;
        flow.gap = 30.0;
        flow.row_gap = 18.0;
        SubsScreen {
            entry,
            tab: Tab::Favorites,
            flow,
            sig: Cell::new((u32::MAX, 0, Tab::GoTo)),
            rows: Default::default(),
            tab_rects: Default::default(),
            focus_req: Cell::new(None),
            state: State("subs".into()),
        }
    }

    fn tabs(&self, cx: &Cx<'_, RdHost>) -> Vec<Tab> {
        let st = cx.views.store;
        let mut v = vec![Tab::Favorites];
        if st.signed_in() {
            v.push(Tab::Joined);
        }
        v.push(Tab::Popular);
        if !st.subs_search.0.is_empty() {
            v.push(Tab::Results);
        }
        v.push(Tab::Find);
        v.push(Tab::GoTo);
        v
    }

    fn tab_label(&self, t: Tab, cx: &Cx<'_, RdHost>) -> (String, Icon) {
        match t {
            Tab::Favorites => ("Favourites".into(), Icon::Star),
            Tab::Joined => ("Joined".into(), Icon::Check),
            Tab::Popular => ("Popular".into(), Icon::Trending),
            Tab::Results => (format!("\u{201c}{}\u{201d}", cx.views.store.subs_search.0), Icon::Search),
            Tab::Find => ("Find subreddits\u{2026}".into(), Icon::Search),
            Tab::GoTo => ("Go to r/\u{2026}".into(), Icon::Subs),
        }
    }

    fn list(&self, cx: &Cx<'_, RdHost>) -> Vec<SubRow> {
        let st = cx.views.store;
        match self.tab {
            Tab::Favorites => st
                .prefs
                .favorites
                .iter()
                .map(|f| {
                    st.about.get(&f.to_ascii_lowercase()).map_or(SubRow { name: f.clone(), title: String::new(), desc: String::new(), members: 0, icon: None, nsfw: false }, |a| SubRow::from(&a.sub))
                })
                .collect(),
            Tab::Joined => st.subs_mine.items.iter().map(SubRow::from).collect(),
            Tab::Popular => st.subs_popular.items.iter().map(SubRow::from).collect(),
            Tab::Results => st.subs_search.1.items.iter().map(SubRow::from).collect(),
            _ => Vec::new(),
        }
    }

    fn list_state<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a crate::store::SubList> {
        let st = cx.views.store;
        match self.tab {
            Tab::Joined => Some(&st.subs_mine),
            Tab::Popular => Some(&st.subs_popular),
            Tab::Results => Some(&st.subs_search.1),
            _ => None,
        }
    }

    fn relayout(&self, cx: &Cx<'_, RdHost>) {
        let gen = self.list_state(cx).map_or(0, |l| l.gen);
        let sig = (gen, cx.views.store.gen, self.tab);
        if sig == self.sig.get() {
            return;
        }
        self.sig.set(sig);
        let rows = self.list(cx);
        let h = ui::ls(120.0).max(104.0);
        self.flow.set_heights(&vec![h; rows.len()]);
        *self.rows.borrow_mut() = rows;
    }

    fn show(&mut self, t: Tab, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        match t {
            Tab::Find => {
                fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::FindSub))));
                self.tab = Tab::Results;
            }
            Tab::GoTo => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::GoToSub)))),
            _ => self.tab = t,
        }
        self.flow.to_top();
        self.sig.set((u32::MAX, 0, Tab::GoTo));
        let st = cx.views.store;
        match self.tab {
            Tab::Joined if st.subs_mine.items.is_empty() => fx.push(Fx::App(RdFx::LoadSubs { mine: true })),
            Tab::Popular if st.subs_popular.items.is_empty() => fx.push(Fx::App(RdFx::LoadSubs { mine: false })),
            Tab::Favorites => {
                for f in &st.prefs.favorites {
                    fx.push(Fx::App(RdFx::LoadAbout(f.clone())));
                }
            }
            _ => {}
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<RdHost> for SubsScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        if let Some(e) = common::press_hold(ev, cx) {
            if let Some(r) = self.flow.index(e).and_then(|i| self.rows.borrow().get(i).cloned()) {
                fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Sub(r.name)))));
            }
            return Handled::Yes;
        }
        match ev {
            ScreenEvent::Mount => {
                let first = if cx.views.store.prefs.favorites.is_empty() {
                    if cx.views.store.signed_in() { Tab::Joined } else { Tab::Popular }
                } else {
                    Tab::Favorites
                };
                self.show(first, cx, fx);
            }
            ScreenEvent::Async(_, RdMsg::Changed) | ScreenEvent::Uncover => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::FocusMoved { to, by, .. } => {
                if let Some(i) = self.flow.index(to.elem) {
                    self.flow.on_focus(i, *by == By::Pointer);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Activate(e) => {
                if *e >= TAB_BASE {
                    let tabs = self.tabs(cx);
                    if let Some(t) = tabs.get((*e - TAB_BASE) as usize).copied() {
                        self.show(t, cx, fx);
                    }
                } else if let Some(r) = self.flow.index(*e).and_then(|i| self.rows.borrow().get(i).cloned()) {
                    fx.push(Fx::Nav(NavOp::Push(RdArg::Feed(Feed::Sub(r.name)))));
                }
                return Handled::Yes;
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<RdHost> for SubsScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        self.relayout(cx);
        out.push(GroupSpec {
            id: G_TABS,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(80.0, 20.0, 1760.0, header_h() - 30.0),
            len: self.tabs(cx).len(),
            elem: ElemKind::Control,
        });
        if self.flow.len() > 0 {
            out.push(self.flow.spec());
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if *key >= TAB_BASE { G_TABS } else { G_LIST })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        let k = |e: u32| Step::Move(FocusKey { entry: self.entry, elem: e });
        if key.elem >= TAB_BASE {
            let i = key.elem - TAB_BASE;
            let n = self.tabs(cx).len() as u32;
            return match dir {
                Dir::Left if i > 0 => k(key.elem - 1),
                Dir::Right if i + 1 < n => k(key.elem + 1),
                Dir::Down if self.flow.len() > 0 => k(self.flow.elem(0)),
                _ => Step::Edge,
            };
        }
        match self.flow.neighbour(key, dir) {
            Step::Edge if dir == Dir::Up => {
                let i = self.tabs(cx).iter().position(|t| *t == self.tab).unwrap_or(0) as u32;
                k(TAB_BASE + i)
            }
            s => s,
        }
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        if *key >= TAB_BASE {
            let r = *self.tab_rects.borrow().get((*key - TAB_BASE) as usize)?;
            return Some(Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(*key - TAB_BASE) });
        }
        self.flow.place(*key)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let want = self.focus_req.take().map_or(want, |e| FocusKey { entry: self.entry, elem: e });
        let ok = if want.elem >= TAB_BASE { ((want.elem - TAB_BASE) as usize) < self.tabs(cx).len() } else { self.flow.index(want.elem).is_some() };
        if ok {
            return want;
        }
        if self.flow.len() > 0 {
            return self.flow.key(0);
        }
        FocusKey { entry: self.entry, elem: TAB_BASE }
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if g == G_LIST && self.flow.len() > 0 {
            return self.flow.key(0);
        }
        FocusKey { entry: self.entry, elem: TAB_BASE }
    }
}

impl Screen<RdHost> for SubsScreen {
    fn name(&self) -> &'static str {
        "subs"
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
        self.relayout(cx);
        self.flow.animate(cx.views.now_ms);
        let p = f.painter;
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        let rows = self.rows.borrow().clone();
        if rows.is_empty() {
            let area = self.flow.rect;
            match self.list_state(cx) {
                Some(l) if l.loading => ui::spinner(p, cx.views.now_ms, 960.0, 500.0),
                Some(l) if l.error.is_some() => common::message(p, f.measure, area, "Couldn't load the subreddits", l.error.as_deref().unwrap_or_default()),
                _ if self.tab == Tab::Favorites => common::message(p, f.measure, area, "No favourites yet", "Open a subreddit and choose \u{201c}Add to favourites\u{201d}: it shows up here and in the side menu."),
                _ => common::message(p, f.measure, area, "Nothing here", ""),
            }
        } else {
            let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(0.0, header_h() - 10.0, 1920.0, 1090.0 - header_h()));
            for i in self.flow.visible() {
                let (Some(r), Some(s)) = (self.flow.screen_rect(i), rows.get(i)) else { continue };
                let focused = focus == Some(self.flow.elem(i));
                if focused {
                    p.shadow(r, 18.0, 30.0, 8.0, [0.0, 0.0, 0.0, 0.5]);
                    ui::fill(p, r, 18.0, ui::surface(0.17));
                } else {
                    ui::fill(p, r, 18.0, ui::surface(0.07));
                }
                let isz = r.h - 32.0;
                let ir = Rect::new(r.x + 16.0, r.y + 16.0, isz, isz);
                ui::fill(p, ir, isz / 2.0, ui::with_alpha(ui::accent(), 0.85));
                let drew = s.icon.as_ref().is_some_and(|u| matches!(images::draw(p, images::THUMB, u, ir, isz / 2.0, Fit::Cover, ui::WHITE), images::Drawn::At(_)));
                if !drew {
                    ui::text_raw(p, "r/", ir.x + isz / 2.0, ir.y + (isz - ui::sz(30) as f32 * 1.3) / 2.0, ui::sz(30), ui::WHITE, ui::ALIGN_CENTER, true);
                }
                let tx = ir.x + isz + 20.0;
                let tw = r.x + r.w - 20.0 - tx;
                let ts = ui::sz(28);
                let ms = ui::sz(21);
                let mut y = r.y + 14.0;
                let mut name = format!("r/{}", s.name);
                if s.nsfw {
                    name.push_str("  \u{b7}  NSFW");
                }
                ui::text_cut(p, f.measure, &name, tx, y, tw, ts, ui::WHITE, ui::ALIGN_LEFT, true);
                y += ts as f32 * 1.35;
                let mut meta = Vec::new();
                if s.members > 0 {
                    meta.push(format!("{} members", common::short_count(s.members)));
                }
                if !s.title.is_empty() {
                    meta.push(s.title.clone());
                }
                ui::text_cut(p, f.measure, &meta.join("  \u{b7}  "), tx, y, tw, ms, ui::DIM, ui::ALIGN_LEFT, false);
                y += ms as f32 * 1.42;
                if !s.desc.is_empty() {
                    ui::text_cut(p, f.measure, &s.desc.replace('\n', " "), tx, y, tw, ms, ui::FAINT, ui::ALIGN_LEFT, false);
                }
                if focused {
                    p.rring(r, 18.0, 4.0, ui::accent());
                }
            }
        }
        self.flow.stops(f);
        // Header
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, header_h()));
        ui::text(p, "Subreddits", 80.0, 30.0, ui::sz(44), ui::WHITE, ui::ALIGN_LEFT, true);
        let tabs = self.tabs(cx);
        let labels: Vec<(String, Icon)> = tabs.iter().map(|t| self.tab_label(*t, cx)).collect();
        let bh = ui::ls(56.0).max(50.0);
        let rects = common::button_row(f.measure, &labels.iter().map(|(l, _)| (l.as_str(), true)).collect::<Vec<_>>(), 0.0, 32.0, bh, 12.0, Some(1840.0));
        for (i, (r, (l, icon))) in rects.iter().zip(labels.iter()).enumerate() {
            common::button(p, f.measure, *r, l, Some(*icon), focus == Some(TAB_BASE + i as u32), tabs[i] == self.tab, true);
            ui::stop(f, FocusKey { entry: self.entry, elem: TAB_BASE + i as u32 }, *r, Rect::new(0.0, 0.0, 1920.0, 1080.0));
        }
        *self.tab_rects.borrow_mut() = rects;
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

/// The inbox: replies, mentions and messages.
pub struct InboxScreen {
    entry: EntryId,
    flow: Flow,
    sig: Cell<u32>,
    state: State,
}

impl InboxScreen {
    pub fn new(entry: EntryId) -> Self {
        let top = header_h();
        let mut flow = Flow::new(entry, G_LIST, LIST_BASE, Rect::new(200.0, top, 1520.0, 1080.0 - top));
        flow.row_gap = 16.0;
        InboxScreen { entry, flow, sig: Cell::new(u32::MAX), state: State("inbox".into()) }
    }

    fn relayout(&self, cx: &Cx<'_, RdHost>) {
        let st = cx.views.store;
        if self.sig.get() == st.inbox.gen {
            return;
        }
        self.sig.set(st.inbox.gen);
        let w = self.flow.col_w();
        let heights: Vec<f32> = st
            .messages
            .iter()
            .map(|m| {
                let laid = richtext::cached(&m.name, &m.body, w - 48.0, ui::csz(24));
                24.0 + ui::sz(24) as f32 * 1.42 * 2.0 + laid.h.min(ui::csz(24) as f32 * 1.42 * 6.0) + 24.0
            })
            .collect();
        self.flow.set_heights(&heights);
    }
}

impl Machine<RdHost> for InboxScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Mount => fx.push(Fx::App(RdFx::LoadInbox)),
            ScreenEvent::Async(_, RdMsg::Changed) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::FocusMoved { to, by, .. } => {
                if let Some(i) = self.flow.index(to.elem) {
                    self.flow.on_focus(i, *by == By::Pointer);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Activate(e) => {
                if let Some(m) = self.flow.index(*e).and_then(|i| cx.views.store.messages.get(i)) {
                    match m.context.as_deref().and_then(rd_core::listing::parse_reddit_link) {
                        Some(link) => super::post::open_reddit_link(link, fx),
                        None => fx.push(Fx::App(RdFx::Toast("A private message: open it on reddit.com to reply".into()))),
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

impl Focusable<RdHost> for InboxScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        self.relayout(cx);
        if self.flow.len() > 0 {
            out.push(self.flow.spec());
        }
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G_LIST)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        self.flow.neighbour(key, dir)
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        self.flow.place(*key)
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if self.flow.index(want.elem).is_some() || self.flow.len() == 0 {
            want
        } else {
            self.flow.key(0)
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        self.flow.key(0)
    }
}

impl Screen<RdHost> for InboxScreen {
    fn name(&self) -> &'static str {
        "inbox"
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
        self.relayout(cx);
        self.flow.animate(cx.views.now_ms);
        let p = f.painter;
        let st = cx.views.store;
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        if st.messages.is_empty() {
            if st.inbox.loading {
                ui::spinner(p, cx.views.now_ms, 960.0, 500.0);
            } else {
                common::message(p, f.measure, self.flow.rect, if st.inbox.error.is_some() { "Couldn't load the inbox" } else { "No messages" }, st.inbox.error.as_deref().unwrap_or_default());
            }
        }
        {
            let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(0.0, header_h() - 10.0, 1920.0, 1090.0 - header_h()));
            for i in self.flow.visible() {
                let (Some(r), Some(m)) = (self.flow.screen_rect(i), st.messages.get(i)) else { continue };
                let focused = focus == Some(self.flow.elem(i));
                ui::fill(p, r, 16.0, ui::surface(if focused { 0.17 } else { 0.07 }));
                if m.unread {
                    ui::fill(p, Rect::new(r.x, r.y + 14.0, 6.0, r.h - 28.0), 3.0, ui::accent());
                }
                let ts = ui::sz(24);
                let x = r.x + 24.0;
                let w = r.w - 48.0;
                let mut y = r.y + 16.0;
                ui::text_cut(p, f.measure, &m.subject, x, y, w, ts, ui::WHITE, ui::ALIGN_LEFT, true);
                y += ts as f32 * 1.42;
                let mut meta = format!("u/{}  \u{b7}  {}", m.author, rd_core::listing::age(m.created, rd_core::now_secs() as i64));
                if let Some(s) = &m.subreddit {
                    meta.push_str(&format!("  \u{b7}  r/{s}"));
                }
                ui::text_cut(p, f.measure, &meta, x, y, w, ts, ui::DIM, ui::ALIGN_LEFT, false);
                y += ts as f32 * 1.42;
                let laid = richtext::cached(&m.name, &m.body, w, ui::csz(24));
                let clip = Rect::new(r.x, y, r.w, r.y + r.h - y - 8.0);
                let _s = plx_ui::screen::ClipScope::open_in(p, clip);
                richtext::draw(p, &laid, x, y, clip, [0.9, 0.9, 0.92, 1.0], false);
                if focused {
                    p.rring(r, 16.0, 4.0, ui::accent());
                }
            }
        }
        self.flow.stops(f);
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, header_h()));
        ui::text(p, "Inbox", 200.0, 40.0, ui::sz(44), ui::WHITE, ui::ALIGN_LEFT, true);
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
