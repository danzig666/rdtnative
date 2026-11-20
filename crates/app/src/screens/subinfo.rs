//! A subreddit's info page: its icon, name and numbers (members, people online, age), with Join
//! or Leave, "Open r/x" and Refresh on top; its rules on the left and its description and sidebar
//! on the right. Up/Down move through a column (a long sidebar is paged through), Left/Right go
//! between the columns, BACK leaves.

use super::common::{self, State};
use crate::flow::Flow;
use crate::host::{RdArg, RdFx, RdHost, RdMsg};
use crate::images::{self, Fit};
use crate::richtext;
use crate::store::SubPage;
use crate::ui::{self, icon::Kind as Icon};
use plx_machine::machine::{By, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::Rect;
use rd_core::listing::Feed;
use rd_core::model::Rule;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};

const G_BAR: GroupId = GroupId(1);
const G_RULES: GroupId = GroupId(2);
const G_SIDE: GroupId = GroupId(3);
const RULE_BASE: u32 = 0x1000_0000;
const SIDE_BASE: u32 = 0x2000_0000;
const BAR_BASE: u32 = 0x3000_0000;

const LEFT: f32 = 80.0;
/// The rules column's width; the description and sidebar take the rest.
const RULES_W: f32 = 760.0;
const GAP: f32 = 50.0;
const PAD: f32 = 22.0;
const FULL: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bar {
    Join,
    Open,
    Refresh,
}

/// A bar button's element: by its kind, not its place, so focus stays on the button it is on
/// when Join comes in front of it (once the page says whether the user joined).
fn bar_elem(b: Bar) -> u32 {
    BAR_BASE + b as u32
}

/// The right column's cards: the short description, the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Summary,
    Sidebar,
}

fn name_size() -> i32 {
    ui::sz(44)
}

/// Where the buttons sit: under the name, the title and the numbers.
fn bar_y() -> f32 {
    36.0 + name_size() as f32 * 1.3 + ui::sz(26) as f32 * 1.42 + ui::sz(22) as f32 * 1.42 + 20.0
}

fn bar_h() -> f32 {
    ui::ls(58.0).max(52.0)
}

/// The header with the column headings under it.
fn header_h() -> f32 {
    bar_y() + bar_h() + 24.0 + ui::sz(24) as f32 * 1.6
}

fn prefs_sig() -> u64 {
    (ui::text_scale() * 100.0) as u64 | ((ui::content_scale() * 100.0) as u64) << 12 | (ui::font_code(false) as u64) << 24
}

/// A rule's tag: what it applies to, when it is not everything.
fn rule_tag(r: &Rule) -> Option<&'static str> {
    match r.kind.as_str() {
        "link" => Some("Posts only"),
        "comment" => Some("Comments only"),
        _ => None,
    }
}

pub struct SubInfoScreen {
    entry: EntryId,
    sub: String,
    from_feed: bool,
    rules: Flow,
    side: Flow,
    /// The sizes the columns were placed for.
    cols_sig: u64,
    /// (page gen, sizes) the cards were measured for.
    sig: Cell<(u32, u64)>,
    side_parts: RefCell<Vec<Side>>,
    bar_rects: RefCell<Vec<Rect>>,
    focus_req: Cell<Option<u32>>,
    /// A first load failed and focus went to Refresh.
    error_seated: Cell<bool>,
    state: State,
}

impl SubInfoScreen {
    pub fn new(entry: EntryId, sub: String, from_feed: bool) -> Self {
        let mut rules = Flow::new(entry, G_RULES, RULE_BASE, FULL);
        let mut side = Flow::new(entry, G_SIDE, SIDE_BASE, FULL);
        for f in [&mut rules, &mut side] {
            f.row_gap = 16.0;
            f.pad_top = 6.0;
            f.margin = 30.0;
        }
        let mut s = SubInfoScreen {
            entry,
            state: State(format!("subinfo {sub}")),
            sub,
            from_feed,
            rules,
            side,
            cols_sig: u64::MAX,
            sig: Cell::new((u32::MAX, 0)),
            side_parts: Default::default(),
            bar_rects: Default::default(),
            focus_req: Cell::new(None),
            error_seated: Cell::new(false),
        };
        s.place_columns();
        s
    }

    /// Places the two columns under the header (again when the text size changed).
    fn place_columns(&mut self) {
        let sig = prefs_sig();
        if sig == self.cols_sig {
            return;
        }
        self.cols_sig = sig;
        let top = header_h();
        self.rules.rect = Rect::new(LEFT, top, RULES_W, 1080.0 - top);
        let x = LEFT + RULES_W + GAP;
        self.side.rect = Rect::new(x, top, 1920.0 - LEFT - x, 1080.0 - top);
        self.sig.set((u32::MAX, 0));
    }

    fn page<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a SubPage> {
        cx.views.store.sub_pages.get(&self.sub.to_ascii_lowercase())
    }

    /// The subreddit's name as Reddit writes it, once known.
    fn display_name(&self, cx: &Cx<'_, RdHost>) -> String {
        self.page(cx).and_then(|p| p.info.as_ref()).map(|i| i.sub.display_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| self.sub.clone())
    }

    fn joined(&self, cx: &Cx<'_, RdHost>) -> bool {
        let st = cx.views.store;
        match st.about.get(&self.sub.to_ascii_lowercase()) {
            Some(a) => a.subscribed,
            None => self.page(cx).and_then(|p| p.info.as_ref()).is_some_and(|i| i.subscribed),
        }
    }

    fn bar_items(&self, cx: &Cx<'_, RdHost>) -> Vec<Bar> {
        let mut v = Vec::new();
        // Join shows once the page says whether the user already did.
        if cx.views.store.signed_in() && self.page(cx).is_some_and(|p| p.info.is_some()) {
            v.push(Bar::Join);
        }
        v.extend([Bar::Open, Bar::Refresh]);
        v
    }

    fn bar_label(&self, b: Bar, cx: &Cx<'_, RdHost>) -> (String, Icon, bool) {
        match b {
            Bar::Join if self.joined(cx) => (crate::i18n::tr("Leave").into(), Icon::Check, true),
            Bar::Join => (crate::i18n::tr("Join").into(), Icon::Check, false),
            Bar::Open => (crate::i18n::tr("Open r/{}").replace("{}", &self.display_name(cx)), Icon::Subs, false),
            Bar::Refresh => (crate::i18n::tr("Refresh").into(), Icon::Repeat, false),
        }
    }

    fn activate_bar(&mut self, elem: u32, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some(b) = self.bar_items(cx).into_iter().find(|b| bar_elem(*b) == elem) else { return };
        match b {
            Bar::Join => fx.push(Fx::App(RdFx::Subscribe { sub: self.sub.clone(), on: !self.joined(cx) })),
            // Opened from the subreddit's own feed: back to it, not a second copy.
            Bar::Open if self.from_feed => fx.push(Fx::Nav(NavOp::Pop)),
            Bar::Open => fx.push(Fx::Nav(NavOp::Push(RdArg::Feed(Feed::Sub(self.display_name(cx)))))),
            Bar::Refresh => fx.push(Fx::App(RdFx::LoadSubInfo { sub: self.sub.clone(), reload: true })),
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn rule_title(i: usize, r: &Rule) -> String {
        format!("{}. {}", i + 1, r.short_name.trim())
    }

    fn rule_laid(&self, i: usize, r: &Rule, w: f32) -> std::rc::Rc<richtext::Laid> {
        richtext::cached(&format!("rule:{}:{i}:{}", self.sub, r.description.len()), &r.description, w - 2.0 * PAD, ui::csz(23))
    }

    fn rule_h(&self, i: usize, r: &Rule, w: f32) -> f32 {
        let ts = ui::sz(26);
        let n = richtext::wrap_title(&Self::rule_title(i, r), w - 2.0 * PAD, ts, true, 3).len().max(1);
        let mut h = PAD + richtext::title_h(n, ts);
        if rule_tag(r).is_some() {
            h += 8.0 + ui::sz(20) as f32 * 1.45;
        }
        if !r.description.trim().is_empty() {
            h += 10.0 + self.rule_laid(i, r, w).h;
        }
        h + PAD
    }

    fn side_laid(&self, part: Side, cx: &Cx<'_, RdHost>, w: f32) -> std::rc::Rc<richtext::Laid> {
        let info = self.page(cx).and_then(|p| p.info.as_ref());
        match part {
            Side::Summary => {
                let text = info.map_or("", |i| i.sub.public_description.as_str());
                richtext::cached(&format!("subsum:{}:{}", self.sub, text.len()), text, w - 2.0 * PAD, ui::csz(26))
            }
            Side::Sidebar => {
                let text = info.map_or("", |i| i.sidebar.as_str());
                richtext::cached(&format!("sidebar:{}:{}", self.sub, text.len()), text, w - 2.0 * PAD, ui::csz(23))
            }
        }
    }

    /// Measures the cards when the page or the sizes changed.
    fn relayout(&self, cx: &Cx<'_, RdHost>) {
        let page = self.page(cx);
        let sig = (page.map_or(0, |p| p.gen), prefs_sig());
        if sig == self.sig.get() {
            return;
        }
        self.sig.set(sig);
        let info = page.and_then(|p| p.info.as_ref());
        let w = self.rules.col_w();
        let rules: Vec<f32> = match (info, page) {
            (Some(_), Some(p)) => p.rules.iter().enumerate().map(|(i, r)| self.rule_h(i, r, w)).collect(),
            _ => Vec::new(),
        };
        self.rules.set_heights(&rules);
        let mut parts = Vec::new();
        if let Some(i) = info {
            if !i.sub.public_description.trim().is_empty() {
                parts.push(Side::Summary);
            }
            if !i.sidebar.trim().is_empty() {
                parts.push(Side::Sidebar);
            }
        }
        let w = self.side.col_w();
        let heights: Vec<f32> = parts.iter().map(|p| self.side_laid(*p, cx, w).h + 2.0 * PAD).collect();
        self.side.set_heights(&heights);
        *self.side_parts.borrow_mut() = parts;
    }

    /// The first card of the column with any, from the top of what is in view.
    fn first_content(&self) -> Option<FocusKey<u32>> {
        [&self.rules, &self.side].into_iter().find(|f| f.len() > 0).map(|f| f.key(f.visible().start.min(f.len() - 1)))
    }

    fn bar_key(&self, b: Bar) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: bar_elem(b) }
    }

    /// The bar's first button as things stand.
    fn first_bar(&self, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        self.bar_key(self.bar_items(cx).first().copied().unwrap_or(Bar::Open))
    }

    /// Where button `elem` sits on the bar now (`None`: not on it).
    fn bar_pos(&self, elem: u32, cx: &Cx<'_, RdHost>) -> Option<usize> {
        self.bar_items(cx).iter().position(|b| bar_elem(*b) == elem)
    }
}

impl Machine<RdHost> for SubInfoScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        self.place_columns();
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        // The pointer edge glide: the column under the pointer scrolls by pixels.
        if let Some(dy) = common::wheel_dy(ev) {
            let (x, _) = ui::pointer();
            let flow = if self.rules.contains_x(x) && !self.side.contains_x(x) { &self.rules } else { &self.side };
            if flow.glide(dy) {
                if let Some(j) = flow.focused(cx).and_then(|i| flow.keep_in_view(i)) {
                    self.focus_req.set(Some(flow.elem(j)));
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        match ev {
            ScreenEvent::Mount => fx.push(Fx::App(RdFx::LoadSubInfo { sub: self.sub.clone(), reload: false })),
            ScreenEvent::Uncover => {
                // Signing in or out meanwhile dropped what this page showed: load it again.
                if self.page(cx).is_none() {
                    fx.push(Fx::App(RdFx::LoadSubInfo { sub: self.sub.clone(), reload: false }));
                }
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            ScreenEvent::Async(_, RdMsg::Changed) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::FocusMoved { to, by, .. } => {
                if let Some(i) = self.rules.index(to.elem) {
                    self.rules.on_focus(i, *by == By::Pointer);
                } else if let Some(i) = self.side.index(to.elem) {
                    self.side.on_focus(i, *by == By::Pointer);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Activate(e) => {
                if *e >= BAR_BASE {
                    self.activate_bar(*e, cx, fx);
                }
                return Handled::Yes;
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<RdHost> for SubInfoScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        self.relayout(cx);
        out.push(GroupSpec {
            id: G_BAR,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(LEFT, bar_y(), 1920.0 - 2.0 * LEFT, bar_h()),
            len: self.bar_items(cx).len(),
            elem: ElemKind::Control,
        });
        for f in [&self.rules, &self.side] {
            if f.len() > 0 {
                out.push(f.spec());
            }
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if *key >= BAR_BASE {
            G_BAR
        } else if *key >= SIDE_BASE {
            G_SIDE
        } else {
            G_RULES
        })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        let first = |f: &Flow| (f.len() > 0).then(|| f.key(f.visible().start.min(f.len() - 1)));
        if key.elem >= BAR_BASE {
            let items = self.bar_items(cx);
            let Some(i) = items.iter().position(|b| bar_elem(*b) == key.elem) else { return Step::Edge };
            return match dir {
                Dir::Left if i > 0 => Step::Move(self.bar_key(items[i - 1])),
                Dir::Right if i + 1 < items.len() => Step::Move(self.bar_key(items[i + 1])),
                Dir::Down => self.first_content().map_or(Step::Edge, Step::Move),
                _ => Step::Edge,
            };
        }
        let (flow, other) = if self.rules.index(key.elem).is_some() { (&self.rules, &self.side) } else { (&self.side, &self.rules) };
        match (dir, flow.neighbour(key, dir)) {
            (Dir::Up, Step::Edge) => Step::Move(self.first_bar(cx)),
            (Dir::Left | Dir::Right, _) => {
                // Across to the other column: Right from the rules, Left from the sidebar.
                let across = (dir == Dir::Right) == std::ptr::eq(flow, &self.rules);
                match first(other) {
                    Some(k) if across => Step::Move(k),
                    _ => Step::Edge,
                }
            }
            (_, s) => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        if *key >= BAR_BASE {
            let i = self.bar_pos(*key, cx)?;
            let r = *self.bar_rects.borrow().get(i)?;
            return Some(Placed { rect: r, rest_rect: r, clip: FULL, index: Some(i as u32) });
        }
        if *key >= SIDE_BASE {
            return self.side.place(*key);
        }
        self.rules.place(*key)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let want = self.focus_req.take().map_or(want, |e| FocusKey { entry: self.entry, elem: e });
        // A page that failed to load: Refresh, which tries again.
        let failed = self.page(cx).is_some_and(|p| p.info.is_none() && p.error.is_some() && !p.loading);
        if failed && !self.error_seated.replace(true) {
            return self.bar_key(Bar::Refresh);
        }
        if !failed {
            self.error_seated.set(false);
        }
        let valid = if want.elem >= BAR_BASE {
            self.bar_pos(want.elem, cx).is_some()
        } else if want.elem >= SIDE_BASE {
            self.side.index(want.elem).is_some()
        } else {
            self.rules.index(want.elem).is_some()
        };
        if valid {
            return want;
        }
        self.first_bar(cx)
    }
    fn seat(&self, g: GroupId, _from: Placed, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let first = |f: &Flow| (f.len() > 0).then(|| f.key(f.visible().start.min(f.len() - 1)));
        match g {
            G_RULES => first(&self.rules),
            G_SIDE => first(&self.side),
            _ => None,
        }
        .unwrap_or_else(|| self.first_bar(cx))
    }
}

impl SubInfoScreen {
    fn draw_header(&self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let m = f.measure;
        let cx = f.cx;
        let info = self.page(cx).and_then(|p| p.info.as_ref());
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, header_h() + 10.0));
        let ns = name_size();
        let (ts, ms) = (ui::sz(26), ui::sz(22));
        // The icon spans the name, title and numbers lines.
        let isz = (bar_y() - 20.0 - 36.0).round();
        let ir = Rect::new(LEFT, 36.0, isz, isz);
        ui::fill(p, ir, isz / 2.0, ui::with_alpha(ui::accent(), 0.85));
        let icon = info.and_then(|i| i.sub.icon.as_ref()).map(|u| rd_core::media::unescape(u));
        let drew = icon.is_some_and(|u| matches!(images::draw(p, images::THUMB, &u, ir, isz / 2.0, Fit::Cover, ui::WHITE), images::Drawn::At(_)));
        if !drew {
            ui::text_raw(p, "r/", ir.x + isz / 2.0, ir.y + (isz - ui::sz(40) as f32 * 1.3) / 2.0, ui::sz(40), ui::WHITE, ui::ALIGN_CENTER, true);
        }
        let x = ir.x + isz + 28.0;
        let w = 1920.0 - LEFT - x - if cx.views.store.prefs.on("clock") { 140.0 } else { 0.0 };
        let mut y = 36.0;
        ui::text_cut(p, m, &format!("r/{}", self.display_name(cx)), x, y, w, ns, ui::WHITE, ui::ALIGN_LEFT, true);
        y += ns as f32 * 1.3;
        if let Some(i) = info {
            ui::text_cut(p, m, &i.sub.title, x, y, w, ts, ui::DIM, ui::ALIGN_LEFT, false);
            y += ts as f32 * 1.42;
            let tr = crate::i18n::tr;
            let mut facts = vec![tr("{} members").replace("{}", &common::short_count(i.sub.subscribers))];
            if i.active > 0 {
                facts.push(tr("{} online").replace("{}", &common::short_count(i.active)));
            }
            if i.created_utc > 0 {
                let (yy, mm, dd) = rd_core::listing::utc_date(i.created_utc);
                facts.push(tr("Created {}").replace("{}", &crate::i18n::full_date(yy, mm, dd)));
            }
            let mut fx = x;
            if i.sub.over18 {
                fx += common::badge(p, m, fx, y + 1.0, "NSFW", common::NSFW_RED, ui::WHITE, (ms as f32 * 0.85) as i32) + 12.0;
            }
            ui::text_cut(p, m, &facts.join("  \u{b7}  "), fx, y, x + w - fx, ms, ui::FAINT, ui::ALIGN_LEFT, false);
        }
        if cx.views.store.prefs.on("clock") {
            common::clock(p, 1920.0 - LEFT, 40.0);
        }
        // The buttons.
        let items = self.bar_items(cx);
        let labels: Vec<(String, Icon, bool)> = items.iter().map(|b| self.bar_label(*b, cx)).collect();
        let rects = common::button_row(m, &labels.iter().map(|(l, _, _)| (l.as_str(), true)).collect::<Vec<_>>(), LEFT, bar_y(), bar_h(), 14.0, None);
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for ((b, r), (l, icon, active)) in items.iter().zip(rects.iter()).zip(labels.iter()) {
            common::button(p, m, *r, l, Some(*icon), focus == Some(bar_elem(*b)), *active, true);
            ui::stop(f, self.bar_key(*b), *r, FULL);
        }
        *self.bar_rects.borrow_mut() = rects;
        // The column headings, once there is a page to head.
        if info.is_some() {
            let hy = header_h() - ui::sz(24) as f32 * 1.6;
            ui::text(p, "Rules", self.rules.rect.x, hy, ui::sz(24), ui::DIM, ui::ALIGN_LEFT, true);
            ui::text(p, "About this community", self.side.rect.x, hy, ui::sz(24), ui::DIM, ui::ALIGN_LEFT, true);
        }
    }

    fn draw_card(p: plx_ui::Painter, r: Rect, focused: bool) {
        if focused {
            ui::fill(p, r, 16.0, ui::surface(0.14));
            p.rring(r, 16.0, 3.0, ui::with_alpha(ui::accent(), 0.9));
        } else {
            ui::fill(p, r, 16.0, ui::surface(0.06));
        }
    }

    fn draw_rules(&self, f: &mut DrawFrame<'_, '_, RdHost>, rules: &[Rule], focus: Option<u32>) {
        let p = f.painter;
        let m = f.measure;
        let area = self.rules.rect;
        if rules.is_empty() {
            ui::text(p, "No rules listed", area.x + PAD, area.y + PAD, ui::sz(24), ui::FAINT, ui::ALIGN_LEFT, false);
            return;
        }
        let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(area.x - 10.0, area.y - 6.0, area.w + 20.0, area.h + 6.0));
        let ts = ui::sz(26);
        for i in self.rules.visible() {
            let (Some(r), Some(rule)) = (self.rules.screen_rect(i), rules.get(i)) else { continue };
            Self::draw_card(p, r, focus == Some(self.rules.elem(i)));
            let mut y = r.y + PAD;
            let lines = richtext::wrap_title(&Self::rule_title(i, rule), r.w - 2.0 * PAD, ts, true, 3);
            y = richtext::draw_lines(p, m, &lines, r.x + PAD, y, r.w - 2.0 * PAD, ts, ui::WHITE, true);
            if let Some(tag) = rule_tag(rule) {
                let bs = ui::sz(20);
                common::badge(p, m, r.x + PAD, y + 8.0, crate::i18n::tr(tag), ui::with_alpha(ui::accent(), 0.3), ui::WHITE, bs);
                y += 8.0 + bs as f32 * 1.45;
            }
            if !rule.description.trim().is_empty() {
                let laid = self.rule_laid(i, rule, r.w);
                richtext::draw(p, &laid, r.x + PAD, y + 10.0, area, [0.86, 0.86, 0.9, 1.0], false);
            }
        }
    }

    fn draw_side(&self, f: &mut DrawFrame<'_, '_, RdHost>, focus: Option<u32>) {
        let p = f.painter;
        let cx = f.cx;
        let area = self.side.rect;
        let parts = self.side_parts.borrow().clone();
        if parts.is_empty() {
            ui::text(p, "This subreddit has no description.", area.x + PAD, area.y + PAD, ui::sz(24), ui::FAINT, ui::ALIGN_LEFT, false);
            return;
        }
        let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(area.x - 10.0, area.y - 6.0, area.w + 20.0, area.h + 6.0));
        for i in self.side.visible() {
            let (Some(r), Some(part)) = (self.side.screen_rect(i), parts.get(i)) else { continue };
            Self::draw_card(p, r, focus == Some(self.side.elem(i)));
            let laid = self.side_laid(*part, cx, r.w);
            let ink = if *part == Side::Summary { ui::WHITE } else { [0.88, 0.88, 0.92, 1.0] };
            richtext::draw(p, &laid, r.x + PAD, r.y + PAD, area, ink, false);
        }
    }
}

impl Screen<RdHost> for SubInfoScreen {
    fn name(&self) -> &'static str {
        "subinfo"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        self.place_columns();
        let cx = f.cx;
        self.relayout(cx);
        self.rules.animate(cx.views.now_ms);
        self.side.animate(cx.views.now_ms);
        let p = f.painter;
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        let page = self.page(cx);
        let body = Rect::new(LEFT, header_h(), 1920.0 - 2.0 * LEFT, 1080.0 - header_h());
        match page {
            Some(pg) if pg.info.is_some() => {
                self.draw_rules(f, &pg.rules, focus);
                self.draw_side(f, focus);
                self.rules.stops(f);
                self.side.stops(f);
                if pg.loading {
                    ui::spinner(p, cx.views.now_ms, 1920.0 - LEFT - 40.0, bar_y() + bar_h() / 2.0);
                }
            }
            Some(pg) if pg.error.is_some() && !pg.loading => {
                let detail = format!("{}  {}", pg.error.as_deref().unwrap_or_default(), crate::i18n::tr("OK on Refresh tries again; BACK goes back."));
                common::message(p, f.measure, body, &crate::i18n::tr("Couldn't load r/{}").replace("{}", &self.sub), &detail);
            }
            _ => ui::spinner(p, cx.views.now_ms, 960.0, (body.y + 1080.0) / 2.0 - 60.0),
        }
        self.draw_header(f);
        common::toast(f.painter, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
