//! The on-screen keyboard: searches, replies, subreddit names, filters, the Imgur client ID, a
//! sign-in code and the Reddit API fields. It has a caps lock and a symbols page (client IDs mix
//! cases; URLs and user agents need `:/.?=&()`). Done uses the text; BACK leaves without it.
//! Searching shows the search history and favourite subreddits beside the keys. Layout and pointer
//! handling after ytnative's search keyboard (keys tile the board so a Magic Remote press never
//! lands in a gap).

use super::common::{self, State};
use crate::host::{RdArg, RdFx, RdHost, Typing};
use crate::store::Field;
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::Rect;
use rd_core::listing::Feed;
use std::borrow::Cow;

const G_KEYS: GroupId = GroupId(1);
const G_SUGGEST: GroupId = GroupId(2);
const KEY_BASE: u32 = 0x1000_0000;
const SUG_BASE: u32 = 0x2000_0000;

const COLS: usize = 12;
const KEY_W: f32 = 86.0;
const KEY_H: f32 = 80.0;
const KEY_GAP: f32 = 10.0;
const KEYS_X: f32 = 100.0;
const KEYS_Y: f32 = 380.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Ch(char),
    Caps,
    Symbols,
    Space,
    Back,
    Clear,
    Enter,
    Done,
}

const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
/// As many symbols as letters, so both pages keep the same grid.
const SYMBOLS: &str = "!?=&%()@+,;~#$*'\"<>[]{}|^`";
const ALWAYS: &str = "-_.:/";

fn keys(symbols: bool, enter: bool) -> Vec<K> {
    let mut v: Vec<K> = if symbols { SYMBOLS } else { LETTERS }.chars().map(K::Ch).collect();
    v.extend("1234567890".chars().map(K::Ch));
    v.extend(ALWAYS.chars().map(K::Ch));
    v.extend([K::Caps, K::Symbols, K::Space, K::Back, K::Clear]);
    if enter {
        v.push(K::Enter);
    }
    v.push(K::Done);
    v
}

fn label(k: K, caps: bool, symbols: bool) -> String {
    match k {
        K::Ch(c) if caps && c.is_ascii_lowercase() => c.to_ascii_uppercase().to_string(),
        K::Ch(c) => c.to_string(),
        K::Caps if caps => "abc".into(),
        K::Caps => "ABC".into(),
        K::Symbols if symbols => "abc".into(),
        K::Symbols => "#+=".into(),
        K::Space => "Space".into(),
        K::Back => "\u{232B}".into(),
        K::Clear => "Clear".into(),
        K::Enter => "\u{21B5}".into(),
        K::Done => "Done".into(),
    }
}

/// What the keyboard is for: title, help, the hint when empty, the longest text.
fn spec(t: &Typing) -> (String, String, String, usize) {
    match t {
        Typing::Api(f) => {
            let s = f.spec(&Default::default());
            let hint = if s.placeholder.is_empty() { String::new() } else { format!("{} {}", crate::i18n::tr("Empty uses:"), s.placeholder) };
            (s.label.to_string(), s.help.to_string(), hint, s.max_len)
        }
        Typing::Search { sub: None } => ("Search Reddit".into(), "Posts from all of Reddit. Done searches.".into(), "What are you looking for?".into(), 200),
        Typing::Search { sub: Some(s) } => (format!("Search r/{s}"), "Posts in this subreddit. Done searches.".into(), "What are you looking for?".into(), 200),
        Typing::GoToSub => ("Go to a subreddit".into(), "Type its name, without r/.".into(), "pics".into(), 30),
        Typing::FindSub => ("Find subreddits".into(), "A topic or a name. Done shows the matching subreddits.".into(), "photography".into(), 100),
        Typing::ImgurId => (
            "Imgur client ID".into(),
            "From api.imgur.com/oauth2/addclient (anonymous use). Empty uses the built-in one.".into(),
            format!("{} {}", crate::i18n::tr("Empty uses:"), crate::prefs::DEFAULT_IMGUR_ID),
            40,
        ),
        Typing::FilterWord => ("Hide posts with a word".into(), "Posts whose title has it are left out of every feed.".into(), "spoiler".into(), 60),
        Typing::FilterSub => ("Hide a subreddit".into(), "Its posts are left out of Home, Popular and All.".into(), "subreddit name".into(), 30),
        Typing::Reply { .. } => ("Reply".into(), "\u{21B5} starts a new line. Done sends it.".into(), "Write a reply\u{2026}".into(), 10_000),
        Typing::SignInCode => ("Sign-in code".into(), "The address of the page Reddit sent you to, or the code on it.".into(), "https://\u{2026}?code=\u{2026}".into(), 2048),
        Typing::IdleSub => ("Idle screen subreddit".into(), "Its pictures fill the screen when the TV is idle. Empty uses r/EarthPorn.".into(), crate::prefs::DEFAULT_IDLE_SUB.into(), 30),
    }
}

pub struct KeyboardScreen {
    entry: EntryId,
    target: Typing,
    text: String,
    caps: bool,
    symbols: bool,
    suggest: VList,
    state: State,
}

/// A suggestion beside the keyboard.
#[derive(Clone, Debug)]
enum Suggestion {
    Query(String),
    Sub(String),
}

impl KeyboardScreen {
    pub fn new(entry: EntryId, target: Typing, current: &str) -> Self {
        KeyboardScreen {
            entry,
            target,
            text: current.to_string(),
            caps: false,
            symbols: false,
            suggest: VList { entry, group: G_SUGGEST, base: SUG_BASE, rect: Rect::new(1330.0, KEYS_Y, 500.0, 6.0 * 64.0), row_h: 64.0, text_size: 26 },
            state: State(current.to_string()),
        }
    }

    fn keys(&self) -> Vec<K> {
        keys(self.symbols, matches!(self.target, Typing::Reply { .. }))
    }

    fn key_rect(i: usize) -> Rect {
        Rect::new(KEYS_X + (i % COLS) as f32 * (KEY_W + KEY_GAP), KEYS_Y + (i / COLS) as f32 * (KEY_H + KEY_GAP), KEY_W, KEY_H)
    }

    fn kkey(&self, i: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: KEY_BASE + i as u32 }
    }

    fn suggestions(&self, cx: &Cx<'_, RdHost>) -> Vec<Suggestion> {
        let p = &cx.views.store.prefs;
        let t = self.text.to_lowercase();
        let rows = self.suggest.rows_visible();
        match self.target {
            Typing::Search { .. } => {
                let mut v: Vec<Suggestion> = p.history.iter().filter(|h| t.is_empty() || h.to_lowercase().contains(&t)).take(rows).cloned().map(Suggestion::Query).collect();
                if !t.is_empty() {
                    v.extend(p.favorites.iter().filter(|f| f.to_lowercase().contains(&t)).take(3).cloned().map(Suggestion::Sub));
                }
                v.truncate(rows);
                v
            }
            Typing::GoToSub => p.favorites.iter().filter(|f| t.is_empty() || f.to_lowercase().contains(&t)).take(rows).cloned().map(Suggestion::Sub).collect(),
            _ => Vec::new(),
        }
    }

    /// The problem with the text as typed (API fields), by the rules Save and the phone use.
    fn problem(&self) -> Option<String> {
        let t = self.text.trim();
        if t.is_empty() {
            return None;
        }
        let e = match self.target {
            Typing::Api(Field::ClientId) => rd_core::auth::ClientId::parse(t).err(),
            Typing::Api(Field::RedirectUri) => rd_core::config::check_redirect_uri(t).err(),
            Typing::Api(Field::UserAgent) => rd_core::config::check_user_agent(t).err(),
            Typing::GoToSub | Typing::FilterSub | Typing::IdleSub => {
                let n = t.trim_start_matches("r/").trim_start_matches("/r/");
                (!n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| "Subreddit names have letters, digits and _ only".to_string())
            }
            _ => None,
        }?;
        let mut c = e.chars();
        Some(c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default())
    }

    fn done(&mut self, text: String, fx: &mut Effects<'_, RdHost>) {
        let t = text.trim().to_string();
        match &self.target {
            Typing::Api(f) => {
                fx.push(Fx::App(RdFx::SetField(*f, t)));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Typing::Search { sub } => {
                if t.is_empty() {
                    fx.push(Fx::App(RdFx::Toast("Type something to search for".into())));
                    return;
                }
                fx.push(Fx::App(RdFx::Typed(self.target.clone(), t.clone())));
                fx.push(Fx::Nav(NavOp::Replace(RdArg::Feed(Feed::Search { q: t, sub: sub.clone() }))));
            }
            Typing::GoToSub => {
                let name = t.trim_start_matches("/r/").trim_start_matches("r/").to_string();
                if name.is_empty() {
                    return;
                }
                fx.push(Fx::Nav(NavOp::Replace(RdArg::Feed(Feed::Sub(name)))));
            }
            Typing::Reply { .. } if t.is_empty() => {
                fx.push(Fx::App(RdFx::Toast("The reply is empty".into())));
            }
            _ => {
                fx.push(Fx::App(RdFx::Typed(self.target.clone(), t)));
                fx.push(Fx::Nav(NavOp::Pop));
            }
        }
    }

    fn press(&mut self, k: K, fx: &mut Effects<'_, RdHost>) {
        let max = spec(&self.target).3;
        match k {
            K::Ch(c) if self.text.chars().count() < max => {
                self.text.push(if self.caps { c.to_ascii_uppercase() } else { c });
            }
            K::Ch(_) => fx.push(Fx::App(RdFx::Toast("That is as long as this text gets".into()))),
            K::Caps => self.caps = !self.caps,
            K::Symbols => self.symbols = !self.symbols,
            K::Space => self.text.push(' '),
            K::Enter => self.text.push('\n'),
            K::Back => {
                self.text.pop();
            }
            K::Clear => self.text.clear(),
            K::Done => {
                if self.problem().is_some() {
                    fx.push(Fx::App(RdFx::Toast("Fix the text first (see the message under it)".into())));
                    return;
                }
                let t = self.text.clone();
                self.done(t, fx);
                return;
            }
        }
        self.state = State(self.text.clone());
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<RdHost> for KeyboardScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        // A USB keyboard (or the PC's in the simulator): typed characters go straight in.
        if let ScreenEvent::Input(plx_machine::machine::InputEvent {
            kind: plx_machine::machine::InputKind::Key { key: plx_machine::machine::Key::Other, sym, edge, .. }, ..
        }) = ev {
            if matches!(edge, plx_machine::machine::Edge::Down | plx_machine::machine::Edge::Repeat) {
                if let Some(c) = char::from_u32(*sym).filter(|c| !c.is_control() && *sym < 0x4000_0000) {
                    if self.text.chars().count() < spec(&self.target).3 {
                        self.text.push(c);
                        self.state = State(self.text.clone());
                        fx.invalidate(plx_machine::present::Provenance::Input);
                    }
                    return Handled::Yes;
                }
            }
        }
        match ev {
            ScreenEvent::Activate(e) if *e >= SUG_BASE => {
                let sug = self.suggestions(cx);
                if let Some(s) = self.suggest.index(*e, sug.len()).map(|i| sug[i].clone()) {
                    match s {
                        Suggestion::Query(q) => self.done(q, fx),
                        Suggestion::Sub(name) => fx.push(Fx::Nav(NavOp::Replace(RdArg::Feed(Feed::Sub(name))))),
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) if *e >= KEY_BASE => {
                if let Some(k) = self.keys().get((e - KEY_BASE) as usize).copied() {
                    self.press(k, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<RdHost> for KeyboardScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        let n = self.keys().len();
        out.push(GroupSpec {
            id: G_KEYS,
            kind: GroupKind::Grid { cols: COLS, holes: &[] },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(KEYS_X, KEYS_Y, COLS as f32 * (KEY_W + KEY_GAP), n.div_ceil(COLS) as f32 * (KEY_H + KEY_GAP)),
            len: n,
            elem: ElemKind::Control,
        });
        let s = self.suggestions(cx).len();
        if s > 0 {
            out.push(self.suggest.spec(s));
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if *key >= SUG_BASE { G_SUGGEST } else { G_KEYS })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        if key.elem >= SUG_BASE {
            let n = self.suggestions(cx).len();
            if dir == Dir::Left {
                return Step::Move(self.kkey(COLS - 1));
            }
            return self.suggest.neighbour(key, dir, n);
        }
        let n = self.keys().len();
        let i = (key.elem - KEY_BASE) as usize;
        let (r, c) = (i / COLS, i % COLS);
        let to = match dir {
            Dir::Left if c > 0 => Some(i - 1),
            Dir::Right if c + 1 < COLS && i + 1 < n => Some(i + 1),
            Dir::Right if c + 1 == COLS => {
                let s = self.suggestions(cx).len();
                if s > 0 {
                    return Step::Move(self.suggest.key(r.min(s - 1)));
                }
                None
            }
            Dir::Up if r > 0 => Some(i - COLS),
            Dir::Down if i + COLS < n => Some(i + COLS),
            // Down from a column the short last row lacks: its last key (Done).
            Dir::Down if r + 1 < n.div_ceil(COLS) => Some(n - 1),
            _ => None,
        };
        to.map_or(Step::Edge, |j| Step::Move(self.kkey(j)))
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        if *key >= SUG_BASE {
            let n = self.suggestions(cx).len();
            return self.suggest.place(*key, n, self.suggest.focused(cx, n));
        }
        let i = key.checked_sub(KEY_BASE)? as usize;
        (i < self.keys().len()).then(|| {
            let r = Self::key_rect(i);
            Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(i as u32) }
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if want.elem >= SUG_BASE {
            let n = self.suggestions(cx).len();
            if self.suggest.index(want.elem, n).is_some() {
                return want;
            }
            return self.kkey(0);
        }
        if want.elem < KEY_BASE || (want.elem - KEY_BASE) as usize >= self.keys().len() {
            return self.kkey(0);
        }
        want
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if g == G_SUGGEST {
            return self.suggest.key(0);
        }
        self.kkey(0)
    }
}

impl Screen<RdHost> for KeyboardScreen {
    fn name(&self) -> &'static str {
        "keyboard"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let cx = f.cx;
        let keys = self.keys();
        for i in 0..keys.len() {
            let r = Self::key_rect(i);
            let half = KEY_GAP / 2.0;
            ui::stop(f, self.kkey(i), Rect::new(r.x - half, r.y - half, r.w + KEY_GAP, r.h + KEY_GAP), Rect::new(0.0, 0.0, 1920.0, 1080.0));
        }
        let (title, help, hint, max) = spec(&self.target);
        ui::text_raw(p, crate::i18n::tr(&title), 100.0, 60.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);

        // The text with a cursor (the last lines of a long reply), or the hint when empty.
        let multi = matches!(self.target, Typing::Reply { .. });
        let field = Rect::new(100.0, 140.0, 1720.0, if multi { 160.0 } else { 100.0 });
        let problem = self.problem();
        ui::fill(p, field, 18.0, [1.0, 1.0, 1.0, 0.08]);
        if problem.is_some() {
            ui::fill(p, Rect::new(field.x, field.y + field.h - 4.0, field.w, 4.0), 2.0, [1.0, 0.45, 0.4, 1.0]);
        }
        let room = field.w - 72.0;
        if self.text.is_empty() {
            ui::text_cut(p, f.measure, crate::i18n::tr(&hint), field.x + 36.0, field.y + 28.0, room, 36, ui::FAINT, ui::ALIGN_LEFT, false);
        } else if multi {
            let mut lines: Vec<String> = Vec::new();
            for para in format!("{}|", self.text).split('\n') {
                let w = ui::wrap_lines(f.measure, para, room, 30, false, 50);
                if w.is_empty() {
                    lines.push(String::new());
                }
                lines.extend(w);
            }
            let start = lines.len().saturating_sub(3);
            for (i, l) in lines[start..].iter().enumerate() {
                ui::text_raw(p, l, field.x + 36.0, field.y + 18.0 + i as f32 * 44.0, 30, ui::WHITE, ui::ALIGN_LEFT, false);
            }
        } else {
            ui::text_cut_start(p, f.measure, &format!("{}|", self.text), field.x + 36.0, field.y + 28.0, room, 36, ui::WHITE, false);
        }
        let below = field.y + field.h + 14.0;
        if max < 10_000 {
            ui::text_raw(p, &format!("{} / {}", self.text.chars().count(), max), field.x + field.w, below, 22, ui::FAINT, ui::ALIGN_RIGHT, false);
        }
        match &problem {
            Some(e) => ui::text_cut(p, f.measure, e, field.x, below, field.w - 200.0, 26, [1.0, 0.55, 0.5, 1.0], ui::ALIGN_LEFT, true),
            None => ui::text_cut(p, f.measure, crate::i18n::tr(&help), field.x, below, field.w - 200.0, 24, ui::DIM, ui::ALIGN_LEFT, false),
        };

        // Keyboard
        let focused = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        let keys_y_shift = if multi { 0.0 } else { 0.0 };
        for (i, k) in keys.into_iter().enumerate() {
            let mut r = Self::key_rect(i);
            r.y += keys_y_shift;
            let is_focus = focused == Some(KEY_BASE + i as u32);
            let lit = (k == K::Caps && self.caps) || (k == K::Symbols && self.symbols);
            let bg = if is_focus {
                ui::WHITE
            } else if k == K::Done {
                ui::accent()
            } else if lit {
                [1.0, 1.0, 1.0, 0.28]
            } else {
                [1.0, 1.0, 1.0, 0.10]
            };
            ui::fill(p, r, 14.0, bg);
            let size = if matches!(k, K::Ch(_)) { 34 } else { 22 };
            let ink = if is_focus { [0.0, 0.0, 0.0, 1.0] } else { ui::WHITE };
            let text = label(k, self.caps, self.symbols);
            let ty = r.y + (r.h - size as f32 * 1.25) / 2.0;
            if matches!(k, K::Ch(_)) {
                ui::text_raw(p, &text, r.x + r.w / 2.0, ty, size, ink, ui::ALIGN_CENTER, true);
            } else {
                ui::text(p, &text, r.x + r.w / 2.0, ty, size, ink, ui::ALIGN_CENTER, true);
            }
        }
        // Suggestions.
        let sug = self.suggestions(cx);
        if !sug.is_empty() {
            let heading = if matches!(self.target, Typing::GoToSub) { "Favourites" } else { "Recent searches" };
            ui::text(p, heading, self.suggest.rect.x + 10.0, KEYS_Y - 50.0, 24, ui::FAINT, ui::ALIGN_LEFT, true);
            let rows: Vec<Row> = sug
                .iter()
                .map(|s| match s {
                    Suggestion::Query(q) => Row::new(q.clone()).icon(ui::icon::Kind::History).raw(),
                    Suggestion::Sub(n) => Row::new(format!("r/{n}")).icon(ui::icon::Kind::Subs).raw(),
                })
                .collect();
            self.suggest.draw(p, f.measure, &rows, self.suggest.focused(cx, rows.len()));
            self.suggest.stops(f, rows.len());
        }
        let hint = if matches!(self.target, Typing::Api(_)) {
            "Done keeps the text. BACK leaves it unchanged. Or use \u{201C}Enter on phone\u{201D} to paste it."
        } else {
            "Done uses the text. BACK leaves without it. A USB keyboard works too."
        };
        ui::text(p, hint, 100.0, 960.0, 26, ui::FAINT, ui::ALIGN_LEFT, false);
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_pages_keep_the_grid_and_cover_what_the_fields_need() {
        assert_eq!(SYMBOLS.chars().count(), LETTERS.chars().count());
        assert_eq!(keys(true, false).len(), keys(false, false).len());
        let all: String = keys(false, false).into_iter().chain(keys(true, false)).filter_map(|k| if let K::Ch(c) = k { Some(c) } else { None }).collect();
        // Client IDs, URLs and Reddit's user agent format.
        for c in "az09-_.:/?=&%()@+~#".chars() {
            assert!(all.contains(c), "{c} missing");
        }
        assert_eq!(label(K::Ch('q'), true, false), "Q");
        assert_eq!(label(K::Ch('-'), true, false), "-");
        assert_eq!(keys(false, true).len(), keys(false, false).len() + 1);
    }
}
