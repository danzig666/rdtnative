//! Signing in to Reddit with the user's own app. While this screen is open the TV serves a sign-in
//! page on the home network: a short address to type into a computer's browser, or a QR code for
//! a phone. That page links to Reddit's approve page and takes back the address Reddit sent the
//! browser to (pasting sends it); with the app's redirect URI pointing at the page's server,
//! Reddit's redirect lands there and no copying is needed. The code can also be typed with the
//! remote. The TV trades it for tokens and closes the page. Signing in while signed in adds an
//! account. Without the Reddit API settings there is nothing to sign in with: the page then only
//! offers to set them up.

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
/// The most rows the page has.
const ROWS: usize = 4;
/// The phone QR code of the sign-in page.
const QR: Rect = Rect::new(940.0, 330.0, 300.0, 300.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    OnTv,
    Code,
    NewAddress,
    SetUpApi,
    Cancel,
}

/// Where Reddit sends its answer (the code) after Allow, which decides how signing in goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Answer {
    /// rdtnative's page or this TV's own address: the answer comes back to the TV by itself.
    Back,
    /// Another web address: the browser shows it in the address bar, to copy.
    Address,
    /// An app address (myapp://...) as the client ID's redirect URI: the browser
    /// cannot open it and stays on Reddit's page; only a computer's Firefox (address bar) or
    /// Chrome (console) shows it. One copy and paste that nothing can avoid.
    AppAddress,
}

fn answer(st: &crate::store::Store) -> Answer {
    let redirect = st.saved.as_ref().map(|s| s.redirect().to_string()).unwrap_or_default();
    let auto = st.signin.as_ref().and_then(|v| v.auto_redirect.as_deref());
    if redirect == rd_core::DEFAULT_REDIRECT_URI || auto == Some(redirect.as_str()) {
        Answer::Back
    } else if redirect.starts_with("http://") || redirect.starts_with("https://") {
        Answer::Address
    } else {
        Answer::AppAddress
    }
}

/// The rows: the ways to sign in, or (without API settings) the way to set them up. The TV's
/// browser only helps when the answer comes back by itself (nothing can be copied out of it).
fn acts(configured: bool, answer: Answer) -> &'static [Act] {
    match (configured, answer) {
        (false, _) => &[Act::SetUpApi, Act::Cancel],
        (true, Answer::Back) => &[Act::OnTv, Act::Code, Act::NewAddress, Act::Cancel],
        (true, _) => &[Act::Code, Act::NewAddress, Act::Cancel],
    }
}

/// The page's text for each kind of answer: the intro under the title, the numbered steps and a
/// hint under them.
fn texts(answer: Answer) -> (&'static str, &'static [&'static str], Option<&'static str>) {
    match answer {
        Answer::Back => (
            "\u{201c}Sign in on this TV\u{201d} opens Reddit in the TV's browser: log in with the remote and press Allow. Or, on a computer or phone on the same Wi-Fi, open the sign-in page: type the address below or scan the QR code.",
            &[
                "1.  On the phone or computer, press \u{201c}Approve on Reddit\u{201d}.",
                "2.  Log in to Reddit if asked, and press Allow.",
                "3.  The TV signs in by itself: nothing to copy.",
            ],
            Some("If a page shows a code instead, choose \u{201c}Type the code\u{201d} and enter it."),
        ),
        Answer::Address => (
            "Your redirect URI is a web address other than rdtnative's page, so you copy one address back to the TV. On a computer or phone on the same Wi-Fi, open the sign-in page: type the address below or scan the QR code.",
            &[
                "1.  Press \u{201c}Approve on Reddit\u{201d} on the sign-in page.",
                "2.  Log in to Reddit if asked, and press Allow.",
                "3.  Copy the address the browser ends up on (it has code= in it).",
                "4.  Back on the sign-in page, paste it: the TV signs in.",
            ],
            None,
        ),
        Answer::AppAddress => (
            "Your client ID sends Reddit's answer to an app address (like myapp://) that browsers can't open, so you copy it to the TV by hand, on a computer. This can't be avoided, but only once per account: the TV stays signed in.",
            &[
                "1.  Open the address above in Chrome, Edge or Firefox; press \u{201c}Approve on Reddit\u{201d}.",
                "2.  In the new Reddit tab (Chrome or Edge: press Ctrl+Shift+J first), log in if asked and press Allow.",
                "3.  The page seems stuck or shows an error: that's expected. Chrome/Edge: copy the whole red \u{201c}Failed to launch\u{2026}\u{201d} line. Firefox: copy all of the address bar.",
                "4.  Back in the first tab (the sign-in page), paste it: the TV signs in.",
            ],
            Some("Phones can't show this address: use a computer. On a Mac, Cmd+Option+J opens the Console. No red line? Press Allow again. After 10 minutes the TV shows a new address: start over from it."),
        ),
    }
}

pub struct SignInScreen {
    list: VList,
    qr: Option<(String, QrCode)>,
    /// `Store::signins` when the page opened: a sign-in since then closes it.
    signins: u32,
    state: State,
}

impl SignInScreen {
    pub fn new(entry: EntryId) -> Self {
        SignInScreen {
            list: VList { entry, group: G, base: BASE, rect: Rect::new(1340.0, 330.0, 480.0, ROWS as f32 * 80.0), row_h: 80.0, text_size: 28 },
            qr: None,
            signins: 0,
            state: State("signin".into()),
        }
    }

    fn acts(cx: &Cx<'_, RdHost>) -> &'static [Act] {
        acts(cx.views.store.configured(), answer(cx.views.store))
    }

    fn rows(cx: &Cx<'_, RdHost>) -> Vec<Row> {
        Self::acts(cx)
            .iter()
            .map(|a| match a {
                Act::OnTv => Row::new("Sign in on this TV").icon(Icon::User),
                Act::Code => Row::new("Type the code").icon(Icon::Key),
                Act::NewAddress => Row::new("New address").icon(Icon::Repeat),
                Act::SetUpApi => Row::new("Set up Reddit API").icon(Icon::Key),
                Act::Cancel => Row::new("Cancel"),
            })
            .collect()
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
        let st = cx.views.store;
        match ev {
            ScreenEvent::Mount => {
                self.signins = st.signins;
                if st.configured() {
                    fx.push(Fx::App(RdFx::SignInStart));
                }
            }
            ScreenEvent::Unmount => fx.push(Fx::App(RdFx::SignInStop)),
            ScreenEvent::Tick(_) | ScreenEvent::Async(_, RdMsg::Changed) | ScreenEvent::Uncover => {
                // Signed in (an account added, too): done here, once this page is on top again.
                let on_top = cx.focus.current.is_some_and(|k| k.entry == self.list.entry);
                if on_top && st.signins != self.signins && st.signin.is_none() {
                    self.signins = st.signins;
                    fx.push(Fx::Nav(NavOp::Pop));
                }
                if !matches!(ev, ScreenEvent::Tick(_)) {
                    fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                }
            }
            ScreenEvent::Activate(e) => {
                let acts = Self::acts(cx);
                match self.list.index(*e, acts.len()).map(|i| acts[i]) {
                    Some(Act::OnTv) => fx.push(Fx::App(RdFx::SignInOnTv)),
                    Some(Act::Code) => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::SignInCode)))),
                    Some(Act::NewAddress) => fx.push(Fx::App(RdFx::SignInStart)),
                    // In place of this page: once saved, the API page offers to sign in.
                    Some(Act::SetUpApi) => fx.push(Fx::Nav(NavOp::Replace(RdArg::ApiSettings))),
                    Some(Act::Cancel) => fx.push(Fx::Nav(NavOp::Pop)),
                    None => {}
                }
                return Handled::Yes;
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

// The focus group, the arrows and the pointer all count the rows on the page: a group shorter
// than the list leaves its last rows out of reach.
impl Focusable<RdHost> for SignInScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(Self::acts(cx).len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        self.list.neighbour(key, dir, Self::acts(cx).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        let n = Self::acts(cx).len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, Self::acts(cx).len()).is_some() {
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
        let rows = Self::rows(cx);
        let mut l = self.list;
        l.text_size = ui::sz(26);
        self.list = l;
        let focus = self.list.focused(cx, rows.len());
        self.list.draw(p, f.measure, &rows, focus);
        self.list.stops(f, rows.len());
        if !st.configured() {
            let ls = ui::sz(30);
            let why = "Signing in needs a Reddit API client ID, which you bring yourself: rdtnative comes without one. Enter it first in \u{201c}Set up Reddit API\u{201d}.";
            for (i, l) in ui::wrap_lines(f.measure, crate::i18n::tr(why), 1080.0, ls, false, 4).iter().enumerate() {
                ui::text_raw(p, l, 160.0, 330.0 + i as f32 * ls as f32 * 1.42, ls, ui::WHITE, ui::ALIGN_LEFT, false);
            }
            common::toast(p, f.measure, cx);
            return;
        }
        let kind = answer(st);
        let (intro, steps, tip) = texts(kind);
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
                let msg = if v.is_some_and(|v| v.busy) { "Signing in\u{2026}" } else { "No sign-in page now: choose \u{201c}New address\u{201d}" };
                ui::text(p, msg, addr_box.x + 30.0, addr_box.y + 36.0, ls, ui::DIM, ui::ALIGN_LEFT, false);
            }
        }
        // Without the phone's QR code (an app address cannot be seen on a phone) the steps may
        // use the width up to the rows.
        let width = if kind == Answer::AppAddress { 1140.0 } else { 740.0 };
        let mut sy = 460.0;
        for line in steps {
            for l in ui::wrap_lines(f.measure, crate::i18n::tr(line), width, ls, false, 2) {
                ui::text_raw(p, &l, 160.0, sy, ls, ui::WHITE, ui::ALIGN_LEFT, false);
                sy += ls as f32 * 1.35;
            }
            sy += ls as f32 * 0.25;
        }
        let ts = ui::sz(22);
        let tip: Option<String> = match (kind, v.and_then(|v| v.auto_redirect.as_deref())) {
            (Answer::Address, Some(auto)) => {
                Some(format!("{} {auto}", crate::i18n::tr("If you can change your client ID's redirect URI, set it (and Settings \u{203a} Reddit API) to this address and nothing needs copying:")))
            }
            _ => tip.map(|t| crate::i18n::tr(t).to_string()),
        };
        if let Some(tip) = tip {
            sy += ts as f32 * 0.4;
            for l in ui::wrap_lines(f.measure, &tip, width, ts, false, 3) {
                ui::text_raw(p, &l, 160.0, sy, ts, ui::FAINT, ui::ALIGN_LEFT, false);
                sy += ts as f32 * 1.4;
            }
        }
        if kind != Answer::AppAddress {
            ui::text(p, "Or scan with a phone:", QR.x - 20.0, QR.y - 70.0, ls, ui::WHITE, ui::ALIGN_LEFT, true);
            draw_qr(p, &self.qr, QR);
        }
        let y = 800.0;
        match st.signin.as_ref() {
            Some(v) if v.busy => {
                ui::spinner(p, cx.views.now_ms, 200.0, y + 30.0);
                ui::text(p, "Signing in\u{2026}", 260.0, y + 10.0, ls, ui::WHITE, ui::ALIGN_LEFT, true);
            }
            Some(v) if v.error.is_some() => {
                for (i, l) in ui::wrap_lines(f.measure, crate::i18n::tr(v.error.as_deref().unwrap_or_default()), 1100.0, ls, true, 3).iter().enumerate() {
                    ui::text_raw(p, l, 160.0, y + i as f32 * ls as f32 * 1.4, ls, [1.0, 0.55, 0.5, 1.0], ui::ALIGN_LEFT, true);
                }
            }
            _ => {
                let redirect = st.saved.as_ref().map(|s| s.redirect().to_string()).unwrap_or_default();
                let note = format!("{} {redirect}", crate::i18n::tr("Redirect URI (Settings \u{203a} Reddit API):"));
                ui::text_cut(p, f.measure, &note, 160.0, y, 1600.0, ui::sz(22), ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
        // Reddit approves whoever its site has signed in, not the account being added.
        if st.signed_in() {
            let ts = ui::sz(22);
            let tip = "Adding an account: Reddit approves whoever is signed in on reddit.com in that browser. Sign out there first, or type the address into a private window.";
            for (i, l) in ui::wrap_lines(f.measure, crate::i18n::tr(tip), 1600.0, ts, false, 2).iter().enumerate() {
                ui::text_raw(p, l, 160.0, y + 70.0 + i as f32 * ts as f32 * 1.4, ts, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
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
    fn without_api_settings_only_the_setup_is_offered() {
        assert_eq!(acts(true, Answer::Back).len(), ROWS);
        assert_eq!(acts(true, Answer::Back).last(), Some(&Act::Cancel));
        assert_eq!(acts(false, Answer::Back), [Act::SetUpApi, Act::Cancel]);
        // The TV's browser cannot hand back an answer that has to be copied.
        assert!(!acts(true, Answer::AppAddress).contains(&Act::OnTv));
        assert!(!acts(true, Answer::Address).contains(&Act::OnTv));
    }
}
