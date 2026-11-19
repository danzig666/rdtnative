//! Signing in to Reddit with the user's own app. While this screen is open the TV serves a sign-in
//! page on the home network: a short address to type into a computer's browser, or a QR code for
//! a phone. That page links to Reddit's approve page and takes back the address Reddit sent the
//! browser to (pasting sends it); with the app's redirect URI pointing at the page's server,
//! Reddit's redirect lands there and no copying is needed. The code can also be typed with the
//! remote. The TV trades it for tokens and closes the page.

use super::common::{self, State};
use crate::host::{RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::ui::{self, icon::Kind as Icon, Row, VList};
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::qr::QrCode;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::{Painter, Rect};
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x1000_0000;
/// The phone QR code of the sign-in page.
const QR: Rect = Rect::new(940.0, 330.0, 300.0, 300.0);

pub struct SignInScreen {
    list: VList,
    qr: Option<(String, QrCode)>,
    state: State,
}

impl SignInScreen {
    pub fn new(entry: EntryId) -> Self {
        SignInScreen {
            list: VList { entry, group: G, base: BASE, rect: Rect::new(1340.0, 330.0, 480.0, 3.0 * 80.0), row_h: 80.0, text_size: 28 },
            qr: None,
            state: State("signin".into()),
        }
    }

    fn rows() -> Vec<Row> {
        vec![Row::new("Type the code").icon(Icon::Key), Row::new("New address").icon(Icon::Repeat), Row::new("Cancel")]
    }
}

fn qr_for(slot: &mut Option<(String, QrCode)>, url: Option<&str>, frame: Rect) {
    match url {
        Some(u) if slot.as_ref().is_none_or(|q| q.0 != u) => {
            *slot = QrCode::new(u).ok().map(|mut q| {
                q.prepare(frame);
                (u.to_string(), q)
            });
        }
        None => *slot = None,
        _ => {}
    }
}

fn draw_qr(p: Painter, q: &Option<(String, QrCode)>, frame: Rect) {
    ui::fill(p, Rect::new(frame.x - 20.0, frame.y - 20.0, frame.w + 40.0, frame.h + 40.0), 20.0, if q.is_some() { ui::WHITE } else { ui::surface(0.1) });
    if let Some((_, qr)) = q {
        qr.draw(p, frame);
    }
}

impl Machine<RdHost> for SignInScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Mount => fx.push(Fx::App(RdFx::SignInStart)),
            ScreenEvent::Unmount => fx.push(Fx::App(RdFx::SignInStop)),
            ScreenEvent::Async(_, RdMsg::Changed) | ScreenEvent::Uncover => {
                // Signed in: done here.
                if cx.views.store.signed_in() && cx.views.store.signin.is_none() {
                    fx.push(Fx::Nav(NavOp::Pop));
                }
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            ScreenEvent::Activate(e) => {
                match self.list.index(*e, 3) {
                    Some(0) => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::SignInCode)))),
                    Some(1) => fx.push(Fx::App(RdFx::SignInStart)),
                    Some(2) => fx.push(Fx::Nav(NavOp::Pop)),
                    _ => {}
                }
                return Handled::Yes;
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<RdHost> for SignInScreen {
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(3));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        self.list.neighbour(key, dir, 3)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        self.list.place(*key, 3, self.list.focused(cx, 3))
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, 3).is_some() {
            want
        } else {
            self.list.key(0)
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<RdHost> for SignInScreen {
    fn name(&self) -> &'static str {
        "signin"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, RdHost>) {
        let v = cx.views.store.signin.as_ref();
        qr_for(&mut self.qr, v.and_then(|v| v.phone_url.as_deref()), QR);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let cx = f.cx;
        let st = cx.views.store;
        ui::text(p, "Sign in to Reddit", 160.0, 60.0, ui::sz(48), ui::WHITE, ui::ALIGN_LEFT, true);
        let hs = ui::sz(25);
        let intro = "On a computer or phone on the same Wi-Fi as the TV, open the sign-in page below. It walks you through approving rdtnative on Reddit.";
        for (i, l) in ui::wrap_lines(f.measure, crate::i18n::tr(intro), 1600.0, hs, false, 2).iter().enumerate() {
            ui::text_raw(p, l, 160.0, 150.0 + i as f32 * hs as f32 * 1.42, hs, ui::DIM, ui::ALIGN_LEFT, false);
        }
        let ls = ui::sz(26);
        let v = st.signin.as_ref();
        // The short address, big, to type into a computer's browser.
        ui::text(p, "On a computer, type this address:", 160.0, 260.0, ls, ui::WHITE, ui::ALIGN_LEFT, true);
        let addr_box = Rect::new(140.0, 310.0, 740.0, 110.0);
        ui::fill(p, addr_box, 18.0, ui::surface(0.08));
        match v.and_then(|v| v.phone_url.as_deref()) {
            Some(url) => {
                // Browsers take the address without "http://": shorter to type.
                let url = url.strip_prefix("http://").unwrap_or(url);
                let size = ui::sz(44);
                ui::text_cut(p, f.measure, url, addr_box.x + 30.0, addr_box.y + (addr_box.h - size as f32 * 1.2) / 2.0, addr_box.w - 60.0, size, ui::accent(), ui::ALIGN_LEFT, true);
            }
            None => {
                let msg = if v.is_some_and(|v| v.busy) { "Signing in\u{2026}" } else { "Not running: choose \u{201c}New address\u{201d}" };
                ui::text(p, msg, addr_box.x + 30.0, addr_box.y + 36.0, ls, ui::DIM, ui::ALIGN_LEFT, false);
            }
        }
        let steps = [
            "1.  Press \u{201c}Approve on Reddit\u{201d} on that page.",
            "2.  Copy the address Reddit sends the browser to.",
            "3.  Paste it on the page: the TV signs in.",
        ];
        for (i, l) in steps.iter().enumerate() {
            ui::text(p, l, 160.0, 460.0 + i as f32 * ls as f32 * 1.6, ls, ui::WHITE, ui::ALIGN_LEFT, false);
        }
        // Without copying: the app's redirect URI pointing at the page's server.
        if let Some(auto) = v.and_then(|v| v.auto_redirect.as_deref()) {
            let redirect = st.saved.as_ref().map(|s| s.redirect().to_string()).unwrap_or_default();
            let tip = if redirect == auto {
                "Your app's redirect URI points at this TV: approving signs in by itself.".to_string()
            } else {
                format!("{} {auto}", crate::i18n::tr("No copying: make your Reddit app's redirect URI (and Settings \u{203a} Reddit API) this address:"))
            };
            let ts = ui::sz(22);
            for (i, l) in ui::wrap_lines(f.measure, &tip, 720.0, ts, false, 3).iter().enumerate() {
                ui::text_raw(p, l, 160.0, 640.0 + i as f32 * ts as f32 * 1.4, ts, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
        ui::text(p, "Or scan with a phone:", QR.x - 20.0, QR.y - 70.0, ls, ui::WHITE, ui::ALIGN_LEFT, true);
        draw_qr(p, &self.qr, QR);
        let y = 800.0;
        match st.signin.as_ref() {
            Some(v) if v.busy => {
                ui::spinner(p, cx.views.now_ms, 200.0, y + 30.0);
                ui::text(p, "Signing in\u{2026}", 260.0, y + 10.0, ls, ui::WHITE, ui::ALIGN_LEFT, true);
            }
            Some(v) if v.error.is_some() => {
                for (i, l) in ui::wrap_lines(f.measure, v.error.as_deref().unwrap_or_default(), 1100.0, ls, true, 3).iter().enumerate() {
                    ui::text_raw(p, l, 160.0, y + i as f32 * ls as f32 * 1.4, ls, [1.0, 0.55, 0.5, 1.0], ui::ALIGN_LEFT, true);
                }
            }
            None if !st.configured() => {
                ui::text(p, "Enter your Reddit API settings first (Settings \u{203a} Reddit API).", 160.0, y, ls, [1.0, 0.55, 0.5, 1.0], ui::ALIGN_LEFT, true);
            }
            _ => {
                let redirect = st.saved.as_ref().map(|s| s.redirect().to_string()).unwrap_or_default();
                let note = format!("{} {redirect}", crate::i18n::tr("Your app's redirect URI:"));
                ui::text_cut(p, f.measure, &note, 160.0, y, 1600.0, ui::sz(22), ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
        let rows = Self::rows();
        let mut l = self.list;
        l.text_size = ui::sz(26);
        self.list = l;
        self.list.draw(p, f.measure, &rows, self.list.focused(cx, 3));
        self.list.stops(f, 3);
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
