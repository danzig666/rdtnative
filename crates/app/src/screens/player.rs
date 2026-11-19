//! Full-screen video: the TV's video plane under a transparent page. A bar with the title,
//! progress and times shows on any key and hides after a few seconds. OK pauses, Left/Right seek
//! 10 seconds (Up/Down 60), BACK stops and returns. GIFs loop without sound.

use super::common::{self, State};
use crate::host::{RdFx, RdHost, RdMsg};
use crate::player::Phase;
use crate::ui;
use plx_machine::machine::{Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, Machine, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const HUD_MS: u32 = 4000;

pub struct PlayerScreen {
    entry: EntryId,
    url: String,
    title: String,
    looping: bool,
    shown_at: u32,
    state: State,
}

impl PlayerScreen {
    pub fn new(entry: EntryId, url: String, title: String, looping: bool) -> Self {
        PlayerScreen { entry, state: State(format!("player {url}")), url, title, looping, shown_at: 0 }
    }
}

impl Machine<RdHost> for PlayerScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.shown_at = cx.views.now_ms;
                fx.push(Fx::App(RdFx::Play { url: self.url.clone(), looping: self.looping }));
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                fx.push(Fx::App(RdFx::StopVideo));
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge, .. }, .. }) if *key != Key::Back => {
                if *edge == Edge::Up {
                    return Handled::Yes;
                }
                let hud_was_hidden = cx.views.now_ms.wrapping_sub(self.shown_at) > HUD_MS;
                self.shown_at = cx.views.now_ms;
                match key {
                    Key::Ok if *edge == Edge::Down => fx.push(Fx::App(RdFx::PauseVideo)),
                    Key::Left => fx.push(Fx::App(RdFx::SeekVideo(-10_000))),
                    Key::Right => fx.push(Fx::App(RdFx::SeekVideo(10_000))),
                    Key::Down if !hud_was_hidden => fx.push(Fx::App(RdFx::SeekVideo(-60_000))),
                    Key::Up if !hud_was_hidden => fx.push(Fx::App(RdFx::SeekVideo(60_000))),
                    _ => {}
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Pointer { .. } | InputKind::Click { .. }, .. }) => {
                self.shown_at = cx.views.now_ms;
                if matches!(ev, ScreenEvent::Input(InputEvent { kind: InputKind::Click { .. }, .. })) {
                    fx.push(Fx::App(RdFx::PauseVideo));
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Async(_, RdMsg::Changed) | ScreenEvent::Tick(_) => {
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
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
        let hud = paused || now.wrapping_sub(self.shown_at) < HUD_MS || !matches!(st.phase, Phase::Playing);
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
            p.rect(Rect::new(0.0, 0.0, 1920.0, 180.0), 0.0, [0.0, 0.0, 0.0, 0.7], [0.0, 0.0, 0.0, 0.0], 0.0);
            ui::text_cut(p, f.measure, &self.title, 80.0, 50.0, 1760.0, ui::sz(34), ui::WHITE, ui::ALIGN_LEFT, true);
            p.rect(Rect::new(0.0, 860.0, 1920.0, 220.0), 0.0, [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.8], 0.0);
            let bar = Rect::new(80.0, 980.0, 1760.0, 8.0);
            ui::fill(p, bar, 4.0, [1.0, 1.0, 1.0, 0.25]);
            if st.duration_ms > 0 {
                let k = (st.position_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0);
                ui::fill(p, Rect::new(bar.x, bar.y, bar.w * k, bar.h), 4.0, ui::accent());
                ui::fill(p, Rect::new(bar.x + bar.w * k - 10.0, bar.y - 6.0, 20.0, 20.0), 10.0, ui::WHITE);
            }
            let ts = ui::sz(26);
            let times = if st.duration_ms > 0 { format!("{} / {}", ui::fmt_time(st.position_ms), ui::fmt_time(st.duration_ms)) } else { ui::fmt_time(st.position_ms) };
            ui::text_raw(p, &times, 80.0, 920.0, ts, ui::WHITE, ui::ALIGN_LEFT, true);
            let icon = if paused { ui::icon::Kind::Play } else { ui::icon::Kind::Pause };
            ui::icon::draw(p, icon, Rect::new(940.0, 900.0, 44.0, 44.0), ui::WHITE, true);
            let mut hint = String::from("OK pause  \u{b7}  \u{2190}/\u{2192} 10 s  \u{b7}  \u{2191}/\u{2193} 1 min  \u{b7}  BACK closes");
            if self.looping {
                hint = format!("Loops  \u{b7}  {hint}");
            }
            if st.width > 0 {
                hint = format!("{}p  \u{b7}  {hint}", st.height);
            }
            ui::text_raw(p, &hint, 1840.0, 922.0, ui::sz(22), ui::DIM, ui::ALIGN_RIGHT, false);
        }
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
}
