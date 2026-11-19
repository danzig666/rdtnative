//! The on-screen keyboard for one Reddit API field. Client IDs mix capitals, small letters,
//! digits, `-` and `_`; redirect URIs and user agents need `:/.?=&()` and spaces, so it has a
//! caps lock (not ytnative's one-shot Shift) and a symbols page. Done keeps the text and goes
//! back; BACK goes back without changing the field. The field is checked as you type with the
//! same rules the phone form and Save use. Layout and pointer handling after ytnative's search
//! keyboard (keys tile the board so a Magic Remote press never lands in a gap).

use super::common::{self, State};
use crate::host::{RdFx, RdHost};
use crate::store::Field;
use crate::ui;
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{
    AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step,
};
use plx_ui::Rect;
use std::borrow::Cow;

const G_KEYS: GroupId = GroupId(1);
const KEY_BASE: u32 = 0x1000_0000;

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
    Done,
}

const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
/// As many symbols as letters, so both pages keep the same grid.
const SYMBOLS: &str = "!?=&%()@+,;~#$*'\"<>[]{}|^`";
const ALWAYS: &str = "-_.:/";

fn keys(symbols: bool) -> Vec<K> {
    let mut v: Vec<K> = if symbols { SYMBOLS } else { LETTERS }.chars().map(K::Ch).collect();
    v.extend("1234567890".chars().map(K::Ch));
    v.extend(ALWAYS.chars().map(K::Ch));
    v.extend([K::Caps, K::Symbols, K::Space, K::Back, K::Clear, K::Done]);
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
        K::Done => "Done".into(),
    }
}

pub struct KeyboardScreen {
    entry: EntryId,
    field: Field,
    text: String,
    caps: bool,
    symbols: bool,
    state: State,
}

impl KeyboardScreen {
    pub fn new(entry: EntryId, field: Field, current: &str) -> Self {
        KeyboardScreen { entry, field, text: current.to_string(), caps: false, symbols: false, state: State(current.to_string()) }
    }

    fn keys(&self) -> Vec<K> {
        keys(self.symbols)
    }

    fn key_rect(i: usize) -> Rect {
        Rect::new(KEYS_X + (i % COLS) as f32 * (KEY_W + KEY_GAP), KEYS_Y + (i / COLS) as f32 * (KEY_H + KEY_GAP), KEY_W, KEY_H)
    }

    fn kkey(&self, i: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: KEY_BASE + i as u32 }
    }

    fn max_len(&self) -> usize {
        self.field.spec(&Default::default()).max_len
    }

    /// The problem with the text as typed, by the same rules Save and the phone form use.
    fn problem(&self) -> Option<String> {
        let t = self.text.trim();
        if t.is_empty() {
            return None; // empty = the default (client ID: not set yet)
        }
        let e = match self.field {
            Field::ClientId => rd_core::auth::ClientId::parse(t).err(),
            Field::RedirectUri => rd_core::config::check_redirect_uri(t).err(),
            Field::UserAgent => rd_core::config::check_user_agent(t).err(),
        }?;
        let mut c = e.chars();
        Some(c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default())
    }

    fn press(&mut self, k: K, fx: &mut Effects<'_, RdHost>) {
        match k {
            K::Ch(c) if self.text.chars().count() < self.max_len() => {
                self.text.push(if self.caps { c.to_ascii_uppercase() } else { c });
            }
            K::Ch(_) => fx.push(Fx::App(RdFx::Toast("That is as long as this field gets".into()))),
            K::Caps => self.caps = !self.caps,
            K::Symbols => self.symbols = !self.symbols,
            K::Space => self.text.push(' '),
            K::Back => {
                self.text.pop();
            }
            K::Clear => self.text.clear(),
            K::Done => {
                fx.push(Fx::App(RdFx::SetField(self.field, self.text.trim().to_string())));
                fx.push(Fx::Nav(NavOp::Pop));
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
        match ev {
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
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
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
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G_KEYS)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        let n = self.keys().len();
        let i = (key.elem - KEY_BASE) as usize;
        let (r, c) = (i / COLS, i % COLS);
        let to = match dir {
            Dir::Left if c > 0 => Some(i - 1),
            Dir::Right if c + 1 < COLS && i + 1 < n => Some(i + 1),
            Dir::Up if r > 0 => Some(i - COLS),
            Dir::Down if i + COLS < n => Some(i + COLS),
            // Down from a column the short last row lacks: its last key (Done).
            Dir::Down if r + 1 < n.div_ceil(COLS) => Some(n - 1),
            _ => None,
        };
        to.map_or(Step::Edge, |j| Step::Move(self.kkey(j)))
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        let i = key.checked_sub(KEY_BASE)? as usize;
        (i < self.keys().len()).then(|| {
            let r = Self::key_rect(i);
            Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(i as u32) }
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if want.elem < KEY_BASE || (want.elem - KEY_BASE) as usize >= self.keys().len() {
            return self.kkey(0);
        }
        want
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
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
        let spec = self.field.spec(&Default::default());
        ui::text(p, spec.label, 100.0, 60.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);

        // The field: the text with a cursor, or the default it falls back to when empty.
        let field = Rect::new(100.0, 140.0, 1720.0, 100.0);
        let problem = self.problem();
        ui::fill(p, field, 18.0, [1.0, 1.0, 1.0, 0.08]);
        if problem.is_some() {
            ui::fill(p, Rect::new(field.x, field.y + field.h - 4.0, field.w, 4.0), 2.0, [1.0, 0.45, 0.4, 1.0]);
        }
        let room = field.w - 72.0;
        if self.text.is_empty() {
            let hint = if spec.placeholder.is_empty() { String::new() } else { format!("{} {}", crate::i18n::tr("Empty uses:"), spec.placeholder) };
            ui::text_cut(p, f.measure, &hint, field.x + 36.0, field.y + 28.0, room, 36, ui::FAINT, ui::ALIGN_LEFT, false);
        } else {
            ui::text_cut_start(p, f.measure, &format!("{}|", self.text), field.x + 36.0, field.y + 28.0, room, 36, ui::WHITE, false);
        }
        let count = format!("{} / {}", self.text.chars().count(), spec.max_len);
        ui::text_raw(p, &count, field.x + field.w, field.y + field.h + 14.0, 22, ui::FAINT, ui::ALIGN_RIGHT, false);
        match &problem {
            Some(e) => ui::text_cut(p, f.measure, e, field.x, field.y + field.h + 14.0, field.w - 200.0, 26, [1.0, 0.55, 0.5, 1.0], ui::ALIGN_LEFT, true),
            None => ui::text_cut(p, f.measure, crate::i18n::tr(spec.help), field.x, field.y + field.h + 14.0, field.w - 200.0, 24, ui::DIM, ui::ALIGN_LEFT, false),
        };

        // Keyboard
        let focused = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for (i, k) in keys.into_iter().enumerate() {
            let r = Self::key_rect(i);
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
        let hint = "Done keeps the text. BACK leaves it unchanged. Or use \u{201C}Enter on phone\u{201D} to paste it.";
        ui::text(p, hint, 100.0, 900.0, 26, ui::FAINT, ui::ALIGN_LEFT, false);
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
        assert_eq!(keys(true).len(), keys(false).len());
        let all: String = keys(false).into_iter().chain(keys(true)).filter_map(|k| if let K::Ch(c) = k { Some(c) } else { None }).collect();
        // Client IDs, URLs and Reddit's user agent format.
        for c in "az09-_.:/?=&%()@+~#".chars() {
            assert!(all.contains(c), "{c} missing");
        }
        assert_eq!(label(K::Ch('q'), true, false), "Q");
        assert_eq!(label(K::Ch('-'), true, false), "-");
    }
}
