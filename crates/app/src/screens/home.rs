//! Home. Without API settings: a welcome that leads to Settings › Reddit API. With them: the
//! front page (Hot without an account) as a list, plus the actions row. Phase 1 shows the
//! titles only; cards, media and comments come in Phases 3-5.

use super::common::{self, State};
use crate::host::{RdArg, RdFx, RdHost, RdMsg};
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G_ACTIONS: GroupId = GroupId(1);
const G_POSTS: GroupId = GroupId(2);
const ACT_BASE: u32 = 0x1000_0000;
const POST_BASE: u32 = 0x2000_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Setup,
    Refresh,
}

pub struct HomeScreen {
    actions: VList,
    posts: VList,
    asked: bool,
    state: State,
}

impl HomeScreen {
    pub fn new(entry: EntryId) -> Self {
        HomeScreen {
            actions: VList { entry, group: G_ACTIONS, base: ACT_BASE, rect: Rect::new(100.0, 300.0, 520.0, 200.0), row_h: 84.0, text_size: 30 },
            posts: VList { entry, group: G_POSTS, base: POST_BASE, rect: Rect::new(700.0, 230.0, 1120.0, 760.0), row_h: 95.0, text_size: 30 },
            asked: false,
            state: State("home".into()),
        }
    }

    fn actions(&self, cx: &Cx<'_, RdHost>) -> Vec<Action> {
        if cx.views.store.configured() {
            vec![Action::Refresh, Action::Setup]
        } else {
            vec![Action::Setup]
        }
    }

    fn post_count(&self, cx: &Cx<'_, RdHost>) -> usize {
        cx.views.store.front.posts.len()
    }
}

impl Machine<RdHost> for HomeScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Mount | ScreenEvent::Tick(_) => {
                // First frame with settings (also after they were first saved): load the front page.
                if !self.asked && cx.views.store.configured() {
                    self.asked = true;
                    if cx.views.store.front.posts.is_empty() && !cx.views.store.front.loading {
                        fx.push(Fx::App(RdFx::LoadFront));
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) if (ACT_BASE..POST_BASE).contains(e) => {
                match self.actions(cx).get((e - ACT_BASE) as usize) {
                    Some(Action::Setup) => fx.push(Fx::Nav(NavOp::Push(RdArg::ApiSettings))),
                    Some(Action::Refresh) => fx.push(Fx::App(RdFx::LoadFront)),
                    None => {}
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) if *e >= POST_BASE => {
                fx.push(Fx::App(RdFx::Toast("Posts open in a later version".into())));
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

impl Focusable<RdHost> for HomeScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.actions.spec(self.actions(cx).len()));
        let n = self.post_count(cx);
        if n > 0 {
            out.push(self.posts.spec(n));
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if *key >= POST_BASE { G_POSTS } else { G_ACTIONS })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        let n = self.post_count(cx);
        if key.elem >= POST_BASE {
            if dir == Dir::Left {
                return Step::Move(self.actions.key(0));
            }
            return self.posts.neighbour(key, dir, n);
        }
        if dir == Dir::Right && n > 0 {
            let top = self.posts.top_for(self.posts.focused(cx, n), n);
            return Step::Move(self.posts.key(top));
        }
        self.actions.neighbour(key, dir, self.actions(cx).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        if *key >= POST_BASE {
            let n = self.post_count(cx);
            return self.posts.place(*key, n, self.posts.focused(cx, n));
        }
        let n = self.actions(cx).len();
        self.actions.place(*key, n, self.actions.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let ok = if want.elem >= POST_BASE {
            self.posts.index(want.elem, self.post_count(cx)).is_some()
        } else {
            self.actions.index(want.elem, self.actions(cx).len()).is_some()
        };
        if ok {
            want
        } else {
            self.actions.key(0)
        }
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if g == G_POSTS {
            self.posts.key(0)
        } else {
            self.actions.key(0)
        }
    }
}

impl Screen<RdHost> for HomeScreen {
    fn name(&self) -> &'static str {
        "home"
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
        let store = cx.views.store;
        common::logo(p, 100.0, 80.0);

        let actions: Vec<Row> = self
            .actions(cx)
            .iter()
            .map(|a| match a {
                Action::Setup => Row::new("Reddit API settings").icon(ui::icon::Kind::Gear),
                Action::Refresh => Row::new("Refresh").icon(ui::icon::Kind::Repeat),
            })
            .collect();
        self.actions.draw(p, f.measure, &actions, self.actions.focused(cx, actions.len()));
        self.actions.stops(f, actions.len());

        let area = self.posts.rect;
        if !store.configured() {
            ui::text(p, "Welcome to rdtnative", area.x, 300.0, 48, ui::WHITE, ui::ALIGN_LEFT, true);
            let lines = [
                "rdtnative talks to Reddit with your own Reddit app.",
                "Open Reddit API settings and enter its client ID:",
                "type it with the remote, or scan a QR code and paste it on your phone.",
            ];
            for (i, l) in lines.iter().enumerate() {
                ui::text(p, l, area.x, 390.0 + i as f32 * 50.0, 30, ui::DIM, ui::ALIGN_LEFT, false);
            }
            common::toast(p, f.measure, cx);
            return;
        }

        let front = &store.front;
        let heading = if front.title.is_empty() { "Front page" } else { &front.title };
        ui::text(p, heading, area.x, 150.0, 40, ui::WHITE, ui::ALIGN_LEFT, true);
        if front.posts.is_empty() {
            if front.loading {
                ui::spinner(p, cx.views.now_ms, area.x + area.w / 2.0, area.y + 200.0);
            } else if let Some(e) = &front.error {
                ui::text(p, "Couldn't load the front page", area.x, area.y + 60.0, 34, ui::WHITE, ui::ALIGN_LEFT, false);
                for (i, line) in ui::wrap_lines(f.measure, e, area.w, 26, false, 4).iter().enumerate() {
                    ui::text_raw(p, line, area.x, area.y + 120.0 + i as f32 * 38.0, 26, ui::DIM, ui::ALIGN_LEFT, false);
                }
                ui::text(p, "Check Reddit API settings, then choose Refresh.", area.x, area.y + 290.0, 26, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        } else {
            let rows: Vec<Row> = front
                .posts
                .iter()
                .map(|post| {
                    let nsfw = if post.over_18 { "  NSFW" } else { "" };
                    Row::new(post.title.clone())
                        .value(format!("r/{}  \u{2191}{}  {} comments{nsfw}", post.subreddit, common::short_count(post.score), common::short_count(post.comments)))
                        .raw()
                })
                .collect();
            self.posts.draw(p, f.measure, &rows, self.posts.focused(cx, rows.len()));
            self.posts.stops(f, rows.len());
            if front.loading {
                ui::spinner(p, cx.views.now_ms, area.x + area.w - 40.0, 170.0);
            }
        }
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
