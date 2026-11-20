//! Settings › Reddit API: the OAuth client ID, the redirect URI and the user agent. Each field
//! opens the on-screen keyboard; "Enter on phone" shows a QR code of a one-time page on the home
//! network where all three can be typed or pasted (as RustCouch's "Type on phone"). The right
//! panel explains the focused field, or shows the QR code while the phone link is open.

use super::common::{self, State};
use crate::host::{RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::store::Field;
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::qr::QrCode;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G_ROWS: GroupId = GroupId(1);
const BASE: u32 = 0x1000_0000;
const ROW_PHONE: usize = 3;
const ROW_SAVE: usize = 4;
const ROWS: usize = 5;

const PANEL: Rect = Rect::new(1160.0, 200.0, 660.0, 760.0);
const QR_FRAME: Rect = Rect::new(1290.0, 320.0, 400.0, 400.0);

pub struct ApiScreen {
    list: VList,
    qr: Option<(String, QrCode)>,
    state: State,
}

impl ApiScreen {
    pub fn new(entry: EntryId) -> Self {
        ApiScreen {
            list: VList { entry, group: G_ROWS, base: BASE, rect: Rect::new(100.0, 200.0, 1000.0, ROWS as f32 * 96.0), row_h: 96.0, text_size: 30 },
            qr: None,
            state: State("api".into()),
        }
    }

    fn rows(&self, cx: &Cx<'_, RdHost>) -> Vec<Row> {
        let store = cx.views.store;
        let mut rows: Vec<Row> = Field::ALL
            .iter()
            .map(|&f| {
                let spec = f.spec(&store.draft);
                let shown = if spec.value.is_empty() {
                    crate::i18n::tr(if f == Field::ClientId { "Not set" } else { "Default" }).to_string()
                } else {
                    spec.value.clone()
                };
                let mark = if store.error(f).is_some() { "\u{26A0} " } else { "" };
                Row::new(format!("{mark}{}", crate::i18n::tr(spec.label))).value(shown).raw()
            })
            .collect();
        rows.push(Row::new(if store.phone.is_some() { "Phone link is open (OK closes it)" } else { "Enter on phone (QR code)" }).icon(ui::icon::Kind::Phone).active(store.phone.is_some()));
        rows.push(Row::new("Save and connect").icon(ui::icon::Kind::Check));
        rows
    }
}

impl Machine<RdHost> for ApiScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Activate(e) => {
                match self.list.index(*e, ROWS) {
                    Some(i) if i < Field::ALL.len() => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::Api(Field::ALL[i]))))),
                    Some(ROW_PHONE) => fx.push(Fx::App(if cx.views.store.phone.is_some() { RdFx::PhoneStop } else { RdFx::PhoneStart })),
                    Some(ROW_SAVE) => fx.push(Fx::App(RdFx::SaveApi)),
                    _ => {}
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            // Leaving the page closes the phone link (it is for this page only).
            ScreenEvent::Unmount => {
                fx.push(Fx::App(RdFx::PhoneStop));
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

impl Focusable<RdHost> for ApiScreen {
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(ROWS));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G_ROWS)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        self.list.neighbour(key, dir, ROWS)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        self.list.place(*key, ROWS, self.list.focused(cx, ROWS))
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, ROWS).is_some() {
            want
        } else {
            self.list.key(0)
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<RdHost> for ApiScreen {
    fn name(&self) -> &'static str {
        "api"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, RdHost>) {
        match &cx.views.store.phone {
            Some(v) if self.qr.as_ref().map(|q| q.0 != v.url).unwrap_or(true) => {
                self.qr = QrCode::new(&v.url).ok().map(|mut q| {
                    q.prepare(QR_FRAME);
                    (v.url.clone(), q)
                });
            }
            None => self.qr = None,
            _ => {}
        }
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let cx = f.cx;
        let store = cx.views.store;
        ui::text(p, "Reddit API settings", 100.0, 80.0, ui::sz(52).min(64), ui::WHITE, ui::ALIGN_LEFT, true);

        let rows = self.rows(cx);
        self.list.text_size = ui::sz(30).min(48);
        let focus = self.list.focused(cx, ROWS);
        self.list.draw(p, f.measure, &rows, focus);
        self.list.stops(f, ROWS);

        // Under the list: the setup notes (as many lines as the screen holds at large text).
        let mut y = self.list.rect.y + self.list.rect.h + 30.0;
        for step in rd_core::config::SETUP_STEPS {
            for line in ui::wrap_lines(f.measure, crate::i18n::tr(step), 1000.0, ui::sz(26), false, 2) {
                if y + ui::ls(38.0) <= 1070.0 {
                    ui::text_raw(p, &line, 100.0, y, ui::sz(26), ui::FAINT, ui::ALIGN_LEFT, false);
                }
                y += ui::ls(38.0);
            }
        }

        ui::fill(p, PANEL, 24.0, [1.0, 1.0, 1.0, 0.06]);
        let (px, pw) = (PANEL.x + 40.0, PANEL.w - 80.0);
        if let Some(phone) = &store.phone {
            ui::text(p, if phone.opened { "Opened on your phone\u{2026}" } else { "Scan with your phone" }, px, PANEL.y + 36.0, ui::sz(34), ui::WHITE, ui::ALIGN_LEFT, true);
            if let Some((_, qr)) = &self.qr {
                ui::fill(p, Rect::new(QR_FRAME.x - 20.0, QR_FRAME.y - 20.0, QR_FRAME.w + 40.0, QR_FRAME.h + 40.0), 20.0, ui::WHITE);
                qr.draw(p, QR_FRAME);
            }
            let mut y = QR_FRAME.y + QR_FRAME.h + 50.0;
            ui::text_cut(p, f.measure, &phone.url, px, y, pw, ui::sz(24), ui::DIM, ui::ALIGN_LEFT, false);
            y += ui::ls(44.0);
            for line in ui::wrap_lines(f.measure, crate::i18n::tr("Same Wi-Fi as the TV. The link works once and for 10 minutes; the fields fill in here when you send them."), pw, ui::sz(24), false, 4) {
                ui::text_raw(p, &line, px, y, ui::sz(24), ui::FAINT, ui::ALIGN_LEFT, false);
                y += ui::ls(34.0);
            }
        } else if let Some(i) = focus.filter(|i| *i < Field::ALL.len()) {
            let field = Field::ALL[i];
            let spec = field.spec(&store.draft);
            ui::text(p, spec.label, px, PANEL.y + 36.0, ui::sz(34), ui::WHITE, ui::ALIGN_LEFT, true);
            let mut y = PANEL.y + 36.0 + ui::ls(64.0);
            let (shown, ink) = if spec.value.is_empty() { (spec.placeholder.clone(), ui::FAINT) } else { (spec.value.clone(), ui::WHITE) };
            for line in ui::wrap_lines(f.measure, &shown, pw, ui::sz(28), false, 4) {
                ui::text_raw(p, &line, px, y, ui::sz(28), ink, ui::ALIGN_LEFT, false);
                y += ui::ls(40.0);
            }
            y += 20.0;
            if let Some(err) = store.error(field) {
                for line in ui::wrap_lines(f.measure, err, pw, ui::sz(26), true, 3) {
                    ui::text_raw(p, &line, px, y, ui::sz(26), [1.0, 0.55, 0.5, 1.0], ui::ALIGN_LEFT, true);
                    y += ui::ls(36.0);
                }
                y += 14.0;
            }
            for line in ui::wrap_lines(f.measure, crate::i18n::tr(spec.help), pw, ui::sz(24), false, 6) {
                ui::text_raw(p, &line, px, y, ui::sz(24), ui::DIM, ui::ALIGN_LEFT, false);
                y += ui::ls(34.0);
            }
            ui::text_cut(p, f.measure, crate::i18n::tr("OK edits it with the on-screen keyboard."), px, PANEL.y + PANEL.h - 70.0, pw, ui::sz(24), ui::FAINT, ui::ALIGN_LEFT, false);
        } else {
            let (title, body) = if focus == Some(ROW_PHONE) {
                ("Enter on phone", "Shows a QR code. Scan it with a phone on the same Wi-Fi: a page opens with all three settings, so you can paste the client ID there.")
            } else {
                ("Save and connect", "Checks the settings, saves them on this TV and loads the front page with your client ID.")
            };
            ui::text(p, title, px, PANEL.y + 36.0, ui::sz(34), ui::WHITE, ui::ALIGN_LEFT, true);
            let mut y = PANEL.y + 36.0 + ui::ls(64.0);
            for line in ui::wrap_lines(f.measure, crate::i18n::tr(body), pw, ui::sz(26), false, 8) {
                ui::text_raw(p, &line, px, y, ui::sz(26), ui::DIM, ui::ALIGN_LEFT, false);
                y += ui::ls(38.0);
            }
            let status = if store.configured() { "Saved settings are in use." } else { "Nothing is saved yet." };
            ui::text_cut(p, f.measure, crate::i18n::tr(status), px, PANEL.y + PANEL.h - 70.0, pw, ui::sz(24), ui::FAINT, ui::ALIGN_LEFT, false);
        }
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
