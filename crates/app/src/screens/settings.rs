//! Settings: the root page (option pages, filters, accounts, Reddit API, Imgur, history, about),
//! the option pages (each option opens a list of its values), the user's lists (favourites,
//! filtered subreddits and words, search history), the accounts on this TV and About. The right
//! panel explains the focused row; on the text size options it shows a live sample.

use super::common::{self, State};
use crate::host::{Confirm, MenuKind, RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::prefs;
use crate::ui::{self, icon::Kind as Icon, Row, VList};
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::{Painter, Rect};
use rd_core::listing::Feed;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x1000_0000;
const PANEL: Rect = Rect::new(1180.0, 170.0, 660.0, 840.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Root,
    Page(usize),
    Lists,
    About,
    Accounts,
}

#[derive(Clone, Debug)]
enum Act {
    Page(usize),
    Lists,
    Option(&'static str),
    SignIn,
    SignOut,
    Accounts,
    /// Use this account ("" browses without one).
    Switch(String),
    RemoveAccount(String),
    Api,
    Imgur,
    ClearRead,
    About,
    RemoveFavorite(String),
    OpenSub(String),
    RemoveFilterSub(String),
    RemoveFilterWord(String),
    AddFilterSub,
    AddFilterWord,
    ClearHistory,
    CheckUpdate,
    InstallUpdate,
    IdleSub,
    Nothing,
}

pub struct SettingsScreen {
    mode: Mode,
    list: VList,
    state: State,
}

pub type ListsScreen = SettingsScreen;
pub type AboutScreen = SettingsScreen;

impl SettingsScreen {
    fn make(entry: EntryId, mode: Mode) -> Self {
        SettingsScreen { mode, list: VList { entry, group: G, base: BASE, rect: Rect::new(80.0, 170.0, 1040.0, 860.0), row_h: 72.0, text_size: 28 }, state: State(format!("settings {mode:?}")) }
    }
    pub fn root(entry: EntryId) -> Self {
        Self::make(entry, Mode::Root)
    }
    pub fn page(entry: EntryId, i: usize) -> Self {
        Self::make(entry, Mode::Page(i))
    }
    #[allow(clippy::new_ret_no_self)]
    pub fn new(entry: EntryId) -> Self {
        // `ListsScreen::new` and `AboutScreen::new` share this type; host picks the mode.
        Self::make(entry, Mode::Lists)
    }
    pub fn about(entry: EntryId) -> Self {
        Self::make(entry, Mode::About)
    }
    pub fn accounts(entry: EntryId) -> Self {
        Self::make(entry, Mode::Accounts)
    }

    fn title(&self) -> String {
        match self.mode {
            Mode::Root => "Settings".into(),
            Mode::Page(i) => prefs::pages().get(i).map_or("Settings".into(), |p| p.title.to_string()),
            Mode::Lists => "Filters and favourites".into(),
            Mode::About => "About rdtnative".into(),
            Mode::Accounts => "Accounts".into(),
        }
    }

    fn rows(&self, cx: &Cx<'_, RdHost>) -> Vec<(Row, Act, &'static str)> {
        let st = cx.views.store;
        let p = &st.prefs;
        let mut v: Vec<(Row, Act, &'static str)> = Vec::new();
        match self.mode {
            Mode::Root => {
                for (i, page) in prefs::pages().iter().enumerate() {
                    let summary: Vec<&str> = page.keys.iter().take(3).map(|k| p.label(k)).collect();
                    v.push((Row::new(page.title).icon(page.icon).value(summary.join(", ")), Act::Page(i), ""));
                }
                v.push((
                    Row::new("Filters and favourites").icon(Icon::Folder).value(format!("{} favourites, {} filters", p.favorites.len(), p.filter_subs.len() + p.filter_words.len())),
                    Act::Lists,
                    "Favourite subreddits for the side menu; subreddits and words whose posts are hidden; search history.",
                ));
                // No writable folder that survives a restart (Developer Mode on old webOS, or a
                // package without its data folder): say so instead of signing out silently.
                let temporary = crate::DATA_TEMPORARY.load(std::sync::atomic::Ordering::Relaxed);
                if st.accounts.is_empty() {
                    if temporary {
                        v.push((Row::new("Sign in to Reddit").icon(Icon::User), Act::SignIn, "Sign in to vote, save, join subreddits, reply and see your home feed and inbox. Note: the TV gives rdtnative no folder that survives a restart, so the sign-in is lost when the TV restarts."));
                    } else {
                        v.push((Row::new("Sign in to Reddit").icon(Icon::User), Act::SignIn, "Sign in to vote, save, join subreddits, reply and see your home feed and inbox. The sign-in is kept across restarts and updates."));
                    }
                } else {
                    let tr = |s: &str| crate::i18n::tr(s).to_string();
                    let who = st.username().map_or_else(|| tr("Not signed in"), |u| format!("u/{u}"));
                    let row = |who: String| Row::new(tr("Accounts")).icon(Icon::User).value(who).raw();
                    if temporary {
                        v.push((row(format!("{who} {}", tr("(until restart)"))), Act::Accounts, "The TV gives rdtnative no folder that survives a restart: the sign-in and settings are lost when the TV restarts."));
                    } else {
                        v.push((row(who), Act::Accounts, "The Reddit accounts on this TV: switch between them, add one or sign out. They are kept across restarts and updates."));
                    }
                }
                v.push((
                    Row::new("Reddit API").icon(Icon::Key).value(st.saved.as_ref().map_or("Not set".into(), |s| s.client_id.clone())).raw(),
                    Act::Api,
                    "Your Reddit app's client ID, redirect URI and user agent.",
                ));
                v.push((
                    Row::new("Imgur client ID").icon(Icon::Key).value(if p.imgur_client_id.is_some() { p.imgur_id().to_string() } else { format!("{} (built in)", p.imgur_id()) }).raw(),
                    Act::Imgur,
                    "Imgur albums and galleries are loaded through Imgur's API with this client ID. Register your own at api.imgur.com/oauth2/addclient and enter it here.",
                ));
                v.push((Row::new("Clear read history").icon(Icon::History), Act::ClearRead, "Forget which posts you opened (they stop being dimmed or hidden)."));
                v.push((
                    Row::new("Check for updates").icon(Icon::Repeat).value(update_text()).raw(),
                    Act::CheckUpdate,
                    "Looks for a newer rdtnative on GitHub. The app also checks by itself about once a day and asks before installing. Updates are installed through Homebrew Channel.",
                ));
                if crate::update::found() {
                    v.push((
                        Row::new(format!("Install version {} (closes the app)", crate::update::found_version().unwrap_or_default())).icon(Icon::Check).raw(),
                        Act::InstallUpdate,
                        "Homebrew Channel downloads the new version, checks it and installs it. rdtnative closes while it installs; open it again afterwards.",
                    ));
                }
                v.push((Row::new("About and remote buttons").icon(Icon::Info), Act::About, "Version, credits and what each remote button does."));
            }
            Mode::Page(i) => {
                if let Some(page) = prefs::pages().get(i) {
                    for k in page.keys {
                        if let Some(o) = prefs::opt(k) {
                            v.push((Row::new(o.label).value(p.label(k)), Act::Option(o.key), o.help));
                        }
                        if *k == "idle_after" {
                            v.push((Row::new("Idle screen subreddit").value(format!("r/{}", p.idle_sub())), Act::IdleSub, "The subreddit whose pictures the idle screen shows (its hot posts). OK types another one."));
                        }
                    }
                }
            }
            Mode::Lists => {
                v.push((Row::new("Favourite subreddits").icon(Icon::Star).value(p.favorites.len().to_string()), Act::Nothing, "Add favourites with the star button on a subreddit's page or a post's menu."));
                for f in &p.favorites {
                    v.push((Row::new(format!("    r/{f}")).value("OK opens").raw(), Act::OpenSub(f.clone()), "OK opens it. Remove it with the star on its page."));
                    v.push((Row::new(format!("    Remove r/{f}")).raw(), Act::RemoveFavorite(f.clone()), "Removes it from your favourites."));
                }
                v.push((Row::new("Hidden subreddits").icon(Icon::Shield).value(p.filter_subs.len().to_string()), Act::Nothing, "Posts from these subreddits are left out of Home, Popular, All and search."));
                for f in &p.filter_subs {
                    v.push((Row::new(format!("    r/{f}")).value("OK removes").raw(), Act::RemoveFilterSub(f.clone()), "OK shows its posts again."));
                }
                v.push((Row::new("    Add a subreddit\u{2026}"), Act::AddFilterSub, "Type a subreddit name to hide its posts."));
                v.push((Row::new("Hidden words").icon(Icon::Shield).value(p.filter_words.len().to_string()), Act::Nothing, "Posts whose title has one of these words are left out."));
                for w in &p.filter_words {
                    v.push((Row::new(format!("    \u{201c}{w}\u{201d}")).value("OK removes").raw(), Act::RemoveFilterWord(w.clone()), "OK shows those posts again."));
                }
                v.push((Row::new("    Add a word\u{2026}"), Act::AddFilterWord, "Type a word or phrase to hide posts with it in their title."));
                v.push((Row::new("Clear search history").icon(Icon::History).value(p.history.len().to_string()), Act::ClearHistory, "Forget the searches suggested on the search page."));
            }
            Mode::About => {
                let lines: [(&str, &str); 12] = [
                    ("rdtnative", env!("CARGO_PKG_VERSION")),
                    ("OK", "Open \u{b7} hold for the menu"),
                    ("BACK", "Back \u{b7} opens the side menu on the start page"),
                    ("Arrows / wheel", "Move \u{b7} the wheel steps by item"),
                    ("Pointer at an edge", "Scrolls up or down"),
                    ("Green / Red", "Upvote / downvote"),
                    ("Yellow", "Save"),
                    ("Blue", "Comments \u{b7} on a post: post \u{2194} comments"),
                    ("In comments: Left / Right", "Parent comment / next thread"),
                    ("In pictures: OK", "Zoom in \u{b7} arrows move"),
                    ("Credits", "ytnative, plx-native (GPL-3.0), Inter, DejaVu, Roboto"),
                    ("Licence", "GNU GPL v3.0 \u{b7} not affiliated with Reddit or LG"),
                ];
                for (a, b) in lines {
                    v.push((Row::new(a).value(b).raw(), Act::Nothing, ""));
                }
            }
            Mode::Accounts => {
                let tr = |s: &str| crate::i18n::tr(s).to_string();
                let names = crate::accounts::names(st);
                for name in &names {
                    let row = Row::new(format!("u/{name}")).icon(Icon::User);
                    if crate::accounts::in_use(st, name) {
                        v.push((row.value(tr("In use")).active(true).raw(), Act::Nothing, "The account in use: votes, saves, the inbox and Home are this account's."));
                    } else {
                        v.push((row.value(tr("OK switches")).raw(), Act::Switch(name.clone()), "OK switches to this account; Home reloads with its subscriptions."));
                    }
                }
                v.push((
                    Row::new("Add account").icon(Icon::ListPlus),
                    Act::SignIn,
                    "Signs in with another Reddit account; the accounts here stay. Reddit approves whoever is signed in on reddit.com in that browser: sign out there first, or use a private window.",
                ));
                if st.signed_in() {
                    v.push((Row::new("Browse without an account").icon(Icon::Users), Act::Switch(String::new()), "Stops using the account until you switch back to it; it stays on this TV."));
                }
                for name in &names {
                    if crate::accounts::in_use(st, name) {
                        v.push((Row::new(tr("Sign out of u/{}").replace("{}", name)).icon(Icon::Leave).raw(), Act::SignOut, "Signs this TV out of the account and forgets it here. Signing in again needs a phone or a computer."));
                    } else {
                        v.push((Row::new(tr("Remove u/{}").replace("{}", name)).icon(Icon::Leave).raw(), Act::RemoveAccount(name.clone()), "Forgets the account on this TV and signs it out with Reddit."));
                    }
                }
                if let Some(o) = prefs::opt("inbox_alerts") {
                    v.push((Row::new(o.label).icon(Icon::Bell).value(p.label(o.key)), Act::Option(o.key), o.help));
                }
            }
        }
        v
    }

    fn run(&self, act: Act, fx: &mut Effects<'_, RdHost>) {
        match act {
            Act::Page(i) => fx.push(Fx::Nav(NavOp::Push(RdArg::SettingsPage(i)))),
            Act::Lists => fx.push(Fx::Nav(NavOp::Push(RdArg::Lists))),
            Act::Option(k) => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Option(k))))),
            Act::SignIn => fx.push(Fx::Nav(NavOp::Push(RdArg::SignIn))),
            Act::SignOut => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Confirm(Confirm::SignOut))))),
            Act::Accounts => fx.push(Fx::Nav(NavOp::Push(RdArg::Accounts))),
            Act::Switch(name) => {
                // Another account's Home, from the start.
                fx.push(Fx::App(RdFx::SwitchAccount(name)));
                fx.push(Fx::Nav(NavOp::Root(RdArg::Home)));
            }
            Act::RemoveAccount(name) => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Confirm(Confirm::Remove(name)))))),
            Act::Api => fx.push(Fx::Nav(NavOp::Push(RdArg::ApiSettings))),
            Act::Imgur => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::ImgurId)))),
            Act::ClearRead => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Confirm(Confirm::ClearRead))))),
            Act::About => fx.push(Fx::Nav(NavOp::Push(RdArg::About))),
            Act::RemoveFavorite(s) => fx.push(Fx::App(RdFx::EditPrefs(
                |p, s| {
                    p.favorites.retain(|f| !f.eq_ignore_ascii_case(s));
                },
                s,
            ))),
            Act::OpenSub(s) => fx.push(Fx::Nav(NavOp::Push(RdArg::Feed(Feed::Sub(s))))),
            Act::RemoveFilterSub(s) => fx.push(Fx::App(RdFx::EditPrefs(|p, s| p.filter_subs.retain(|f| !f.eq_ignore_ascii_case(s)), s))),
            Act::RemoveFilterWord(s) => fx.push(Fx::App(RdFx::EditPrefs(|p, s| p.filter_words.retain(|f| f != s), s))),
            Act::AddFilterSub => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::FilterSub)))),
            Act::AddFilterWord => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::FilterWord)))),
            Act::ClearHistory => fx.push(Fx::App(RdFx::EditPrefs(|p, _| p.history.clear(), String::new()))),
            Act::CheckUpdate => crate::update::check_now(),
            Act::InstallUpdate => crate::update::install(),
            Act::IdleSub => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::IdleSub)))),
            Act::Nothing => {}
        }
    }

    fn sized(&self) -> VList {
        let mut l = self.list;
        l.text_size = ui::sz(27);
        l.row_h = ui::ls(70.0).max(56.0);
        l.rect = Rect::new(80.0, ui::ls(150.0).max(140.0), 1040.0, 1040.0 - ui::ls(150.0).max(140.0));
        l
    }
}

impl Machine<RdHost> for SettingsScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Activate(e) => {
                let rows = self.rows(cx);
                if let Some(i) = self.list.index(*e, rows.len()) {
                    self.run(rows[i].1.clone(), fx);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Async(_, RdMsg::Changed) | ScreenEvent::Uncover => {
                self.list = self.sized();
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<RdHost> for SettingsScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(self.rows(cx).len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        self.list.neighbour(key, dir, self.rows(cx).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        let n = self.rows(cx).len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let n = self.rows(cx).len();
        match self.list.index(want.elem, n) {
            Some(_) => want,
            None if want.elem >= BASE && n > 0 => self.list.key(n - 1),
            None => self.list.key(0),
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<RdHost> for SettingsScreen {
    fn name(&self) -> &'static str {
        "settings"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {
        self.list = self.sized();
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let cx = f.cx;
        ui::text_raw(p, &crate::i18n::tr(&self.title()), 80.0, 50.0, ui::sz(48), ui::WHITE, ui::ALIGN_LEFT, true);
        let rows = self.rows(cx);
        let shown: Vec<Row> = rows.iter().map(|(r, _, _)| r.clone()).collect();
        let focus = self.list.focused(cx, shown.len());
        self.list.draw(p, f.measure, &shown, focus);
        self.list.stops(f, shown.len());
        // The panel: help for the focused row.
        if self.mode != Mode::About {
            ui::fill(p, PANEL, 24.0, ui::surface(0.06));
            if let Some(i) = focus {
                let (row, act, help) = &rows[i];
                let (px, pw) = (PANEL.x + 36.0, PANEL.w - 72.0);
                let ts = ui::sz(32);
                let mut y = PANEL.y + 36.0;
                for l in ui::wrap_lines(f.measure, &if row.raw { row.label.trim().to_string() } else { crate::i18n::tr(&row.label).to_string() }, pw, ts, true, 2) {
                    ui::text_raw(p, &l, px, y, ts, ui::WHITE, ui::ALIGN_LEFT, true);
                    y += ts as f32 * 1.35;
                }
                y += 16.0;
                let hs = ui::sz(25);
                for l in ui::wrap_lines(f.measure, crate::i18n::tr(help), pw, hs, false, 8) {
                    ui::text_raw(p, &l, px, y, hs, ui::DIM, ui::ALIGN_LEFT, false);
                    y += hs as f32 * 1.42;
                }
                if let Act::Option(k) = act {
                    y += 20.0;
                    // The value follows the label at any text size.
                    let now_w = ui::text(p, "Now:", px, y, hs, ui::FAINT, ui::ALIGN_LEFT, false);
                    ui::text(p, cx.views.store.prefs.label(k), px + now_w + ui::ls(14.0), y, hs, ui::accent(), ui::ALIGN_LEFT, true);
                    y += hs as f32 * 2.0;
                    if matches!(*k, "text_scale" | "content_scale" | "font") {
                        sample(p, f.measure, Rect::new(px, y, pw, PANEL.y + PANEL.h - y - 30.0));
                    }
                    ui::text(p, "OK shows the choices.", px, PANEL.y + PANEL.h - 60.0, ui::sz(22), ui::FAINT, ui::ALIGN_LEFT, false);
                }
            }
        }
        common::toast(p, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

/// The update state, for the "Check for updates" row.
fn update_text() -> String {
    use crate::update::State;
    let tr = |s: &str| crate::i18n::tr(s).to_string();
    match crate::update::state() {
        State::Unknown => format!("{} {}", tr("Version"), crate::update::current_version()),
        State::Checking => tr("Checking\u{2026}"),
        State::UpToDate => format!("{} ({})", tr("Up to date"), crate::update::current_version()),
        State::Available(r) => format!("{} {}", tr("Version available:"), r.version),
        State::CheckFailed(_) => tr("Couldn't check"),
        State::Downloading(Some(p)) => format!("{} {p}%", tr("Downloading")),
        State::Downloading(None) => tr("Downloading\u{2026}"),
        State::Installing => tr("Installing\u{2026}"),
        State::Installed => tr("Installed: open rdtnative again"),
        State::InstallFailed(e) => format!("{} {}", tr("Update failed:"), tr(&e)),
    }
}

/// A sample of how text looks at the current sizes and font.
fn sample(p: Painter, m: &dyn plx_machine::machine::Measure, r: Rect) {
    ui::fill(p, r, 18.0, ui::surface(0.12));
    let x = r.x + 24.0;
    let w = r.w - 48.0;
    let mut y = r.y + 20.0;
    let ts = ui::sz(26);
    for l in ui::wrap_lines(m, "Today I learned that octopuses have three hearts", w, ts, true, 2) {
        ui::text_raw(p, &l, x, y, ts, ui::WHITE, ui::ALIGN_LEFT, true);
        y += ts as f32 * 1.42;
    }
    ui::text_raw(p, "r/todayilearned  \u{b7}  u/someone  \u{b7}  5h", x, y + 4.0, ui::sz(21), ui::DIM, ui::ALIGN_LEFT, false);
    y += ui::sz(21) as f32 * 1.42 + 18.0;
    let cs = ui::csz(25);
    for l in ui::wrap_lines(m, "And blue blood. Two of the hearts pump blood to the gills, the third to the rest of the body.", w, cs, false, 5) {
        if y + cs as f32 > r.y + r.h - 10.0 {
            break;
        }
        ui::text_raw(p, &l, x, y, cs, [0.92, 0.92, 0.94, 1.0], ui::ALIGN_LEFT, false);
        y += cs as f32 * 1.42;
    }
}
