//! Full-screen video: the TV's video plane under a transparent page. A bar with the title,
//! progress, times and a row of controls (speed, quality, loop, replay) shows on any key and hides
//! after a few seconds. OK pauses (and replays once the video ended), Left/Right seek 10 seconds,
//! the remote's rewind and fast-forward 30; its play, pause and stop keys do what they say. Down
//! moves to the controls, Up or BACK leaves them; BACK on the video stops it and returns. GIFs
//! loop without sound.

use super::common::{self, State};
use crate::host::{RdFx, RdHost, RdMsg};
use crate::player::{Control, Phase, Status, SPEEDS};
use crate::ui::{self, icon::Kind as Icon};
use plx_machine::machine::{Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Machine, Measure, NavOp, ScreenEvent};
use plx_ui::consts::{classify_input, Key as K};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const HUD_MS: u32 = 4000;
/// The controls close after this long without a key (and the bar then fades as usual).
const ROW_MS: u32 = 8000;
/// Left / Right.
const SEEK_MS: i64 = 10_000;
/// The remote's rewind and fast-forward keys jump further.
const SKIP_MS: i64 = 30_000;
/// The progress line.
const BAR: Rect = Rect::new(80.0, 980.0, 1760.0, 8.0);
/// Elements of an open list (drawn only: the remote's keys move in it).
const LIST_BASE: u32 = 0x100;

pub struct PlayerScreen {
    entry: EntryId,
    url: String,
    title: String,
    looping: bool,
    shown_at: u32,
    state: State,
    nav: BarNav,
    /// Keys the player has no use for, each logged once (to learn what the TV sends for them).
    unbound: Vec<(u32, u32)>,
    /// A page went over the video while it still loaded: it was stopped, and starts again when
    /// the page goes.
    stopped_covered: bool,
}

impl PlayerScreen {
    pub fn new(entry: EntryId, url: String, title: String, looping: bool) -> Self {
        PlayerScreen { entry, state: State(format!("player {url}")), url, title, looping, shown_at: 0, nav: BarNav::default(), unbound: Vec::new(), stopped_covered: false }
    }

    /// The controls close by themselves: at once when the video failed, else after [`ROW_MS`]
    /// without a key, the bar then fading from there as it does after any key.
    fn expire_row(&mut self, now: u32, st: &Status) {
        if self.nav.row.is_none() {
            return;
        }
        if matches!(st.phase, Phase::Failed(_)) {
            self.nav = BarNav::default();
        } else if now.wrapping_sub(self.shown_at) > ROW_MS {
            self.nav = BarNav::default();
            self.shown_at = now.wrapping_sub(HUD_MS);
        }
    }

    /// How much of the bar shows: all of it while the video is not playing or the remote is in
    /// the controls, else fading out a while after the last key.
    fn hud_alpha(&self, now: u32, st: &Status) -> f32 {
        if self.nav.row.is_some() || st.phase != Phase::Playing {
            return 1.0;
        }
        1.0 - ui::ease_out((now.wrapping_sub(self.shown_at) as f32 - HUD_MS as f32) / 450.0)
    }

    /// What the pointer is over: the controls only count while the bar shows.
    fn hit(&self, x: f32, y: f32, cx: &Cx<'_, RdHost>) -> Hit {
        let st = &cx.views.store.play;
        if self.hud_alpha(cx.views.now_ms, st) <= 0.01 {
            return Hit::Video;
        }
        Layout::new(cx.measure, st, &self.nav).hit(x, y)
    }
}

impl Machine<RdHost> for PlayerScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        let now = cx.views.now_ms;
        let st = &cx.views.store.play;
        match ev {
            ScreenEvent::Mount => {
                self.shown_at = now;
                fx.push(Fx::App(RdFx::Play { url: self.url.clone(), looping: self.looping }));
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                fx.push(Fx::App(RdFx::StopVideo));
                Handled::Yes
            }
            // A page pushed over the video (a voice search, Sign in, a shared link): a playing
            // video pauses, one still loading stops (it would start, with its sound, under the
            // page). It plays on, or starts again, when the page goes.
            ScreenEvent::Cover => {
                match st.phase {
                    Phase::Playing => fx.push(Fx::App(RdFx::VideoControl(Control::Pause))),
                    Phase::Loading | Phase::Idle => {
                        fx.push(Fx::App(RdFx::StopVideo));
                        self.stopped_covered = true;
                    }
                    _ => {}
                }
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                // Stopped under the page, or another video took the player meanwhile.
                if std::mem::take(&mut self.stopped_covered) || st.phase == Phase::Idle {
                    fx.push(Fx::App(RdFx::Play { url: self.url.clone(), looping: self.looping }));
                }
                self.shown_at = now;
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, sym, wcode, edge, .. }, .. }) => {
                if *edge == Edge::Up {
                    return Handled::Yes;
                }
                let k = classify_input(*key, *sym, *wcode);
                let act = match self.nav.key(k, *edge, st) {
                    Did::Pass => return Handled::No,
                    Did::Done(act) => act,
                };
                if let Some(a) = act {
                    fx.push(a.fx());
                }
                if k == K::Other && *edge == Edge::Down && common::color_key(ev).is_none() && !self.unbound.contains(&(*sym, *wcode)) {
                    self.unbound.push((*sym, *wcode));
                    crate::log(&format!("player: unbound key sym {sym} wcode {wcode}"));
                }
                self.shown_at = now;
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Pointer { x, y, .. }, .. }) => {
                let at = self.hit(*x, *y, cx);
                self.nav.hover(at);
                self.shown_at = now;
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Click { x, y, .. }, .. }) => {
                let at = self.hit(*x, *y, cx);
                if let Some(a) = self.nav.click(at, st) {
                    fx.push(a.fx());
                }
                self.shown_at = now;
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.expire_row(t.ms, st);
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Async(_, RdMsg::Changed) => {
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

/// The controls in the bar, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctl {
    Speed,
    Quality,
    Loop,
    Replay,
}

/// The controls this video has: Quality only when the stream offers heights (HLS).
fn controls(st: &Status) -> Vec<Ctl> {
    let mut v = vec![Ctl::Speed];
    if !st.heights.is_empty() {
        v.push(Ctl::Quality);
    }
    v.extend([Ctl::Loop, Ctl::Replay]);
    v
}

/// A list control's values top to bottom (Up raises the speed or the quality), and the index of
/// the current one.
fn choices(c: Ctl, st: &Status) -> (Vec<(String, Control)>, usize) {
    match c {
        Ctl::Speed => {
            let at = |x: f32| SPEEDS.iter().rev().position(|r| (r - x).abs() < 0.01);
            (SPEEDS.iter().rev().map(|r| (speed_label(*r), Control::Rate(*r))).collect(), at(st.rate.0).or(at(1.0)).unwrap_or(0))
        }
        Ctl::Quality => {
            let mut v = vec![("Auto".to_string(), Control::Quality(0))];
            v.extend(st.heights.iter().map(|h| (format!("{h}p"), Control::Quality(*h))));
            // A cap is the tallest height within it; with none within it the whole stream plays.
            let cur = if st.max_height == 0 { 0 } else { st.heights.iter().position(|h| *h <= st.max_height).map_or(0, |i| i + 1) };
            (v, cur)
        }
        Ctl::Loop | Ctl::Replay => (Vec::new(), 0),
    }
}

/// A speed as the player shows it ("1×", "1.25×"), with a decimal comma in the languages that
/// write one.
fn speed_label(rate: f32) -> String {
    speed_text(rate, crate::i18n::current() != "en")
}

fn speed_text(rate: f32, comma: bool) -> String {
    let n = format!("{}", (rate * 100.0).round() / 100.0);
    format!("{}\u{d7}", if comma { n.replace('.', ",") } else { n })
}

/// A control's label: its current value.
fn chip_label(c: Ctl, st: &Status) -> String {
    let tr = crate::i18n::tr;
    match c {
        Ctl::Speed => speed_label(st.rate.0),
        Ctl::Quality => {
            let (values, cur) = choices(c, st);
            if cur == 0 && st.height > 0 {
                format!("{} \u{b7} {}p", tr("Auto"), st.height)
            } else {
                tr(&values[cur].0).to_string()
            }
        }
        Ctl::Loop => tr(if st.looping { "Loop on" } else { "Loop off" }).to_string(),
        Ctl::Replay => tr("Replay").to_string(),
    }
}

/// A control's icon, and whether it is on (Loop: drawn outlined; the others say their value).
fn chip_look(c: Ctl, st: &Status) -> (Icon, bool) {
    match c {
        Ctl::Speed => (Icon::Gauge, false),
        Ctl::Quality => (Icon::Film, false),
        Ctl::Loop => (Icon::Repeat, st.looping),
        Ctl::Replay => (Icon::Prev, false),
    }
}

/// What a key asks of the player.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Toggle,
    Seek(i64),
    Leave,
    Do(Control),
}

impl Act {
    fn fx(self) -> Fx<RdHost> {
        match self {
            Act::Toggle => Fx::App(RdFx::PauseVideo),
            Act::Seek(ms) => Fx::App(RdFx::SeekVideo(ms)),
            Act::Leave => Fx::Nav(NavOp::Pop),
            Act::Do(c) => Fx::App(RdFx::VideoControl(c)),
        }
    }
}

/// OK on the video: pause or go on, or start again once it ended.
fn ok_act(st: &Status) -> Act {
    if st.phase == Phase::Ended { Act::Do(Control::Replay) } else { Act::Toggle }
}

/// What a key did: passed on (BACK on the video, which closes the page) or taken, maybe asking
/// something of the player.
#[derive(Debug, PartialEq)]
enum Did {
    Pass,
    Done(Option<Act>),
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Video,
    Chip(Ctl),
    /// A value of the open list.
    Row(usize),
}

/// Where the remote is in the bar: on the video, on a control (`row`), or in that control's list
/// at a highlighted value (`pick`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct BarNav {
    row: Option<Ctl>,
    pick: Option<usize>,
}

impl BarNav {
    fn key(&mut self, k: K, edge: Edge, st: &Status) -> Did {
        let down = edge == Edge::Down;
        // The transport keys work wherever the remote is.
        let transport = match k {
            K::Play | K::Pause | K::PlayPause | K::Stop if !down => Some(None),
            K::Play => Some(Some(Act::Do(Control::Play))),
            K::Pause => Some(Some(Act::Do(Control::Pause))),
            K::PlayPause => Some(Some(ok_act(st))),
            K::Stop => Some(Some(Act::Leave)),
            K::Left { alt: true } => Some(Some(Act::Seek(-SKIP_MS))),
            K::Right { alt: true } => Some(Some(Act::Seek(SKIP_MS))),
            _ => None,
        };
        if let Some(act) = transport {
            return Did::Done(act);
        }
        if let (Some(c), Some(i)) = (self.row, self.pick) {
            let last = choices(c, st).0.len().saturating_sub(1);
            return Did::Done(match k {
                K::Up => {
                    self.pick = Some(i.saturating_sub(1));
                    None
                }
                K::Down => {
                    self.pick = Some((i + 1).min(last));
                    None
                }
                K::Ok if down => self.choose(st),
                K::Back | K::Left { .. } | K::Right { .. } if down => {
                    self.pick = None;
                    None
                }
                _ => None,
            });
        }
        if let Some(c) = self.row {
            let ctls = controls(st);
            let at = ctls.iter().position(|x| *x == c).unwrap_or(0);
            return Did::Done(match k {
                K::Left { .. } => {
                    self.row = Some(ctls[at.saturating_sub(1)]);
                    None
                }
                K::Right { .. } => {
                    self.row = Some(ctls[(at + 1).min(ctls.len() - 1)]);
                    None
                }
                K::Up | K::Back if down => {
                    *self = BarNav::default();
                    None
                }
                K::Ok if down => self.press(c, st),
                _ => None,
            });
        }
        match k {
            K::Back => Did::Pass,
            K::Ok if down => Did::Done(Some(ok_act(st))),
            K::Left { .. } => Did::Done(Some(Act::Seek(-SEEK_MS))),
            K::Right { .. } => Did::Done(Some(Act::Seek(SEEK_MS))),
            K::Down if down && !matches!(st.phase, Phase::Failed(_)) => {
                self.row = Some(Ctl::Speed);
                Did::Done(None)
            }
            _ => Did::Done(None),
        }
    }

    /// OK on a control: a list opens at the current value, Loop flips, Replay starts again.
    fn press(&mut self, c: Ctl, st: &Status) -> Option<Act> {
        match c {
            Ctl::Speed | Ctl::Quality => {
                self.pick = Some(choices(c, st).1);
                None
            }
            Ctl::Loop => Some(Act::Do(Control::Loop(!st.looping))),
            Ctl::Replay => Some(Act::Do(Control::Replay)),
        }
    }

    /// OK in a list: the highlighted value, unless it is the current one. The list closes.
    fn choose(&mut self, st: &Status) -> Option<Act> {
        let (c, i) = (self.row?, self.pick.take()?);
        let (values, cur) = choices(c, st);
        values.get(i).filter(|_| i != cur).map(|(_, v)| Act::Do(*v))
    }

    /// The pointer moved: a control or a list value under it takes the focus.
    fn hover(&mut self, at: Hit) {
        match at {
            Hit::Chip(c) if self.row != Some(c) => *self = BarNav { row: Some(c), pick: None },
            Hit::Row(i) => self.pick = Some(i),
            _ => {}
        }
    }

    /// A click: on a control or a list value as OK there; beside an open list it closes the
    /// list, elsewhere it is OK on the video.
    fn click(&mut self, at: Hit, st: &Status) -> Option<Act> {
        match at {
            Hit::Row(i) => {
                self.pick = Some(i);
                self.choose(st)
            }
            Hit::Chip(c) if self.row == Some(c) && self.pick.is_some() => {
                self.pick = None;
                None
            }
            Hit::Chip(c) => {
                *self = BarNav { row: Some(c), pick: None };
                self.press(c, st)
            }
            Hit::Video if self.pick.is_some() => {
                self.pick = None;
                None
            }
            Hit::Video => Some(ok_act(st)),
        }
    }
}

/// Where the bar's controls are on screen.
struct Layout {
    chips: Vec<(Ctl, Rect)>,
    /// The open list: the area of its values and their row height.
    list: Option<(Rect, f32)>,
}

impl Layout {
    fn new(m: &dyn Measure, st: &Status, nav: &BarNav) -> Layout {
        let h = ui::ls(52.0);
        // A video that failed has nothing to control.
        let ctls = if matches!(st.phase, Phase::Failed(_)) { Vec::new() } else { controls(st) };
        let labels: Vec<(Ctl, String)> = ctls.into_iter().map(|c| (c, chip_label(c, st))).collect();
        let names: Vec<(&str, bool)> = labels.iter().map(|(_, l)| (l.as_str(), true)).collect();
        let mut rects = common::button_row(m, &names, 0.0, BAR.y - 24.0 - h, h, 14.0, Some(BAR.x + BAR.w));
        // Beside the times and the play state when there is room, else on a line above them.
        if rects.first().is_some_and(|r| r.x < 1010.0) {
            for r in &mut rects {
                r.y -= h + 16.0;
            }
        }
        let chips: Vec<(Ctl, Rect)> = labels.iter().map(|(c, _)| *c).zip(rects).collect();
        let list = nav.row.filter(|_| nav.pick.is_some()).and_then(|c| {
            let (_, chip) = chips.iter().find(|(x, _)| *x == c)?;
            let (n, rh) = (choices(c, st).0.len() as f32, ui::ls(52.0));
            let w = ui::ls(260.0).max(chip.w);
            // The extra pixel keeps the last row inside the list's own rounding.
            Some((Rect::new(chip.x.min(1900.0 - w), chip.y - 20.0 - n * rh, w, n * rh + 1.0), rh))
        });
        Layout { chips, list }
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let inside = |r: &Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
        if let Some((r, rh)) = self.list.filter(|(r, _)| inside(r)) {
            return Hit::Row(((y - r.y) / rh) as usize);
        }
        self.chips.iter().find(|(_, r)| inside(r)).map_or(Hit::Video, |(c, _)| Hit::Chip(*c))
    }
}

impl Focusable<RdHost> for PlayerScreen {
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: G,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            len: 1,
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, _key: &u32, _cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        let r = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        Some(Placed { rect: r, rest_rect: r, clip: r, index: Some(0) })
    }
    fn reconcile(&self, _want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
}

impl Screen<RdHost> for PlayerScreen {
    fn name(&self) -> &'static str {
        "player"
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
        let st = &cx.views.store.play;
        let now = cx.views.now_ms;
        let paused = matches!(st.phase, Phase::Paused | Phase::Ended);
        let fade = self.hud_alpha(now, st).clamp(0.0, 1.0);
        let hud = fade > 0.01;
        if hud && fade < 1.0 {
            ui::animating();
        }
        let p = p.alpha(fade);
        match &st.phase {
            Phase::Loading | Phase::Idle => ui::spinner(p, now, 960.0, 540.0),
            Phase::Failed(e) => {
                ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 1.0]);
                common::message(p, f.measure, Rect::new(160.0, 260.0, 1600.0, 500.0), "Couldn't play this video", &format!("{e}\n\nSettings \u{203a} Content \u{203a} Video stream may help (try HLS or MP4)."));
            }
            _ => {}
        }
        if hud {
            ui::animating();
            let lay = Layout::new(f.measure, st, &self.nav);
            p.rect(Rect::new(0.0, 0.0, 1920.0, 180.0), 0.0, [0.0, 0.0, 0.0, 0.7], [0.0, 0.0, 0.0, 0.0], 0.0);
            ui::text_cut(p, f.measure, &self.title, 80.0, 50.0, 1760.0, ui::sz(34), ui::WHITE, ui::ALIGN_LEFT, true);
            let top = lay.chips.first().map_or(860.0, |(_, r)| (r.y - 60.0).min(860.0));
            p.rect(Rect::new(0.0, top, 1920.0, 1080.0 - top), 0.0, [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.8], 0.0);
            ui::fill(p, BAR, 4.0, [1.0, 1.0, 1.0, 0.25]);
            if st.duration_ms > 0 {
                let k = (st.position_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0);
                ui::fill(p, Rect::new(BAR.x, BAR.y, BAR.w * k, BAR.h), 4.0, ui::accent());
                ui::fill(p, Rect::new(BAR.x + BAR.w * k - 10.0, BAR.y - 6.0, 20.0, 20.0), 10.0, ui::WHITE);
            }
            let ts = ui::sz(26);
            let times = if st.duration_ms > 0 { format!("{} / {}", ui::fmt_time(st.position_ms), ui::fmt_time(st.duration_ms)) } else { ui::fmt_time(st.position_ms) };
            ui::text_raw(p, &times, 80.0, 920.0, ts, ui::WHITE, ui::ALIGN_LEFT, true);
            let icon = if paused { Icon::Play } else { Icon::Pause };
            ui::icon::draw(p, icon, Rect::new(940.0, 900.0, 44.0, 44.0), ui::WHITE, true);
            for (c, r) in &lay.chips {
                let (icon, set) = chip_look(*c, st);
                common::button(p, f.measure, *r, &chip_label(*c, st), Some(icon), self.nav.row == Some(*c), set, true);
            }
            if let (Some((r, rh)), Some(c)) = (lay.list, self.nav.row) {
                let (values, cur) = choices(c, st);
                ui::fill(p, Rect::new(r.x - 8.0, r.y - 8.0, r.w + 16.0, r.h + 16.0), 14.0, [0.13, 0.13, 0.16, 1.0]);
                let rows: Vec<ui::Row> = values.iter().enumerate().map(|(i, (v, _))| ui::Row::new(v.clone()).value(if i == cur { "\u{2713}" } else { "" }).active(i == cur)).collect();
                let list = ui::VList { entry: self.entry, group: G, base: LIST_BASE, rect: r, row_h: rh, text_size: ui::sz(26) };
                list.draw(p, f.measure, &rows, self.nav.pick);
            }
            let ok = match st.phase {
                Phase::Ended => "OK replay",
                Phase::Paused => "OK play",
                _ => "OK pause",
            };
            let more = if st.heights.is_empty() { "\u{2193} speed, loop" } else { "\u{2193} speed, quality, loop" };
            let keys = if matches!(st.phase, Phase::Failed(_)) {
                vec!["BACK closes"]
            } else if self.nav.pick.is_some() {
                vec!["\u{2191}/\u{2193} choose", "OK select", "BACK cancels"]
            } else if self.nav.row.is_some() {
                vec!["\u{2190}/\u{2192} choose", "OK select", "\u{2191} closes"]
            } else {
                vec![ok, "\u{2190}/\u{2192} 10 s", "\u{25c0}\u{25c0}/\u{25b6}\u{25b6} 30 s", more, "BACK closes"]
            };
            let hint = keys.iter().map(|k| crate::i18n::tr(k)).collect::<Vec<_>>().join("  \u{b7}  ");
            ui::text_raw(p, &hint, BAR.x + BAR.w, BAR.y + 30.0, ui::sz(22), ui::DIM, ui::ALIGN_RIGHT, false);
        }
        // Not faded with the bar: the player's own messages (a refused speed) come while it hides.
        common::toast(f.painter, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::Rate;

    const DOWN: Edge = Edge::Down;
    const LEFT: K = K::Left { alt: false };
    const RIGHT: K = K::Right { alt: false };

    fn playing() -> Status {
        Status { phase: Phase::Playing, heights: vec![720, 480], ..Default::default() }
    }

    fn act(a: Act) -> Did {
        Did::Done(Some(a))
    }

    #[test]
    fn controls_left_alone_close_and_the_bar_fades() {
        let st = playing();
        let mut s = PlayerScreen::new(EntryId(1), "u".into(), "t".into(), false);
        s.shown_at = 1_000;
        s.nav.row = Some(Ctl::Speed);
        s.expire_row(1_000 + ROW_MS, &st);
        assert!(s.nav.row.is_some());
        let now = 1_001 + ROW_MS;
        s.expire_row(now, &st);
        assert!(s.nav.row.is_none());
        // Not gone at once: the usual fade, from full.
        assert!(s.hud_alpha(now, &st) > 0.99);
        assert!(s.hud_alpha(now + 450, &st) < 0.01);
        // A failed video closes them at once.
        s.nav.row = Some(Ctl::Speed);
        s.expire_row(now, &Status { phase: Phase::Failed("x".into()), ..Default::default() });
        assert!(s.nav.row.is_none());
    }

    #[test]
    fn keys_on_the_video() {
        let st = playing();
        let mut n = BarNav::default();
        assert_eq!(n.key(K::Ok, DOWN, &st), act(Act::Toggle));
        assert_eq!(n.key(K::Ok, Edge::Repeat, &st), Did::Done(None));
        assert_eq!(n.key(LEFT, Edge::Repeat, &st), act(Act::Seek(-SEEK_MS)));
        assert_eq!(n.key(RIGHT, DOWN, &st), act(Act::Seek(SEEK_MS)));
        assert_eq!(n.key(K::Left { alt: true }, DOWN, &st), act(Act::Seek(-SKIP_MS)));
        assert_eq!(n.key(K::Right { alt: true }, Edge::Repeat, &st), act(Act::Seek(SKIP_MS)));
        assert_eq!(n.key(K::Play, DOWN, &st), act(Act::Do(Control::Play)));
        assert_eq!(n.key(K::Pause, DOWN, &st), act(Act::Do(Control::Pause)));
        assert_eq!(n.key(K::PlayPause, DOWN, &st), act(Act::Toggle));
        assert_eq!(n.key(K::PlayPause, Edge::Repeat, &st), Did::Done(None));
        assert_eq!(n.key(K::Stop, DOWN, &st), act(Act::Leave));
        assert_eq!(n.key(K::Other, DOWN, &st), Did::Done(None));
        assert_eq!(n.key(K::Up, DOWN, &st), Did::Done(None));
        assert_eq!(n.key(K::Back, DOWN, &st), Did::Pass);
        assert_eq!(n, BarNav::default());
        // Once the video ended, OK and play/pause start it again.
        let ended = Status { phase: Phase::Ended, ..playing() };
        assert_eq!(n.key(K::Ok, DOWN, &ended), act(Act::Do(Control::Replay)));
        assert_eq!(n.key(K::PlayPause, DOWN, &ended), act(Act::Do(Control::Replay)));
        // A video that failed has no controls to move to.
        let failed = Status { phase: Phase::Failed("refused".into()), ..playing() };
        assert_eq!(n.key(K::Down, DOWN, &failed), Did::Done(None));
        assert_eq!(n, BarNav::default());
    }

    #[test]
    fn controls_row_and_lists() {
        let st = playing();
        let mut n = BarNav::default();
        assert_eq!(n.key(K::Down, DOWN, &st), Did::Done(None));
        assert_eq!(n.row, Some(Ctl::Speed));
        // The speed list opens at 1x; Up is faster.
        assert_eq!(n.key(K::Ok, DOWN, &st), Did::Done(None));
        assert_eq!(n.pick, Some(3));
        n.key(K::Up, DOWN, &st);
        assert_eq!(n.key(K::Ok, DOWN, &st), act(Act::Do(Control::Rate(1.25))));
        assert_eq!(n, BarNav { row: Some(Ctl::Speed), pick: None });
        // The current value changes nothing.
        n.key(K::Ok, DOWN, &st);
        assert_eq!(n.key(K::Ok, DOWN, &st), Did::Done(None));
        // Quality: Auto, then the stream's heights; Down stops at the last.
        n.key(RIGHT, DOWN, &st);
        assert_eq!(n.row, Some(Ctl::Quality));
        n.key(K::Ok, DOWN, &st);
        assert_eq!(n.pick, Some(0));
        for _ in 0..4 {
            n.key(K::Down, Edge::Repeat, &st);
        }
        assert_eq!(n.key(K::Ok, DOWN, &st), act(Act::Do(Control::Quality(480))));
        // BACK or a side key closes a list without choosing.
        n.key(K::Ok, DOWN, &st);
        assert_eq!(n.key(K::Back, DOWN, &st), Did::Done(None));
        assert_eq!(n, BarNav { row: Some(Ctl::Quality), pick: None });
        n.key(RIGHT, DOWN, &st);
        assert_eq!(n.key(K::Ok, DOWN, &st), act(Act::Do(Control::Loop(true))));
        n.key(RIGHT, DOWN, &st);
        n.key(RIGHT, DOWN, &st);
        assert_eq!(n.row, Some(Ctl::Replay));
        assert_eq!(n.key(K::Ok, DOWN, &st), act(Act::Do(Control::Replay)));
        // The transport keys still work in the row; BACK leaves it, then the page.
        assert_eq!(n.key(K::Left { alt: true }, DOWN, &st), act(Act::Seek(-SKIP_MS)));
        assert_eq!(n.key(K::Back, DOWN, &st), Did::Done(None));
        assert_eq!(n, BarNav::default());
        assert_eq!(n.key(K::Back, DOWN, &st), Did::Pass);
        // Up leaves the row too.
        n.key(K::Down, DOWN, &st);
        n.key(K::Up, DOWN, &st);
        assert_eq!(n, BarNav::default());
    }

    #[test]
    fn values_offered_and_current() {
        // An MP4 has no quality choice.
        let mp4 = Status { phase: Phase::Playing, ..Default::default() };
        assert_eq!(controls(&mp4), [Ctl::Speed, Ctl::Loop, Ctl::Replay]);
        assert_eq!(controls(&playing()), [Ctl::Speed, Ctl::Quality, Ctl::Loop, Ctl::Replay]);
        let capped = Status { heights: vec![1080, 480, 360], max_height: 720, rate: Rate(0.5), ..playing() };
        let (values, cur) = choices(Ctl::Quality, &capped);
        assert_eq!(values.iter().map(|v| v.1).collect::<Vec<_>>(), [Control::Quality(0), Control::Quality(1080), Control::Quality(480), Control::Quality(360)]);
        // 480 is the tallest within a 720 cap; a cap below every height plays the whole stream.
        assert_eq!(cur, 2);
        assert_eq!(choices(Ctl::Quality, &Status { max_height: 240, ..capped.clone() }).1, 0);
        assert_eq!(choices(Ctl::Speed, &capped).1, 5);
        assert_eq!(choices(Ctl::Speed, &Status { rate: Rate(3.0), ..capped }).1, 3);
        assert_eq!(speed_text(1.0, false), "1\u{d7}");
        assert_eq!(speed_text(1.25, false), "1.25\u{d7}");
        assert_eq!(speed_text(0.75, true), "0,75\u{d7}");
    }

    #[test]
    fn pointer() {
        let st = playing();
        let mut n = BarNav::default();
        assert_eq!(n.click(Hit::Video, &st), Some(Act::Toggle));
        n.hover(Hit::Chip(Ctl::Loop));
        assert_eq!(n, BarNav { row: Some(Ctl::Loop), pick: None });
        assert_eq!(n.click(Hit::Chip(Ctl::Speed), &st), None);
        assert_eq!(n.pick, Some(3));
        n.hover(Hit::Row(0));
        assert_eq!(n.click(Hit::Row(0), &st), Some(Act::Do(Control::Rate(2.0))));
        // A click beside an open list, or on its control again, only closes it.
        n.click(Hit::Chip(Ctl::Speed), &st);
        assert_eq!(n.click(Hit::Video, &st), None);
        n.click(Hit::Chip(Ctl::Speed), &st);
        assert_eq!(n.click(Hit::Chip(Ctl::Speed), &st), None);
        assert_eq!(n.pick, None);
        assert_eq!(n.click(Hit::Chip(Ctl::Replay), &st), Some(Act::Do(Control::Replay)));
    }
}
