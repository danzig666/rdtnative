//! The on-screen keyboard: searches, replies, subreddit names, filters, the Imgur client ID, a
//! sign-in code and the Reddit API fields. It has a caps lock and a symbols page (client IDs mix
//! cases; URLs and user agents need `:/.?=&()`). Done uses the text; BACK leaves without it, except
//! that a reply is kept as a draft for the next Reply (and Clear wants a second OK), so a long
//! reply typed with the remote is not lost to one key.
//! "Type on phone" opens a one-time page with the text so far (a QR code and a short address, as
//! sign-in does); what the phone sends is used as Done uses it. A USB keyboard types too (Enter is
//! Done), and the colour buttons delete, finish, add a space and open the phone page.
//! Searching shows the search history and favourite subreddits beside the keys. Layout and pointer
//! handling after ytnative's search keyboard (keys tile the board so a Magic Remote press never
//! lands in a gap).

use super::common::{self, Color, State};
use crate::host::{RdArg, RdFx, RdHost, Typing};
use crate::store::{Field, Store};
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, Machine, NavOp, ScreenEvent};
use plx_ui::consts::{SDLK_BACKSPACE, SDLK_CLEAR};
use plx_ui::frame::Budget;
use plx_ui::qr::QrCode;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::listing::Feed;
use std::borrow::Cow;

const G_KEYS: GroupId = GroupId(1);
const G_SUGGEST: GroupId = GroupId(2);
const G_PHONE: GroupId = GroupId(3);
const KEY_BASE: u32 = 0x1000_0000;
const SUG_BASE: u32 = 0x2000_0000;
/// The "Type on phone" button.
const PHONE_BASE: u32 = 0x3000_0000;

const COLS: usize = 12;
const KEY_W: f32 = 86.0;
const KEY_H: f32 = 80.0;
const KEY_GAP: f32 = 10.0;
const KEYS_X: f32 = 100.0;
const KEYS_Y: f32 = 380.0;

/// The column right of the keys: "Type on phone" on top, then the suggestions or, while the phone
/// page is open, its QR code.
const SIDE: Rect = Rect::new(1330.0, KEYS_Y, 500.0, 64.0);
const QR_FRAME: Rect = Rect::new(1440.0, 490.0, 280.0, 280.0);
/// A second OK on Clear within this long empties the text.
const CLEAR_CONFIRM_MS: u32 = 2000;
/// How much of the answered post or comment the phone page quotes.
const QUOTE_CHARS: usize = 2000;

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
        Typing::Reply { .. } => ("Reply".into(), "\u{21B5} starts a new line. Done sends it.".into(), "Write a reply\u{2026}".into(), rd_phone::MULTILINE_MAX_LEN),
        Typing::SignInCode => ("Sign-in code".into(), "The address of the page Reddit sent you to, or the code on it.".into(), "https://\u{2026}?code=\u{2026}".into(), 2048),
    }
}

/// What Done says to an empty text where one is not allowed.
fn empty_message(t: &Typing) -> Option<&'static str> {
    match t {
        Typing::Search { .. } => Some("Type something to search for"),
        Typing::GoToSub => Some("Type a subreddit name"),
        Typing::Reply { .. } => Some("The reply is empty"),
        _ => None,
    }
}

/// The problem with a typed text (API fields, subreddit names), by the rules Save and the phone use.
fn problem(target: &Typing, text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let e = match target {
        Typing::Api(Field::ClientId) => rd_core::auth::ClientId::parse(t).err(),
        Typing::Api(Field::RedirectUri) => rd_core::config::check_redirect_uri(t).err(),
        Typing::Api(Field::UserAgent) => rd_core::config::check_user_agent(t).err(),
        Typing::GoToSub | Typing::FilterSub => {
            let n = t.trim_start_matches("r/").trim_start_matches("/r/");
            (!n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| "Subreddit names have letters, digits and _ only".to_string())
        }
        _ => None,
    }?;
    let mut c = e.chars();
    Some(c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default())
}

/// Done's rules for a text sent from the phone page: the message to show under its field.
pub fn check(target: &Typing, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return empty_message(target).map_or(Ok(()), |m| Err(crate::i18n::tr(m).to_string()));
    }
    problem(target, text).map_or(Ok(()), Err)
}

/// Who a reply answers and what they wrote (markdown): a post's title and text, or a comment.
pub fn reply_context(store: &Store, parent: &str, post: &str) -> Option<(String, String)> {
    if let Some(id) = parent.strip_prefix("t3_") {
        let p = &store.post(id)?.post;
        return Some((p.author.clone(), format!("{}\n\n{}", p.title, p.selftext).trim().to_string()));
    }
    let c = store.thread(post).and_then(|t| super::menu::find(&t.tree, parent))?;
    Some((c.author.clone(), c.body.clone()))
}

/// The "Type on phone" page for a keyboard: one field holding the text typed so far.
pub fn phone_form(target: &Typing, text: &str, store: &Store) -> rd_phone::Form {
    let context = match target {
        Typing::Reply { parent, post } => reply_context(store, parent, post),
        _ => None,
    };
    build_form(target, text, context)
}

fn build_form(target: &Typing, text: &str, context: Option<(String, String)>) -> rd_phone::Form {
    let tr = |s: &str| crate::i18n::tr(s).to_string();
    let (title, _, hint, max) = spec(target);
    let reply = matches!(target, Typing::Reply { .. });
    let title = match &context {
        Some((author, _)) => tr("Reply to {}").replace("{}", &format!("u/{author}")),
        None => tr(&title),
    };
    let mut form = rd_phone::Form::new(&format!("rdtnative: {title}"));
    // A short address, so it can also be typed into a computer's browser.
    form.short = true;
    form.intro = vec![
        tr(if reply { "The TV sends your reply as soon as it gets it." } else { "What you send goes into the TV's keyboard and is used at once, as Done would." }),
        tr("Tip: the microphone on your phone's keyboard lets you dictate."),
    ];
    if let Some((_, body)) = &context {
        form.quote = body.chars().take(QUOTE_CHARS).collect();
    }
    let field = rd_phone::Field::new("text", &if reply { tr("Your reply") } else { title }).value(text).placeholder(&tr(&hint));
    let field = match target {
        Typing::Reply { .. } => field.kind(rd_phone::Kind::Multiline),
        Typing::Api(Field::RedirectUri) | Typing::SignInCode => field.kind(rd_phone::Kind::Url),
        _ => field,
    };
    form.fields.push(field.max_len(max));
    if reply {
        // Long enough to write a long reply on the phone.
        form.lifetime = std::time::Duration::from_secs(30 * 60);
        form.submit = tr("Send the reply");
        form.done = (tr("Sent to the TV."), tr("The TV sends the reply now and shows when Reddit has it. You can close this page."));
    } else {
        form.submit = tr("Send to TV");
        form.done = (tr("Sent to the TV."), tr("You can close this page. The link no longer works."));
    }
    form
}

pub struct KeyboardScreen {
    entry: EntryId,
    target: Typing,
    text: String,
    caps: bool,
    symbols: bool,
    suggest: VList,
    /// "Type on phone" (one row).
    phone: VList,
    qr: Option<(String, QrCode)>,
    /// Who a reply answers and the start of what they wrote, in plain text.
    context: Option<(String, String)>,
    /// When Clear was pressed once: a second press within [`CLEAR_CONFIRM_MS`] empties the text.
    clear_armed: Option<u32>,
    /// Keys come from a USB keyboard: Enter is Done. (A remote's OK is the same key, so arrows or
    /// the pointer give OK back to the on-screen keys.)
    typing: bool,
    /// The newest phone or voice text this keyboard has seen (`store.incoming`).
    incoming: u32,
    /// Done sent the reply: the rig keeps it as the draft until Reddit has it.
    sent: bool,
    state: State,
}

/// A suggestion beside the keyboard.
#[derive(Clone, Debug)]
enum Suggestion {
    Query(String),
    Sub(String),
    /// Search the text in all of Reddit instead of this subreddit.
    AllReddit,
}

impl KeyboardScreen {
    pub fn new(entry: EntryId, target: Typing, current: &str, cx: &Cx<'_, RdHost>) -> Self {
        let store = cx.views.store;
        let context = match &target {
            Typing::Reply { parent, post } => reply_context(store, parent, post).map(|(a, t)| (a, rd_core::markdown::plain(&t))),
            _ => None,
        };
        KeyboardScreen {
            entry,
            target,
            text: current.to_string(),
            caps: false,
            symbols: false,
            suggest: VList { entry, group: G_SUGGEST, base: SUG_BASE, rect: Rect::new(SIDE.x, KEYS_Y + 130.0, SIDE.w, 6.0 * 64.0), row_h: 64.0, text_size: 26 },
            phone: VList { entry, group: G_PHONE, base: PHONE_BASE, rect: SIDE, row_h: 64.0, text_size: 26 },
            qr: None,
            context,
            clear_armed: None,
            typing: false,
            incoming: store.incoming.seq,
            sent: false,
            state: State(current.to_string()),
        }
    }

    fn keys(&self) -> Vec<K> {
        keys(self.symbols, self.multi())
    }

    fn multi(&self) -> bool {
        matches!(self.target, Typing::Reply { .. })
    }

    fn key_rect(i: usize) -> Rect {
        Rect::new(KEYS_X + (i % COLS) as f32 * (KEY_W + KEY_GAP), KEYS_Y + (i / COLS) as f32 * (KEY_H + KEY_GAP), KEY_W, KEY_H)
    }

    fn kkey(&self, i: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: KEY_BASE + i as u32 }
    }

    /// The phone page open for this keyboard.
    fn phone_open<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a crate::store::TypePhoneView> {
        cx.views.store.type_phone.as_ref().filter(|v| v.target == self.target)
    }

    fn suggestions(&self, cx: &Cx<'_, RdHost>) -> Vec<Suggestion> {
        // The phone page's code takes their place.
        if self.phone_open(cx).is_some() {
            return Vec::new();
        }
        let p = &cx.views.store.prefs;
        let t = self.text.to_lowercase();
        let rows = self.suggest.rows_visible();
        match &self.target {
            Typing::Search { sub } => {
                let mut v: Vec<Suggestion> = Vec::new();
                if sub.is_some() {
                    v.push(Suggestion::AllReddit);
                }
                v.extend(p.history.iter().filter(|h| t.is_empty() || h.to_lowercase().contains(&t)).take(rows).cloned().map(Suggestion::Query));
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

    fn search(&mut self, q: String, sub: Option<String>, fx: &mut Effects<'_, RdHost>) {
        if q.is_empty() {
            fx.push(Fx::App(RdFx::Toast("Type something to search for".into())));
            return;
        }
        fx.push(Fx::App(RdFx::Typed(Typing::Search { sub: sub.clone() }, q.clone())));
        fx.push(Fx::Nav(NavOp::Replace(RdArg::Feed(Feed::Search { q, sub }))));
    }

    fn done(&mut self, text: String, fx: &mut Effects<'_, RdHost>) {
        let t = text.trim().to_string();
        if let Some(m) = empty_message(&self.target).filter(|_| t.is_empty()) {
            fx.push(Fx::App(RdFx::Toast(m.into())));
            return;
        }
        match &self.target {
            Typing::Api(f) => {
                fx.push(Fx::App(RdFx::SetField(*f, t)));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Typing::Search { sub } => {
                let sub = sub.clone();
                self.search(t, sub, fx);
            }
            Typing::GoToSub => {
                let name = t.trim_start_matches("/r/").trim_start_matches("r/").to_string();
                if name.is_empty() {
                    fx.push(Fx::App(RdFx::Toast("Type a subreddit name".into())));
                    return;
                }
                fx.push(Fx::Nav(NavOp::Replace(RdArg::Feed(Feed::Sub(name)))));
            }
            _ => {
                self.sent = matches!(self.target, Typing::Reply { .. });
                fx.push(Fx::App(RdFx::Typed(self.target.clone(), t)));
                fx.push(Fx::Nav(NavOp::Pop));
            }
        }
    }

    fn press(&mut self, k: K, now: u32, fx: &mut Effects<'_, RdHost>) {
        let max = spec(&self.target).3;
        if k != K::Clear {
            self.clear_armed = None;
        }
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
            K::Clear => match self.clear_armed.take() {
                Some(at) if now.wrapping_sub(at) < CLEAR_CONFIRM_MS => self.text.clear(),
                _ if !self.text.is_empty() => self.clear_armed = Some(now),
                _ => {}
            },
            K::Done => {
                if problem(&self.target, &self.text).is_some() {
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

    fn toggle_phone(&mut self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        fx.push(Fx::App(if self.phone_open(cx).is_some() {
            RdFx::TypePhoneStop
        } else {
            RdFx::TypePhoneStart { target: self.target.clone(), text: self.text.clone() }
        }));
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// Text from the phone (used as Done uses it) or from voice search (typed in: added to a
    /// reply, in place of a one-line text).
    fn take_incoming(&mut self, text: &str, done: bool, now: u32, fx: &mut Effects<'_, RdHost>) {
        let text = text.trim();
        if !done && self.multi() && !self.text.trim().is_empty() {
            if !self.text.ends_with(char::is_whitespace) {
                self.text.push(' ');
            }
            self.text.push_str(text);
        } else {
            self.text = text.to_string();
        }
        self.text = self.text.chars().take(spec(&self.target).3).collect();
        self.state = State(self.text.clone());
        fx.invalidate(plx_machine::present::Provenance::Lifecycle);
        if done {
            self.press(K::Done, now, fx);
        }
    }

    /// A USB keyboard (or the PC's in the simulator): characters, Backspace, Clear and Enter.
    fn hardware(&mut self, ev: &ScreenEvent<RdHost>, now: u32, fx: &mut Effects<'_, RdHost>) -> Option<Handled> {
        let ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, sym, wcode, edge, .. }, .. }) = ev else { return None };
        // plx reads a keyboard's q as BACK (for development keyboards); here it is a letter. The
        // remote's BACK is wcode 461 or 482, and Esc stays BACK.
        let letter_q = *key == Key::Back && *sym == 'q' as u32 && !matches!(*wcode, 461 | 482);
        if *edge == Edge::Up {
            return (letter_q || (*key == Key::Ok && self.typing)).then_some(Handled::Yes);
        }
        let typed = if letter_q {
            Some('q')
        } else if *key == Key::Other {
            char::from_u32(*sym).filter(|c| !c.is_control() && *sym < 0x4000_0000)
        } else {
            None
        };
        if let Some(c) = typed {
            self.typing = true;
            self.press(K::Ch(c), now, fx);
            return Some(Handled::Yes);
        }
        match (*key, *sym) {
            (Key::Other, SDLK_BACKSPACE) => {
                self.typing = true;
                self.press(K::Back, now, fx);
            }
            (Key::Other, SDLK_CLEAR) => self.press(K::Clear, now, fx),
            (Key::Other, crate::input::SHIFT_ENTER) => {
                self.typing = true;
                self.press(if self.multi() { K::Enter } else { K::Done }, now, fx);
            }
            (Key::Ok, _) if self.typing => {
                if *edge == Edge::Down {
                    self.press(K::Done, now, fx);
                }
            }
            (Key::Up | Key::Down | Key::Left | Key::Right, _) => {
                self.typing = false;
                return None;
            }
            _ => return None,
        }
        Some(Handled::Yes)
    }
}

impl Machine<RdHost> for KeyboardScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        let now = cx.views.now_ms;
        if let Some(c) = common::color_key(ev) {
            if cx.views.store.prefs.on("color_keys") {
                match c {
                    Color::Red => self.press(K::Back, now, fx),
                    Color::Green => self.press(K::Done, now, fx),
                    Color::Yellow => self.press(K::Space, now, fx),
                    Color::Blue => self.toggle_phone(cx, fx),
                }
            }
            return Handled::Yes;
        }
        if let Some(h) = self.hardware(ev, now, fx) {
            return h;
        }
        match ev {
            ScreenEvent::Tick(t) => {
                if self.clear_armed.is_some_and(|at| t.ms.wrapping_sub(at) >= CLEAR_CONFIRM_MS) {
                    self.clear_armed = None;
                    fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                }
                let inc = &cx.views.store.incoming;
                if inc.seq != self.incoming {
                    self.incoming = inc.seq;
                    self.take_incoming(&inc.text, inc.done, now, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                if self.phone_open(cx).is_some() {
                    fx.push(Fx::App(RdFx::TypePhoneStop));
                }
                // Left without sending: the text waits for the next Reply on the same comment.
                if let (Typing::Reply { parent, .. }, false) = (&self.target, self.sent) {
                    fx.push(Fx::App(RdFx::SetDraft { parent: parent.clone(), text: self.text.trim().to_string() }));
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) if self.phone.index(*e, 1).is_some() => {
                self.toggle_phone(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Activate(e) if *e >= SUG_BASE => {
                let sug = self.suggestions(cx);
                if let Some(s) = self.suggest.index(*e, sug.len()).map(|i| sug[i].clone()) {
                    match s {
                        Suggestion::Query(q) => self.done(q, fx),
                        Suggestion::Sub(name) => fx.push(Fx::Nav(NavOp::Replace(RdArg::Feed(Feed::Sub(name))))),
                        Suggestion::AllReddit => {
                            let q = self.text.trim().to_string();
                            self.search(q, None, fx);
                        }
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) if *e >= KEY_BASE => {
                if let Some(k) = self.keys().get((e - KEY_BASE) as usize).copied() {
                    self.press(k, now, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind, .. }) => {
                if matches!(kind, InputKind::Pointer { .. } | InputKind::Click { .. }) {
                    self.typing = false;
                }
                if common::back_down(ev) && self.multi() && !self.text.trim().is_empty() {
                    fx.push(Fx::App(RdFx::Toast("Draft kept".into())));
                }
                Handled::No
            }
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
        out.push(self.phone.spec(1));
        let s = self.suggestions(cx).len();
        if s > 0 {
            out.push(self.suggest.spec(s));
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if self.phone.index(*key, 1).is_some() {
            G_PHONE
        } else if *key >= SUG_BASE {
            G_SUGGEST
        } else {
            G_KEYS
        })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        let n = self.keys().len();
        let s = self.suggestions(cx).len();
        if self.phone.index(key.elem, 1).is_some() {
            return match dir {
                Dir::Left => Step::Move(self.kkey(COLS - 1)),
                Dir::Down if s > 0 => Step::Move(self.suggest.key(0)),
                _ => Step::Edge,
            };
        }
        if key.elem >= SUG_BASE {
            let i = self.suggest.index(key.elem, s).unwrap_or(0);
            return match dir {
                // The suggestions start a row below the phone button: row i sits by key row i + 1.
                Dir::Left => {
                    let row = (i + 1).min(n.div_ceil(COLS) - 1);
                    Step::Move(self.kkey((row * COLS + COLS - 1).min(n - 1)))
                }
                Dir::Up if i == 0 => Step::Move(self.phone.key(0)),
                _ => self.suggest.neighbour(key, dir, s),
            };
        }
        let i = (key.elem - KEY_BASE) as usize;
        let (r, c) = (i / COLS, i % COLS);
        let to = match dir {
            Dir::Left if c > 0 => Some(i - 1),
            Dir::Right if c + 1 < COLS && i + 1 < n => Some(i + 1),
            Dir::Right if c + 1 == COLS => {
                return Step::Move(if r == 0 || s == 0 { self.phone.key(0) } else { self.suggest.key((r - 1).min(s - 1)) });
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
        if self.phone.index(*key, 1).is_some() {
            return self.phone.place(*key, 1, self.phone.focused(cx, 1));
        }
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
        if self.phone.index(want.elem, 1).is_some() {
            return want;
        }
        if want.elem >= SUG_BASE {
            let n = self.suggestions(cx).len();
            if self.suggest.index(want.elem, n).is_some() {
                return want;
            }
            // The suggestions made way for the phone page's code: stay in that column.
            return self.phone.key(0);
        }
        if want.elem < KEY_BASE || (want.elem - KEY_BASE) as usize >= self.keys().len() {
            return self.kkey(0);
        }
        want
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        match g {
            G_SUGGEST => self.suggest.key(0),
            G_PHONE => self.phone.key(0),
            _ => self.kkey(0),
        }
    }
}

/// The colour buttons' line: a dot in each button's colour and what it does here.
fn color_hints(p: Painter, x: f32, y: f32) {
    const HINTS: [([f32; 4], &str); 4] = [
        ([0.9, 0.25, 0.25, 1.0], "Delete"),
        ([0.3, 0.78, 0.4, 1.0], "Done"),
        ([0.95, 0.8, 0.25, 1.0], "Space"),
        ([0.3, 0.55, 0.95, 1.0], "Type on phone"),
    ];
    let mut cx = x;
    for (col, label) in HINTS {
        ui::fill(p, Rect::new(cx, y + 6.0, 20.0, 20.0), 10.0, col);
        cx += 32.0;
        cx += ui::text(p, label, cx, y, 24, ui::DIM, ui::ALIGN_LEFT, false) + 44.0;
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
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, RdHost>) {
        match self.phone_open(cx) {
            Some(v) if self.qr.as_ref().is_none_or(|q| q.0 != v.url) => {
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
        let keys = self.keys();
        for i in 0..keys.len() {
            let r = Self::key_rect(i);
            let half = KEY_GAP / 2.0;
            ui::stop(f, self.kkey(i), Rect::new(r.x - half, r.y - half, r.w + KEY_GAP, r.h + KEY_GAP), Rect::new(0.0, 0.0, 1920.0, 1080.0));
        }
        let (title, help, hint, max) = spec(&self.target);
        // A reply names whom it answers, with the start of what they wrote under it.
        let field_y = match &self.context {
            Some((author, text)) => {
                let t = crate::i18n::tr("Reply to {}").replace("{}", &format!("u/{author}"));
                ui::text_raw(p, &t, 100.0, 36.0, 40, ui::WHITE, ui::ALIGN_LEFT, true);
                ui::fill(p, Rect::new(100.0, 98.0, 5.0, 58.0), 2.5, ui::accent());
                for (i, l) in ui::wrap_lines(f.measure, text, 1690.0, 24, false, 2).iter().enumerate() {
                    ui::text_cut(p, f.measure, l, 122.0, 94.0 + i as f32 * 32.0, 1690.0, 24, ui::DIM, ui::ALIGN_LEFT, false);
                }
                176.0
            }
            None => {
                ui::text_raw(p, crate::i18n::tr(&title), 100.0, 60.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
                140.0
            }
        };

        // The text with a cursor (the last lines of a long reply), or the hint when empty.
        let multi = self.multi();
        let field = Rect::new(100.0, field_y, 1720.0, if !multi { 100.0 } else if self.context.is_some() { 150.0 } else { 160.0 });
        let problem = problem(&self.target, &self.text);
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
        if max < rd_phone::MULTILINE_MAX_LEN {
            ui::text_raw(p, &format!("{} / {}", self.text.chars().count(), max), field.x + field.w, below, 22, ui::FAINT, ui::ALIGN_RIGHT, false);
        }
        match &problem {
            Some(e) => ui::text_cut(p, f.measure, e, field.x, below, field.w - 200.0, 26, [1.0, 0.55, 0.5, 1.0], ui::ALIGN_LEFT, true),
            None => ui::text_cut(p, f.measure, crate::i18n::tr(&help), field.x, below, field.w - 200.0, 24, ui::DIM, ui::ALIGN_LEFT, false),
        };

        // Keyboard
        let focused = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for (i, k) in keys.into_iter().enumerate() {
            let r = Self::key_rect(i);
            let is_focus = focused == Some(KEY_BASE + i as u32);
            let lit = (k == K::Caps && self.caps) || (k == K::Symbols && self.symbols) || (k == K::Clear && self.clear_armed.is_some());
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
            // A first OK on Clear asks; the second one clears.
            let text = if k == K::Clear && self.clear_armed.is_some() { "Clear?".into() } else { label(k, self.caps, self.symbols) };
            let ty = r.y + (r.h - size as f32 * 1.25) / 2.0;
            if matches!(k, K::Ch(_)) {
                ui::text_raw(p, &text, r.x + r.w / 2.0, ty, size, ink, ui::ALIGN_CENTER, true);
            } else {
                ui::text(p, &text, r.x + r.w / 2.0, ty, size, ink, ui::ALIGN_CENTER, true);
            }
        }

        // Right of the keys: "Type on phone", then its QR code or the suggestions.
        let open = self.phone_open(cx);
        let row = if open.is_some() { Row::new("Close the phone link").active(true) } else { Row::new("Type on phone") };
        ui::fill(p, self.phone.row_rect(0, 0), 12.0, [1.0, 1.0, 1.0, 0.06]);
        self.phone.draw(p, f.measure, &[row.icon(ui::icon::Kind::Phone)], self.phone.focused(cx, 1));
        self.phone.stops(f, 1);
        if let Some(v) = open {
            ui::fill(p, Rect::new(QR_FRAME.x - 20.0, QR_FRAME.y - 20.0, QR_FRAME.w + 40.0, QR_FRAME.h + 40.0), 20.0, ui::WHITE);
            if let Some((_, qr)) = &self.qr {
                qr.draw(p, QR_FRAME);
            }
            let mid = SIDE.x + SIDE.w / 2.0;
            let mut y = QR_FRAME.y + QR_FRAME.h + 36.0;
            // Browsers take the address without "http://": shorter to type.
            let url = v.url.strip_prefix("http://").unwrap_or(&v.url);
            ui::text_cut(p, f.measure, url, mid, y, SIDE.w, 30, ui::accent(), ui::ALIGN_CENTER, true);
            y += 50.0;
            let status = if v.opened { "Opened on your phone\u{2026}" } else { "Same Wi-Fi as the TV. The phone keyboard's microphone can dictate." };
            for line in ui::wrap_lines(f.measure, crate::i18n::tr(status), SIDE.w, 22, false, 2) {
                ui::text_raw(p, &line, mid, y, 22, ui::FAINT, ui::ALIGN_CENTER, false);
                y += 30.0;
            }
        } else {
            let sug = self.suggestions(cx);
            if !sug.is_empty() {
                let heading = match self.target {
                    Typing::GoToSub => "Favourites",
                    _ if sug.iter().any(|s| matches!(s, Suggestion::Query(_))) => "Recent searches",
                    _ => "",
                };
                if !heading.is_empty() {
                    ui::text(p, heading, self.suggest.rect.x + 10.0, self.suggest.rect.y - 40.0, 24, ui::FAINT, ui::ALIGN_LEFT, true);
                }
                let rows: Vec<Row> = sug
                    .iter()
                    .map(|s| match s {
                        Suggestion::Query(q) => Row::new(q.clone()).icon(ui::icon::Kind::History).raw(),
                        Suggestion::Sub(n) => Row::new(format!("r/{n}")).icon(ui::icon::Kind::Subs).raw(),
                        Suggestion::AllReddit => Row::new("Search all of Reddit").icon(ui::icon::Kind::Search),
                    })
                    .collect();
                self.suggest.draw(p, f.measure, &rows, self.suggest.focused(cx, rows.len()));
                self.suggest.stops(f, rows.len());
            }
        }
        if cx.views.store.prefs.on("color_keys") {
            color_hints(p, 100.0, 780.0);
        }
        let hint = match self.target {
            Typing::Api(_) => "Done keeps the text. BACK leaves it unchanged. Or use \u{201C}Enter on phone\u{201D} to paste it.",
            Typing::Reply { .. } => "Done sends the reply; BACK keeps it as a draft. On a USB keyboard Enter sends and Shift+Enter starts a new line.",
            _ => "Done uses the text. BACK leaves without it. A USB keyboard works too: Enter is Done.",
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
        // A reply's keys fill the four rows exactly; the others leave one gap.
        assert_eq!(keys(false, true).len(), 4 * COLS);
    }

    #[test]
    fn the_phone_page_follows_dones_rules() {
        let reply = Typing::Reply { parent: "t1_x".into(), post: "p".into() };
        assert!(check(&reply, " \n ").is_err());
        assert!(check(&reply, "Thanks!\nReally.").is_ok());
        assert!(check(&Typing::Search { sub: None }, "").is_err());
        assert!(check(&Typing::GoToSub, "").is_err());
        assert!(check(&Typing::GoToSub, "r/pics").is_ok());
        assert!(check(&Typing::GoToSub, "two words").is_err());
        assert!(check(&Typing::FindSub, "").is_ok());
        // An empty API field means its default.
        assert!(check(&Typing::Api(Field::RedirectUri), "").is_ok());
        assert!(check(&Typing::Api(Field::ClientId), "x").is_err());
    }

    #[test]
    fn a_reply_page_quotes_the_parent_in_a_text_box() {
        let reply = Typing::Reply { parent: "t1_x".into(), post: "p".into() };
        let f = build_form(&reply, "my draft", Some(("alice".into(), "Line one\nline two".into())));
        assert!(f.short);
        assert!(f.title.contains("u/alice"));
        assert_eq!(f.quote, "Line one\nline two");
        assert_eq!(f.fields.len(), 1);
        let field = &f.fields[0];
        assert_eq!((field.key.as_str(), field.value.as_str(), field.kind, field.max_len), ("text", "my draft", rd_phone::Kind::Multiline, rd_phone::MULTILINE_MAX_LEN));
        assert!(f.lifetime.as_secs() > 600);
        let s = build_form(&Typing::Search { sub: Some("rust".into()) }, "", None);
        assert_eq!((s.fields[0].kind, s.fields[0].max_len), (rd_phone::Kind::Text, 200));
        assert!(s.quote.is_empty());
        assert_eq!(build_form(&Typing::SignInCode, "", None).fields[0].kind, rd_phone::Kind::Url);
    }
}
