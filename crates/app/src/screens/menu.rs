//! Pop-up menus over a page: actions on a post, a comment or a subreddit, the links in a post or
//! comment, one option's values, and the adult check. OK runs the row and closes the menu; BACK
//! closes it. The text size, font, theme and accent menus stay open beside the Settings sample
//! while values are tried.

use super::common::{self, State};
use crate::host::{MenuKind, RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::ui::{self, icon::Kind as Icon, Row, VList};
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use rd_core::comments::{Comment, Node};
use rd_core::listing::{Feed, UserTab};
use rd_core::media::Media;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x1000_0000;

#[derive(Clone, Debug)]
enum Act {
    OpenPost(String),
    Vote(String, i8),
    Save(String, bool),
    Hide(String),
    Viewer(String),
    Link(String),
    Push(RdArg),
    Favorite(String),
    FilterSub(String),
    Subscribe(String, bool),
    Toggle(String, String),
    Reply(String, String),
    SetPref(&'static str, String),
    Adult,
    InstallUpdate,
    Share(String),
    Close,
}

fn find<'a>(nodes: &'a [Node], name: &str) -> Option<&'a Comment> {
    for n in nodes {
        if let Node::Comment(c) = n {
            if c.name == name {
                return Some(c);
            }
            if let Some(f) = find(&c.replies, name) {
                return Some(f);
            }
        }
    }
    None
}

pub struct MenuScreen {
    list: VList,
    kind: MenuKind,
    title: String,
    rows: Vec<(Row, Act)>,
    /// The first focus goes to the current value (option lists).
    seated: std::cell::Cell<bool>,
    /// When it opened: a menu the app opened by itself ignores OK at first.
    opened_ms: u32,
    /// A value was picked in a menu that stays open: rebuilt with the new settings.
    rebuild: bool,
    state: State,
}

/// Options whose effect shows on the Settings page behind the menu (its sample, its colours):
/// their menu stays open, beside the sample, while values are tried.
const LIVE: &[&str] = &["text_scale", "content_scale", "font", "theme", "accent"];

/// A menu the app opened by itself (not the user's OK) ignores OK this long: a press meant for
/// the page under it must not pick a row.
const UNASKED_GRACE_MS: u32 = 800;

impl MenuScreen {
    pub fn new(entry: EntryId, kind: MenuKind, cx: &Cx<'_, RdHost>) -> Self {
        let mut m = MenuScreen {
            list: VList { entry, group: G, base: BASE, rect: Rect::new(0.0, 0.0, 10.0, 10.0), row_h: 64.0, text_size: 28 },
            kind,
            title: String::new(),
            rows: Vec::new(),
            seated: std::cell::Cell::new(false),
            opened_ms: 0,
            rebuild: false,
            state: State("menu".into()),
        };
        m.build(cx);
        m
    }

    fn live(&self) -> bool {
        matches!(self.kind, MenuKind::Option(k) if LIVE.contains(&k))
    }

    /// Menus the app presents by itself (the update prompt).
    fn unasked(&self) -> bool {
        matches!(self.kind, MenuKind::Update)
    }

    fn build(&mut self, cx: &Cx<'_, RdHost>) {
        let st = cx.views.store;
        let signed = st.signed_in();
        let mut rows: Vec<(Row, Act)> = Vec::new();
        let mut title = String::new();
        match &self.kind {
            MenuKind::Post(id) => {
                if let Some(it) = st.post(id) {
                    let p = &it.post;
                    title = p.title.clone();
                    rows.push((Row::new("Open comments").icon(Icon::Chat), Act::OpenPost(id.clone())));
                    if !matches!(it.media, Media::None) {
                        let (label, act) = match &it.media {
                            Media::Link { url, .. } => ("Open the link", Act::Link(url.clone())),
                            _ => ("Show full screen", Act::Viewer(id.clone())),
                        };
                        rows.push((Row::new(label).icon(Icon::Film), act));
                    }
                    rows.push((Row::new(if p.likes == Some(true) { "Remove upvote" } else { "Upvote" }).icon(Icon::ThumbUp), Act::Vote(p.name.clone(), if p.likes == Some(true) { 0 } else { 1 })));
                    rows.push((Row::new(if p.likes == Some(false) { "Remove downvote" } else { "Downvote" }).icon(Icon::ThumbDown), Act::Vote(p.name.clone(), if p.likes == Some(false) { 0 } else { -1 })));
                    rows.push((Row::new(if p.saved { "Remove from saved" } else { "Save" }).icon(Icon::Star), Act::Save(p.name.clone(), !p.saved)));
                    if signed {
                        rows.push((Row::new("Reply").icon(Icon::Chat), Act::Reply(p.name.clone(), id.clone())));
                        rows.push((Row::new("Hide").icon(Icon::ScreenOff), Act::Hide(id.clone())));
                    }
                    rows.push((Row::new(format!("Go to r/{}", p.subreddit)).icon(Icon::Subs).raw(), Act::Push(RdArg::Feed(Feed::Sub(p.subreddit.clone())))));
                    rows.push((Row::new(format!("Posts by u/{}", p.author)).icon(Icon::User).raw(), Act::Push(RdArg::Feed(Feed::User(p.author.clone(), UserTab::Submitted)))));
                    let fav = st.prefs.is_favorite(&p.subreddit);
                    rows.push((
                        Row::new(if fav { format!("Remove r/{} from favourites", p.subreddit) } else { format!("Add r/{} to favourites", p.subreddit) }).icon(Icon::Star).raw(),
                        Act::Favorite(p.subreddit.clone()),
                    ));
                    rows.push((Row::new(format!("Hide all posts from r/{}", p.subreddit)).icon(Icon::Shield).raw(), Act::FilterSub(p.subreddit.clone())));
                    rows.push((Row::new("Links in this post").icon(Icon::Leave), Act::Push(RdArg::Menu(MenuKind::Links(id.clone(), String::new())))));
                    rows.push((Row::new("Share (QR code)").icon(Icon::Phone), Act::Link(format!("https://www.reddit.com{}", p.permalink))));
                }
            }
            MenuKind::Comment(post, name) => {
                if let Some(c) = st.thread(post).and_then(|t| find(&t.tree, name)) {
                    title = format!("u/{}", c.author);
                    rows.push((Row::new(if c.likes == Some(true) { "Remove upvote" } else { "Upvote" }).icon(Icon::ThumbUp), Act::Vote(c.name.clone(), if c.likes == Some(true) { 0 } else { 1 })));
                    rows.push((Row::new(if c.likes == Some(false) { "Remove downvote" } else { "Downvote" }).icon(Icon::ThumbDown), Act::Vote(c.name.clone(), if c.likes == Some(false) { 0 } else { -1 })));
                    if signed {
                        rows.push((Row::new("Reply").icon(Icon::Chat), Act::Reply(c.name.clone(), post.clone())));
                        rows.push((Row::new("Save").icon(Icon::Star), Act::Save(c.name.clone(), true)));
                    }
                    rows.push((Row::new("Collapse or expand").icon(Icon::List), Act::Toggle(post.clone(), c.name.clone())));
                    rows.push((Row::new(format!("Posts by u/{}", c.author)).icon(Icon::User).raw(), Act::Push(RdArg::Feed(Feed::User(c.author.clone(), UserTab::Submitted)))));
                    rows.push((Row::new("Links in this comment").icon(Icon::Leave), Act::Push(RdArg::Menu(MenuKind::Links(post.clone(), c.name.clone())))));
                    if let Some(p) = st.post(post) {
                        rows.push((Row::new("Share (QR code)").icon(Icon::Phone), Act::Link(format!("https://www.reddit.com{}{}/", p.post.permalink, c.id))));
                    }
                }
            }
            MenuKind::Sub(sub) => {
                title = format!("r/{sub}");
                let fav = st.prefs.is_favorite(sub);
                rows.push((Row::new(if fav { "Remove from favourites" } else { "Add to favourites" }).icon(Icon::Star), Act::Favorite(sub.clone())));
                if signed {
                    let joined = st.about.get(&sub.to_ascii_lowercase()).is_some_and(|a| a.subscribed);
                    rows.push((Row::new(if joined { "Leave" } else { "Join" }).icon(Icon::Check), Act::Subscribe(sub.clone(), !joined)));
                }
                rows.push((Row::new(format!("Search in r/{sub}")).icon(Icon::Search).raw(), Act::Push(RdArg::Type(Typing::Search { sub: Some(sub.clone()) }))));
                rows.push((Row::new("Hide its posts from Home, Popular and All").icon(Icon::Shield), Act::FilterSub(sub.clone())));
                if let Some(a) = st.about.get(&sub.to_ascii_lowercase()) {
                    if !a.sub.public_description.is_empty() {
                        title = format!("r/{sub} \u{b7} {}", a.sub.public_description);
                    }
                }
                rows.push((Row::new("Share (QR code)").icon(Icon::Phone), Act::Link(format!("https://www.reddit.com/r/{sub}/"))));
            }
            MenuKind::Option(key) => {
                if let Some(o) = crate::prefs::opt(key) {
                    title = crate::i18n::tr(o.label).to_string();
                    let cur = st.prefs.get(key);
                    for (v, l) in o.values {
                        let act = if *key == "nsfw" && *v == "show" && !st.prefs.adult_ok { Act::Adult } else { Act::SetPref(key, v.to_string()) };
                        rows.push((Row::new(*l).value(if *v == cur { "\u{2713}" } else { "" }).active(*v == cur), act));
                    }
                }
            }
            MenuKind::AdultCheck => {
                title = "Show adult (NSFW) posts without blur? You must be 18 or older.".into();
                rows.push((Row::new("I am 18 or older").icon(Icon::Check), Act::SetPref("nsfw", "show".into())));
                rows.push((Row::new("Cancel"), Act::Close));
            }
            MenuKind::Links(post, comment) => {
                title = "Links".into();
                let mut links: Vec<(String, String)> = Vec::new();
                let body = if comment.is_empty() {
                    st.post(post).map(|p| {
                        if !p.post.is_self && !p.post.url.is_empty() {
                            links.push((p.post.url.clone(), rd_core::media::host(&p.post.url)));
                        }
                        p.post.selftext.clone()
                    })
                } else {
                    st.thread(post).and_then(|t| find(&t.tree, comment)).map(|c| c.body.clone())
                };
                for (u, t) in rd_core::markdown::links(&rd_core::markdown::parse(&body.unwrap_or_default())) {
                    if !links.iter().any(|(x, _)| *x == u) {
                        links.push((u, t));
                    }
                }
                if links.is_empty() {
                    rows.push((Row::new("No links here"), Act::Close));
                }
                for (u, t) in links {
                    let label = if t.is_empty() || t == u { u.clone() } else { format!("{t}  \u{b7}  {}", rd_core::media::host(&u)) };
                    let act = match rd_core::listing::parse_reddit_link(&u) {
                        Some(rd_core::listing::RedditLink::Sub(s)) => Act::Push(RdArg::Feed(Feed::Sub(s))),
                        Some(rd_core::listing::RedditLink::User(n)) => Act::Push(RdArg::Feed(Feed::User(n, UserTab::Submitted))),
                        Some(rd_core::listing::RedditLink::Post(id, c)) => Act::Push(RdArg::Post { id, comment: c }),
                        Some(rd_core::listing::RedditLink::Share(s)) => Act::Share(s),
                        None => Act::Link(u.clone()),
                    };
                    rows.push((Row::new(label).icon(Icon::Leave).raw(), act));
                }
            }
            MenuKind::Update => {
                let v = crate::update::found_version().unwrap_or_default();
                title = format!("{} {v} (you have {})", crate::i18n::tr("rdtnative update available:"), crate::update::current_version());
                // It opens by itself while the user browses: the first focus is the harmless row.
                rows.push((Row::new("Later (Settings \u{203a} Updates)"), Act::Close));
                rows.push((Row::new("Install now (closes the app)").icon(Icon::Check), Act::InstallUpdate));
            }
            MenuKind::Sort(_) | MenuKind::Time(_) | MenuKind::CommentSort(_) => {
                rows.push((Row::new("Close"), Act::Close));
            }
        }
        // Size and place the panel for its rows.
        let n = rows.len().max(1);
        let row_h = ui::ls(64.0).max(56.0);
        let ts = ui::sz(28);
        let w = 760.0;
        let title_h = if title.is_empty() { 0.0 } else { ui::sz(28) as f32 * 1.42 * 2.0 + 30.0 };
        let max_rows = ((1000.0 - title_h - 60.0) / row_h).floor().max(3.0) as usize;
        let h = n.min(max_rows) as f32 * row_h;
        let total = title_h + h + 40.0;
        let y = ((1080.0 - total) / 2.0).max(30.0);
        // A live option's menu stands left, over the Settings list, so its sample stays in view.
        let x = if self.live() { 100.0 } else { (1920.0 - w) / 2.0 };
        self.list = VList { rect: Rect::new(x + 20.0, y + title_h + 20.0, w - 40.0, h), row_h, text_size: ts, ..self.list };
        self.title = title;
        self.rows = rows;
    }

    fn run(&mut self, i: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some((_, act)) = self.rows.get(i).cloned() else { return };
        let me = self.list.entry;
        let close = |fx: &mut Effects<'_, RdHost>| fx.push(Fx::Nav(NavOp::Dismiss(me)));
        match act {
            Act::OpenPost(id) => {
                close(fx);
                fx.push(Fx::App(RdFx::MarkRead(id.clone())));
                fx.push(Fx::Nav(NavOp::Push(RdArg::Post { id, comment: None })));
            }
            Act::Vote(name, dir) => {
                close(fx);
                fx.push(Fx::App(RdFx::Vote { fullname: name, dir }));
            }
            Act::Save(name, on) => {
                close(fx);
                fx.push(Fx::App(RdFx::Save { fullname: name, on }));
            }
            Act::Hide(id) => {
                close(fx);
                fx.push(Fx::App(RdFx::Hide { id }));
            }
            Act::Viewer(id) => {
                close(fx);
                fx.push(Fx::Nav(NavOp::Push(RdArg::Viewer { id, index: 0 })));
            }
            Act::Link(url) => {
                close(fx);
                fx.push(Fx::Nav(NavOp::Push(RdArg::Link(url))));
            }
            Act::Push(arg) => {
                close(fx);
                fx.push(Fx::Nav(NavOp::Push(arg)));
            }
            Act::Favorite(sub) => {
                close(fx);
                let on = cx.views.store.prefs.is_favorite(&sub);
                fx.push(Fx::App(RdFx::EditPrefs(
                    |p, s| {
                        p.toggle_favorite(s);
                    },
                    sub.clone(),
                )));
                fx.push(Fx::App(RdFx::Toast(if on { format!("Removed r/{sub} from favourites") } else { format!("Added r/{sub} to favourites") })));
            }
            Act::FilterSub(sub) => {
                close(fx);
                fx.push(Fx::App(RdFx::Typed(Typing::FilterSub, sub.clone())));
                fx.push(Fx::App(RdFx::Toast(format!("Posts from r/{sub} are hidden (Settings \u{203a} Filters)"))));
            }
            Act::Subscribe(sub, on) => {
                close(fx);
                fx.push(Fx::App(RdFx::Subscribe { sub, on }));
            }
            Act::Toggle(post, name) => {
                close(fx);
                fx.push(Fx::App(RdFx::ToggleComment { post, name }));
            }
            Act::Reply(parent, post) => {
                close(fx);
                fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::Reply { parent, post }))));
            }
            // Stays open: the next frame shows the choice ticked and the page behind it changed;
            // OK on the value already in use closes it.
            Act::SetPref(key, v) if self.live() && cx.views.store.prefs.get(key) == v => close(fx),
            Act::SetPref(key, v) if self.live() => {
                fx.push(Fx::App(RdFx::SetPref(key, v)));
                self.rebuild = true;
            }
            Act::SetPref(key, v) => {
                close(fx);
                fx.push(Fx::App(RdFx::SetPref(key, v)));
            }
            Act::Adult => {
                close(fx);
                fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::AdultCheck))));
            }
            Act::Share(url) => {
                close(fx);
                fx.push(Fx::App(RdFx::OpenShare(url)));
            }
            Act::InstallUpdate => {
                close(fx);
                crate::update::install();
                fx.push(Fx::App(RdFx::Toast("Homebrew Channel is installing the update; the app closes when it is done".into())));
            }
            Act::Close => close(fx),
        }
    }
}

impl Machine<RdHost> for MenuScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Mount => {
                ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.opened_ms = cx.views.now_ms;
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                Handled::Yes
            }
            ScreenEvent::Activate(_) if self.unasked() && cx.views.now_ms.wrapping_sub(self.opened_ms) < UNASKED_GRACE_MS => Handled::Yes,
            ScreenEvent::Activate(e) => {
                if let Some(i) = self.list.index(*e, self.rows.len()) {
                    self.run(i, cx, fx);
                }
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

impl Focusable<RdHost> for MenuScreen {
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(self.rows.len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        self.list.neighbour(key, dir, self.rows.len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        self.list.place(*key, self.rows.len(), self.list.focused(cx, self.rows.len()))
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if !self.seated.replace(true) {
            let cur = self.rows.iter().position(|(r, _)| r.active).unwrap_or(0);
            return self.list.key(cur);
        }
        if self.list.index(want.elem, self.rows.len()).is_some() {
            return want;
        }
        // An option list opens on its current value.
        let cur = self.rows.iter().position(|(r, _)| r.active).unwrap_or(0);
        self.list.key(cur)
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<RdHost> for MenuScreen {
    fn name(&self) -> &'static str {
        "menu"
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
        if std::mem::take(&mut self.rebuild) {
            self.build(cx);
        }
        // A live option's menu dims the page only a little: it is there to be seen.
        ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, if self.live() { 0.2 } else { 0.55 }]);
        let r = self.list.rect;
        let ts = ui::sz(28);
        let title_h = if self.title.is_empty() { 0.0 } else { ts as f32 * 1.42 * 2.0 + 30.0 };
        let panel = Rect::new(r.x - 20.0, r.y - title_h - 20.0, r.w + 40.0, r.h + title_h + 40.0);
        p.shadow(panel, 24.0, 50.0, 12.0, [0.0, 0.0, 0.0, 0.6]);
        ui::fill(p, panel, 24.0, ui::surface(0.1));
        if !self.title.is_empty() {
            let lines = ui::wrap_lines(f.measure, &self.title, r.w - 20.0, ts, true, 2);
            for (i, l) in lines.iter().enumerate() {
                ui::text_cut(p, f.measure, l, r.x + 10.0, panel.y + 24.0 + i as f32 * ts as f32 * 1.42, r.w - 20.0, ts, ui::WHITE, ui::ALIGN_LEFT, true);
            }
        }
        let rows: Vec<Row> = self.rows.iter().map(|(r, _)| r.clone()).collect();
        self.list.draw(p, f.measure, &rows, self.list.focused(cx, rows.len()));
        self.list.stops(f, rows.len());
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
