//! A post and its comments. A bar of buttons on top (vote, save, reply, comment sort, the
//! subreddit, the author, links, refresh); the post on the left (title, media, text) and the
//! comment tree on the right. Up/Down step through comments (a long one is paged through), Left
//! goes to the parent comment, OK opens or closes a thread, holding OK opens a comment's menu,
//! and the colour buttons vote. "Load more" rows fetch in place.

use super::common::{self, Color, State};
use crate::flow::Flow;
use crate::host::{MenuKind, RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::images::{self, Fit};
use crate::richtext;
use crate::store::PostItem;
use crate::ui::{self, icon::Kind as Icon, ChoiceBox};
use plx_machine::machine::{By, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::comments::{self, Row as CRow};
use rd_core::listing::{CommentSort, Feed, UserTab};
use rd_core::media::Media;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};

const G_BAR: GroupId = GroupId(1);
const G_POST: GroupId = GroupId(2);
const G_COMMENTS: GroupId = GroupId(3);
const BAR_BASE: u32 = 0x3000_0000;
const POST_BASE: u32 = 0x1000_0000;
const COM_BASE: u32 = 0x2000_0000;

const PAD: f32 = 22.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bar {
    Up,
    Down,
    Save,
    Reply,
    Sort,
    Sub,
    User,
    Links,
    Refresh,
}

/// The post column's items.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PostPart {
    Head,
    Media,
    Body,
}

/// Depth rail colours.
const RAILS: [[f32; 4]; 6] = [
    [0.36, 0.58, 1.0, 0.8],
    [0.25, 0.78, 0.55, 0.8],
    [1.0, 0.72, 0.25, 0.8],
    [0.85, 0.4, 0.9, 0.8],
    [1.0, 0.45, 0.45, 0.8],
    [0.3, 0.8, 0.85, 0.8],
];

/// One comment row, as drawn: what it is and the data the drawing needs (owned, so it outlives
/// the borrow of the store).
#[derive(Clone)]
enum CItem {
    Comment { name: String, author: String, body: String, score: i64, score_hidden: bool, created: i64, depth: u32, op: bool, mod_: bool, likes: Option<bool>, collapsed: bool, hidden: usize, flair: Option<String>, stickied: bool },
    More { id: String, depth: u32, count: i64, cont: bool, parent: String },
}

pub struct PostScreen {
    entry: EntryId,
    id: String,
    left: Flow,
    right: Flow,
    sig_left: Cell<(u32, u64)>,
    sig_right: Cell<(u32, u32, u64)>,
    rows: RefCell<Vec<CItem>>,
    bar_rects: RefCell<Vec<Rect>>,
    choice: Option<ChoiceBox>,
    focus_req: Cell<Option<u32>>,
    /// Comments whose spoilers were revealed.
    revealed: RefCell<std::collections::HashSet<String>>,
    initial_focus_done: Cell<bool>,
    state: State,
}

fn bar_items(signed_in: bool) -> Vec<Bar> {
    let mut v = vec![Bar::Up, Bar::Down, Bar::Save];
    if signed_in {
        v.push(Bar::Reply);
    }
    v.extend([Bar::Sort, Bar::Sub, Bar::User, Bar::Links, Bar::Refresh]);
    v
}

fn top_h() -> f32 {
    ui::ls(110.0).max(100.0)
}

fn split_x() -> f32 {
    760.0
}

impl PostScreen {
    pub fn new(entry: EntryId, id: String, _comment: Option<String>) -> Self {
        let top = top_h();
        let mut left = Flow::new(entry, G_POST, POST_BASE, Rect::new(60.0, top, split_x() - 60.0 - 30.0, 1080.0 - top));
        left.row_gap = 20.0;
        left.pad_top = 6.0;
        left.margin = 30.0;
        let mut right = Flow::new(entry, G_COMMENTS, COM_BASE, Rect::new(split_x() + 20.0, top, 1920.0 - split_x() - 20.0 - 60.0, 1080.0 - top));
        right.row_gap = 10.0;
        right.pad_top = 6.0;
        right.margin = 40.0;
        PostScreen {
            entry,
            state: State(format!("post {id}")),
            id,
            left,
            right,
            sig_left: Cell::new((u32::MAX, 0)),
            sig_right: Cell::new((u32::MAX, 0, 0)),
            rows: Default::default(),
            bar_rects: Default::default(),
            choice: None,
            focus_req: Cell::new(None),
            revealed: Default::default(),
            initial_focus_done: Cell::new(false),
        }
    }

    fn item<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a PostItem> {
        cx.views.store.post(&self.id)
    }

    fn parts(&self, cx: &Cx<'_, RdHost>) -> Vec<PostPart> {
        let Some(it) = self.item(cx) else { return vec![] };
        let mut v = vec![PostPart::Head];
        if !matches!(it.media, Media::None) || rd_core::media::preview(&it.post, 800).is_some() {
            v.push(PostPart::Media);
        }
        if !it.post.selftext.trim().is_empty() {
            v.push(PostPart::Body);
        }
        v
    }

    fn prefs_sig() -> u64 {
        (ui::text_scale() * 100.0) as u64 | ((ui::content_scale() * 100.0) as u64) << 12 | (ui::font_code(false) as u64) << 24
    }

    fn title_size() -> i32 {
        ui::sz(34)
    }

    fn media_h(&self, it: &PostItem, w: f32) -> f32 {
        let pic = self.media_pic(it);
        match pic {
            Some((_, pw, ph)) if pw > 0 && ph > 0 => (w * ph as f32 / pw as f32).clamp(220.0, 620.0),
            _ => match &it.media {
                Media::Link { .. } if pic.is_none() => ui::ls(130.0),
                _ => 420.0,
            },
        }
    }

    /// The picture shown for the post's media: (url, w, h).
    fn media_pic(&self, it: &PostItem) -> Option<(String, u32, u32)> {
        if let Some(p) = rd_core::media::preview(&it.post, 1100) {
            return Some((p.url, p.w, p.h));
        }
        match &it.media {
            Media::Image(p) => Some((p.url.clone(), p.w, p.h)),
            Media::Gallery(g) => g.first().map(|i| (i.pic.url.clone(), i.pic.w, i.pic.h)),
            Media::Video(v) => v.poster.as_ref().map(|p| (p.url.clone(), p.w, p.h)),
            Media::YouTube { id, .. } => Some((format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"), 480, 360)),
            Media::Link { preview: Some(p), .. } => Some((p.url.clone(), p.w, p.h)),
            _ => None,
        }
    }

    fn head_h(&self, it: &PostItem, w: f32) -> f32 {
        let ts = Self::title_size();
        let ms = ui::sz(22);
        let n = richtext::wrap_title(&it.post.title, w - 2.0 * PAD, ts, true, 8).len().max(1);
        PAD + ms as f32 * 1.42 + 8.0 + richtext::title_h(n, ts) + 10.0 + ms as f32 * 1.42 + PAD
    }

    fn body_laid(&self, it: &PostItem, w: f32) -> std::rc::Rc<richtext::Laid> {
        richtext::cached(&format!("post:{}", it.post.id), &it.post.selftext, w - 2.0 * PAD, ui::csz(27))
    }

    fn relayout_left(&self, cx: &Cx<'_, RdHost>) {
        let Some(it) = self.item(cx) else { return };
        let sig = (cx.views.store.gen, Self::prefs_sig());
        if sig == self.sig_left.get() {
            return;
        }
        self.sig_left.set(sig);
        let w = self.left.col_w();
        let heights: Vec<f32> = self
            .parts(cx)
            .iter()
            .map(|p| match p {
                PostPart::Head => self.head_h(it, w),
                PostPart::Media => self.media_h(it, w),
                PostPart::Body => self.body_laid(it, w).h + 2.0 * PAD,
            })
            .collect();
        self.left.set_heights(&heights);
    }

    fn comment_rows(&self, cx: &Cx<'_, RdHost>) -> Vec<CItem> {
        let Some(t) = cx.views.store.thread(&self.id) else { return vec![] };
        let op = self.item(cx).map(|i| i.post.author.clone()).unwrap_or_default();
        comments::flatten(&t.tree, &t.toggled)
            .into_iter()
            .map(|r| match r {
                CRow::Comment { c, depth, collapsed, hidden_replies } => CItem::Comment {
                    name: c.name.clone(),
                    author: c.author.clone(),
                    body: c.body.clone(),
                    score: c.score,
                    score_hidden: c.score_hidden,
                    created: c.created_utc,
                    depth,
                    op: c.is_submitter || (!op.is_empty() && c.author == op),
                    mod_: c.distinguished.as_deref() == Some("moderator"),
                    likes: c.likes,
                    collapsed,
                    hidden: hidden_replies,
                    flair: c.author_flair_text.clone(),
                    stickied: c.stickied,
                },
                CRow::More { m, depth } => CItem::More { id: m.id.clone(), depth, count: m.count, cont: m.is_continue_thread(), parent: m.parent_id.clone() },
            })
            .collect()
    }

    fn indent(depth: u32) -> f32 {
        depth.min(10) as f32 * ui::ls(26.0).min(34.0)
    }

    fn comment_h(&self, c: &CItem, w: f32) -> f32 {
        let ms = ui::sz(21);
        match c {
            CItem::Comment { name, body, depth, collapsed, .. } => {
                let x = Self::indent(*depth) + 20.0;
                let head = ms as f32 * 1.42;
                if *collapsed {
                    return head + 2.0 * 12.0;
                }
                let laid = richtext::cached(name, body, w - x - 34.0, ui::csz(25));
                let pics = comment_pictures(body).len() as f32;
                12.0 + head + 6.0 + laid.h + pics * (COMMENT_PIC_H + 12.0) + 14.0
            }
            CItem::More { .. } => ms as f32 * 1.42 + 24.0,
        }
    }

    fn relayout_right(&self, cx: &Cx<'_, RdHost>) {
        let gen = cx.views.store.thread(&self.id).map_or(0, |t| t.gen);
        let sig = (gen, cx.views.store.gen, Self::prefs_sig());
        if sig == self.sig_right.get() {
            return;
        }
        self.sig_right.set(sig);
        let rows = self.comment_rows(cx);
        let w = self.right.col_w();
        let heights: Vec<f32> = rows.iter().map(|c| self.comment_h(c, w)).collect();
        self.right.set_heights(&heights);
        *self.rows.borrow_mut() = rows;
    }

    fn bar_label(&self, b: Bar, cx: &Cx<'_, RdHost>) -> (String, Icon, bool) {
        let it = self.item(cx);
        let sort = cx.views.store.thread(&self.id).map_or(CommentSort::Confidence, |t| t.sort);
        match b {
            Bar::Up => (it.map_or(String::new(), |i| common::short_count(i.post.score)), Icon::ThumbUp, it.is_some_and(|i| i.post.likes == Some(true))),
            Bar::Down => (String::new(), Icon::ThumbDown, it.is_some_and(|i| i.post.likes == Some(false))),
            Bar::Save => (if it.is_some_and(|i| i.post.saved) { "Saved" } else { "Save" }.into(), Icon::Star, it.is_some_and(|i| i.post.saved)),
            Bar::Reply => ("Reply".into(), Icon::Chat, false),
            Bar::Sort => (sort.label().into(), Icon::Trending, false),
            Bar::Sub => (it.map_or(String::new(), |i| format!("r/{}", i.post.subreddit)), Icon::Subs, false),
            Bar::User => (it.map_or(String::new(), |i| format!("u/{}", i.post.author)), Icon::User, false),
            Bar::Links => ("Links".into(), Icon::Leave, false),
            Bar::Refresh => (String::new(), Icon::Repeat, false),
        }
    }

    fn activate_bar(&mut self, i: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let items = bar_items(cx.views.store.signed_in());
        let (Some(b), Some(it)) = (items.get(i).copied(), self.item(cx)) else { return };
        let name = it.post.name.clone();
        match b {
            Bar::Up => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(true) { 0 } else { 1 } })),
            Bar::Down => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(false) { 0 } else { -1 } })),
            Bar::Save => fx.push(Fx::App(RdFx::Save { fullname: name, on: !it.post.saved })),
            Bar::Reply => fx.push(Fx::Nav(NavOp::Push(RdArg::Type(Typing::Reply { parent: name, post: self.id.clone() })))),
            Bar::Sort => {
                let anchor = self.bar_rects.borrow().get(i).copied().unwrap_or(Rect::new(900.0, 30.0, 200.0, 60.0));
                let cur = cx.views.store.thread(&self.id).map_or(CommentSort::Confidence, |t| t.sort);
                let b = ChoiceBox::new(self.entry, i, CommentSort::ALL.iter().map(|s| s.label().to_string()).collect(), CommentSort::ALL.iter().position(|s| *s == cur).unwrap_or(0), anchor);
                self.focus_req.set(Some(b.list.elem(b.current)));
                ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.choice = Some(b);
            }
            Bar::Sub => fx.push(Fx::Nav(NavOp::Push(RdArg::Feed(Feed::Sub(it.post.subreddit.clone()))))),
            Bar::User => fx.push(Fx::Nav(NavOp::Push(RdArg::Feed(Feed::User(it.post.author.clone(), UserTab::Submitted))))),
            Bar::Links => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Links(self.id.clone(), String::new()))))),
            Bar::Refresh => fx.push(Fx::App(RdFx::LoadThread { id: self.id.clone(), sort: None, reload: true })),
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn close_choice(&mut self) {
        if let Some(b) = self.choice.take() {
            ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            self.focus_req.set(Some(BAR_BASE + b.row as u32));
        }
    }

    fn open_media(&self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some(it) = self.item(cx) else { return };
        open_media(it, cx, fx);
    }

    fn blurred(&self, cx: &Cx<'_, RdHost>, it: &PostItem) -> bool {
        let prefs = &cx.views.store.prefs;
        (it.post.over_18 && prefs.get("nsfw") == "blur") || (it.post.spoiler && prefs.get("spoilers") == "blur")
    }

    fn comment_at(&self, i: usize) -> Option<CItem> {
        self.rows.borrow().get(i).cloned()
    }

    fn vote_comment(&self, i: usize, dir: i8, fx: &mut Effects<'_, RdHost>) {
        if let Some(CItem::Comment { name, likes, .. }) = self.comment_at(i) {
            let d = match (dir, likes) {
                (1, Some(true)) | (-1, Some(false)) => 0,
                _ => dir,
            };
            fx.push(Fx::App(RdFx::Vote { fullname: name, dir: d }));
        }
    }

    /// The row index of the parent of comment row `i`.
    fn parent_row(&self, i: usize) -> Option<usize> {
        let rows = self.rows.borrow();
        let depth = match rows.get(i)? {
            CItem::Comment { depth, .. } | CItem::More { depth, .. } => *depth,
        };
        if depth == 0 {
            return None;
        }
        (0..i).rev().find(|&j| matches!(&rows[j], CItem::Comment { depth: d, .. } if *d + 1 == depth))
    }

    /// The next comment at the same or a shallower depth (skips a thread).
    fn next_sibling(&self, i: usize) -> Option<usize> {
        let rows = self.rows.borrow();
        let depth = match rows.get(i)? {
            CItem::Comment { depth, .. } | CItem::More { depth, .. } => *depth,
        };
        ((i + 1)..rows.len()).find(|&j| match &rows[j] {
            CItem::Comment { depth: d, .. } | CItem::More { depth: d, .. } => *d <= depth,
        })
    }
}

/// Height of a picture shown under a comment.
const COMMENT_PIC_H: f32 = 300.0;

/// Pictures a comment links to (Reddit and Imgur uploads), at most two: shown under its text.
fn comment_pictures(body: &str) -> Vec<String> {
    if !body.contains("redd.it/") && !body.contains("imgur.com/") {
        return Vec::new();
    }
    let blocks = rd_core::markdown::parse(body);
    rd_core::markdown::links(&blocks)
        .into_iter()
        .map(|(u, _)| rd_core::media::unwrap_media_url(&u))
        .filter(|u| {
            let host = rd_core::media::host(u);
            let path = u.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
            let pic = [".jpg", ".jpeg", ".png", ".webp"].iter().any(|e| path.ends_with(e));
            pic && matches!(host.as_str(), "i.redd.it" | "preview.redd.it" | "i.imgur.com")
        })
        .take(2)
        .collect()
}

/// Opens a reddit.com link inside the app.
/// Opens a post's media: a picture or gallery in the viewer, a video playing, a link its page.
pub fn open_media(it: &PostItem, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
    let prefs = &cx.views.store.prefs;
    let blurred = (it.post.over_18 && prefs.get("nsfw") == "blur") || (it.post.spoiler && prefs.get("spoilers") == "blur");
    let id = it.post.id.clone();
    match &it.media {
        Media::Link { url, .. } => {
            if let Some(link) = rd_core::listing::parse_reddit_link(url) {
                open_reddit_link(link, fx);
            } else {
                fx.push(Fx::Nav(NavOp::Push(RdArg::Link(url.clone()))));
            }
        }
        Media::None => {}
        Media::YouTube { id, .. } => fx.push(Fx::App(RdFx::OpenYouTube(id.clone()))),
        // Videos and GIFs play straight away (unless blurred: the viewer shows them first).
        Media::Video(v) if !blurred => match crate::player::pick_url(v, prefs.get("video_stream")) {
            Some(url) => fx.push(Fx::Nav(NavOp::Push(RdArg::Play { url, title: it.post.title.clone(), looping: v.gif }))),
            None => fx.push(Fx::Nav(NavOp::Push(RdArg::Viewer { id, index: 0 }))),
        },
        _ => fx.push(Fx::Nav(NavOp::Push(RdArg::Viewer { id, index: 0 }))),
    }
}

pub fn open_reddit_link(link: rd_core::listing::RedditLink, fx: &mut Effects<'_, RdHost>) {
    use rd_core::listing::RedditLink;
    let arg = match link {
        RedditLink::Sub(s) => RdArg::Feed(Feed::Sub(s)),
        RedditLink::User(u) => RdArg::Feed(Feed::User(u, UserTab::Submitted)),
        RedditLink::Post(id, c) => RdArg::Post { id, comment: c },
        RedditLink::Share(url) => {
            fx.push(Fx::App(RdFx::OpenShare(url)));
            return;
        }
    };
    fx.push(Fx::Nav(NavOp::Push(arg)));
}

impl Machine<RdHost> for PostScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        // The pointer edge glide: the pane under the pointer scrolls by pixels.
        if let Some(dy) = common::wheel_dy(ev) {
            let (x, _) = ui::pointer();
            let flow = if self.left.contains_x(x) && !self.right.contains_x(x) { &self.left } else { &self.right };
            if flow.glide(dy) {
                if let Some(j) = flow.focused(cx).and_then(|i| flow.keep_in_view(i)) {
                    self.focus_req.set(Some(flow.elem(j)));
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        if let Some(e) = common::press_hold(ev, cx) {
            if let Some(i) = self.right.index(e) {
                if let Some(CItem::Comment { name, .. }) = self.comment_at(i) {
                    fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Comment(self.id.clone(), name)))));
                }
            } else if self.left.index(e).is_some() {
                fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Post(self.id.clone())))));
            }
            return Handled::Yes;
        }
        if self.choice.is_some() && common::back_any(ev) {
            if common::back_down(ev) {
                self.close_choice();
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            return Handled::Yes;
        }
        if let Some(c) = common::color_key(ev) {
            if !cx.views.store.prefs.on("color_keys") {
                return Handled::Yes;
            }
            let focus = cx.focus.current.map(|k| k.elem);
            let on_comment = focus.and_then(|e| self.right.index(e));
            match (c, on_comment) {
                (Color::Green, Some(i)) => self.vote_comment(i, 1, fx),
                (Color::Red, Some(i)) => self.vote_comment(i, -1, fx),
                (Color::Green, None) => self.activate_bar(0, cx, fx),
                (Color::Red, None) => self.activate_bar(1, cx, fx),
                (Color::Yellow, _) => self.activate_bar(2, cx, fx),
                (Color::Blue, _) => {
                    // Jump between the post and its comments.
                    if on_comment.is_some() {
                        self.focus_req.set(Some(POST_BASE));
                    } else if self.right.len() > 0 {
                        self.focus_req.set(Some(COM_BASE));
                    }
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
            }
            return Handled::Yes;
        }
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::App(RdFx::MarkRead(self.id.clone())));
                fx.push(Fx::App(RdFx::LoadThread { id: self.id.clone(), sort: None, reload: false }));
                if let Some(it) = self.item(cx) {
                    if matches!(it.media, Media::Imgur(_)) {
                        fx.push(Fx::App(RdFx::ResolveImgur(self.id.clone())));
                    }
                }
            }
            ScreenEvent::Async(_, RdMsg::Changed) | ScreenEvent::Uncover => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::FocusMoved { to, by, .. } => {
                if let Some(i) = self.right.index(to.elem) {
                    self.right.on_focus(i, *by == By::Pointer);
                } else if let Some(i) = self.left.index(to.elem) {
                    self.left.on_focus(i, *by == By::Pointer);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Activate(e) => {
                let e = *e;
                if e >= ui::CHOICE_BASE {
                    if let Some(b) = self.choice.take() {
                        ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        if let Some(i) = b.list.index(e, b.options.len()) {
                            fx.push(Fx::App(RdFx::LoadThread { id: self.id.clone(), sort: Some(CommentSort::ALL[i]), reload: true }));
                            fx.push(Fx::App(RdFx::EditPrefs(|p, v| p.set("comment_sort", v), CommentSort::ALL[i].key().to_string())));
                            self.right.to_top();
                        }
                        self.focus_req.set(Some(BAR_BASE + b.row as u32));
                    }
                } else if e >= BAR_BASE {
                    self.activate_bar((e - BAR_BASE) as usize, cx, fx);
                } else if let Some(i) = self.right.index(e) {
                    match self.comment_at(i) {
                        Some(CItem::Comment { name, body, .. }) => {
                            // A comment with hidden spoilers reveals them first.
                            if body.contains(">!") && !self.revealed.borrow().contains(&name) {
                                self.revealed.borrow_mut().insert(name);
                            } else {
                                fx.push(Fx::App(RdFx::ToggleComment { post: self.id.clone(), name }));
                            }
                        }
                        Some(CItem::More { id, cont, parent, .. }) => {
                            if cont {
                                // "Continue this thread": the parent comment as its own page.
                                let c = parent.trim_start_matches("t1_").to_string();
                                fx.push(Fx::Nav(NavOp::Push(RdArg::Post { id: self.id.clone(), comment: Some(c) })));
                            } else {
                                fx.push(Fx::App(RdFx::MoreComments { post: self.id.clone(), more_id: id }));
                            }
                        }
                        None => {}
                    }
                } else if let Some(i) = self.left.index(e) {
                    match self.parts(cx).get(i) {
                        Some(PostPart::Media) => self.open_media(cx, fx),
                        Some(PostPart::Body) => {
                            let k = format!("post:{}", self.id);
                            if !self.revealed.borrow_mut().insert(k) {
                                fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Links(self.id.clone(), String::new())))));
                            }
                        }
                        _ => self.open_media(cx, fx),
                    }
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<RdHost> for PostScreen {
    fn groups(&self, cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        if let Some(b) = &self.choice {
            out.push(b.list.spec(b.options.len()));
            return;
        }
        self.relayout_left(cx);
        self.relayout_right(cx);
        out.push(GroupSpec {
            id: G_BAR,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(40.0, 20.0, 1840.0, top_h() - 30.0),
            len: bar_items(cx.views.store.signed_in()).len(),
            elem: ElemKind::Control,
        });
        if self.left.len() > 0 {
            out.push(self.left.spec());
        }
        if self.right.len() > 0 {
            out.push(self.right.spec());
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(if *key >= ui::CHOICE_BASE {
            ui::CHOICE_GROUP
        } else if *key >= BAR_BASE {
            G_BAR
        } else if *key >= COM_BASE {
            G_COMMENTS
        } else {
            G_POST
        })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, RdHost>) -> Step<u32> {
        if let Some(b) = &self.choice {
            return b.list.neighbour(key, dir, b.options.len());
        }
        let k = |e: u32| Step::Move(FocusKey { entry: self.entry, elem: e });
        if key.elem >= BAR_BASE {
            let i = key.elem - BAR_BASE;
            let n = bar_items(cx.views.store.signed_in()).len() as u32;
            return match dir {
                Dir::Left if i > 0 => k(key.elem - 1),
                Dir::Right if i + 1 < n => k(key.elem + 1),
                Dir::Down if self.right.len() > 0 && i >= 3 => {
                    let first = self.right.visible().start.min(self.right.len() - 1);
                    k(self.right.elem(first))
                }
                Dir::Down if self.left.len() > 0 => k(self.left.elem(self.left.visible().start.min(self.left.len() - 1))),
                _ => Step::Edge,
            };
        }
        if let Some(i) = self.right.index(key.elem) {
            return match dir {
                Dir::Left => match self.parent_row(i) {
                    Some(p) => {
                        self.right.reveal(p);
                        k(self.right.elem(p))
                    }
                    None if self.left.len() > 0 => k(self.left.elem(self.left.visible().start.min(self.left.len() - 1))),
                    None => Step::Edge,
                },
                Dir::Right => match self.next_sibling(i) {
                    Some(n) => {
                        self.right.reveal(n);
                        k(self.right.elem(n))
                    }
                    None => Step::Edge,
                },
                Dir::Up => match self.right.neighbour(key, dir) {
                    Step::Edge => k(BAR_BASE + 4.min(bar_items(cx.views.store.signed_in()).len() as u32 - 1)),
                    s => s,
                },
                _ => self.right.neighbour(key, dir),
            };
        }
        if self.left.index(key.elem).is_some() {
            return match dir {
                Dir::Right if self.right.len() > 0 => k(self.right.elem(self.right.visible().start.min(self.right.len() - 1))),
                Dir::Up => match self.left.neighbour(key, dir) {
                    Step::Edge => k(BAR_BASE),
                    s => s,
                },
                _ => self.left.neighbour(key, dir),
            };
        }
        Step::Edge
    }
    fn place(&self, key: &u32, cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        if *key >= ui::CHOICE_BASE {
            let b = self.choice.as_ref()?;
            return b.list.place(*key, b.options.len(), b.list.focused(cx, b.options.len()));
        }
        if *key >= BAR_BASE {
            let r = *self.bar_rects.borrow().get((*key - BAR_BASE) as usize)?;
            return Some(Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(*key - BAR_BASE) });
        }
        if *key >= COM_BASE {
            return self.right.place(*key);
        }
        self.left.place(*key)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        let want = match self.focus_req.take() {
            Some(e) => FocusKey { entry: self.entry, elem: e },
            None => want,
        };
        if let Some(b) = &self.choice {
            return if b.owns(want.elem) { want } else { b.list.key(b.current) };
        }
        // The first time the post is there: its title, not the first bar button.
        if !self.initial_focus_done.get() && self.left.len() > 0 {
            self.initial_focus_done.set(true);
            if want.elem >= BAR_BASE && want.elem < ui::CHOICE_BASE {
                return self.left.key(0);
            }
        }
        let valid = if want.elem >= ui::CHOICE_BASE {
            false
        } else if want.elem >= BAR_BASE {
            ((want.elem - BAR_BASE) as usize) < bar_items(cx.views.store.signed_in()).len()
        } else if want.elem >= COM_BASE {
            self.right.index(want.elem).is_some()
        } else {
            self.left.index(want.elem).is_some()
        };
        if valid {
            return want;
        }
        // First visit: the post's title.
        if self.left.len() > 0 {
            return self.left.key(0);
        }
        FocusKey { entry: self.entry, elem: BAR_BASE }
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        match g {
            G_COMMENTS if self.right.len() > 0 => self.right.key(self.right.visible().start.min(self.right.len() - 1)),
            G_POST if self.left.len() > 0 => self.left.key(0),
            _ => FocusKey { entry: self.entry, elem: BAR_BASE },
        }
    }
}

impl PostScreen {
    fn draw_bar(&self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let cx = f.cx;
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, top_h() + 6.0));
        let items = bar_items(cx.views.store.signed_in());
        let labels: Vec<(String, Icon, bool)> = items.iter().map(|b| self.bar_label(*b, cx)).collect();
        let bh = ui::ls(58.0).max(52.0);
        let rects = common::button_row(f.measure, &labels.iter().map(|(l, _, _)| (l.as_str(), true)).collect::<Vec<_>>(), 60.0, 30.0, bh, 12.0, None);
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for (i, (r, (l, icon, active))) in rects.iter().zip(labels.iter()).enumerate() {
            common::button(p, f.measure, *r, l, Some(*icon), focus == Some(BAR_BASE + i as u32), *active, true);
            if self.choice.is_none() {
                ui::stop(f, FocusKey { entry: self.entry, elem: BAR_BASE + i as u32 }, *r, Rect::new(0.0, 0.0, 1920.0, 1080.0));
            }
        }
        *self.bar_rects.borrow_mut() = rects;
    }

    fn draw_post_part(&self, f: &mut DrawFrame<'_, '_, RdHost>, it: &PostItem, part: PostPart, r: Rect, focused: bool) {
        let p = f.painter;
        let m = f.measure;
        let cx = f.cx;
        let prefs = &cx.views.store.prefs;
        if focused {
            ui::fill(p, r, 16.0, ui::surface(0.12));
            p.rring(r, 16.0, 3.0, ui::with_alpha(ui::accent(), 0.9));
        }
        match part {
            PostPart::Head => {
                let ms = ui::sz(22);
                let ts = Self::title_size();
                let mut y = r.y + PAD;
                let mut x = r.x + PAD;
                ui::text_raw(p, &format!("r/{}", it.post.subreddit), x, y, ms, ui::accent(), ui::ALIGN_LEFT, true);
                x += ui::text_width(m, &format!("r/{}", it.post.subreddit), ms, true) + 16.0;
                let meta = format!("u/{}  \u{b7}  {}", it.post.author, rd_core::listing::age(it.post.created_utc, rd_core::now_secs() as i64));
                ui::text_cut(p, m, &meta, x, y, r.x + r.w - PAD - x, ms, ui::DIM, ui::ALIGN_LEFT, false);
                y += ms as f32 * 1.42 + 8.0;
                let lines = richtext::wrap_title(&it.post.title, r.w - 2.0 * PAD, ts, true, 8);
                y = richtext::draw_lines(p, m, &lines, r.x + PAD, y, r.w - 2.0 * PAD, ts, ui::WHITE, true) + 10.0;
                let mut bx = r.x + PAD;
                let bs = (ms as f32 * 0.85) as i32;
                if it.post.over_18 {
                    bx += common::badge(p, m, bx, y + 2.0, "NSFW", common::NSFW_RED, ui::WHITE, bs) + 8.0;
                }
                if it.post.spoiler {
                    bx += common::badge(p, m, bx, y + 2.0, "SPOILER", common::SPOILER_GRAY, ui::WHITE, bs) + 8.0;
                }
                if let (Some(fl), true) = (&it.post.link_flair_text, prefs.on("show_flair")) {
                    bx += common::badge(p, m, bx, y + 2.0, fl, ui::with_alpha(ui::accent(), 0.3), ui::WHITE, bs) + 8.0;
                }
                let ratio = if it.post.upvote_ratio > 0.0 { format!("  \u{b7}  {:.0}% upvoted", it.post.upvote_ratio * 100.0) } else { String::new() };
                let w = common::score_line(p, m, it, bx, y, ms, ui::DIM, prefs.on("show_scores"));
                ui::text_raw(p, &ratio, bx + w, y, ms, ui::FAINT, ui::ALIGN_LEFT, false);
            }
            PostPart::Media => {
                let blur = (it.post.over_18 && prefs.get("nsfw") == "blur") || (it.post.spoiler && prefs.get("spoilers") == "blur");
                let inner = Rect::new(r.x + 6.0, r.y + 6.0, r.w - 12.0, r.h - 12.0);
                ui::fill(p, inner, 12.0, [0.0, 0.0, 0.0, 0.5]);
                let mut drew = false;
                if let Some((url, _, _)) = self.media_pic(it) {
                    let (srv, url) = if blur { (images::BLUR, rd_core::media::preview(&it.post, 140).map_or(url, |p| p.url)) } else { (images::FULL, url) };
                    let fit = if blur { Fit::Cover } else { Fit::Contain };
                    drew = matches!(images::draw(p, srv, &url, inner, 12.0, fit, ui::WHITE), images::Drawn::At(_));
                    if !drew {
                        ui::spinner(p, cx.views.now_ms, inner.x + inner.w / 2.0, inner.y + inner.h / 2.0);
                    }
                }
                let label = match &it.media {
                    Media::Video(v) if v.gif => Some(("GIF  \u{b7}  OK to play", Icon::Play)),
                    Media::Video(_) => Some(("Video  \u{b7}  OK to play", Icon::Play)),
                    Media::YouTube { .. } => Some(("YouTube  \u{b7}  OK to open", Icon::Play)),
                    Media::Gallery(g) if g.len() > 1 => Some(("Gallery  \u{b7}  OK to open", Icon::Film)),
                    Media::Link { .. } => Some(("Link  \u{b7}  OK to open", Icon::Leave)),
                    _ if blur => Some(("OK to show", Icon::Info)),
                    _ => None,
                };
                if let Media::Link { url, .. } = &it.media {
                    if !drew {
                        let s = ui::sz(26);
                        ui::text_cut(p, m, &rd_core::media::host(url), inner.x + 24.0, inner.y + 20.0, inner.w - 48.0, s, ui::WHITE, ui::ALIGN_LEFT, true);
                        ui::text_cut(p, m, url, inner.x + 24.0, inner.y + 20.0 + s as f32 * 1.4, inner.w - 48.0, ui::sz(20), ui::DIM, ui::ALIGN_LEFT, false);
                    }
                }
                if let Some((text, icon)) = label {
                    if drew || !matches!(it.media, Media::Link { .. }) {
                        let s = ui::sz(22);
                        let w = ui::text_width(m, text, s, true) + s as f32 * 3.0;
                        let br = Rect::new(inner.x + (inner.w - w) / 2.0, inner.y + inner.h - s as f32 * 2.4 - 14.0, w, s as f32 * 2.2);
                        ui::fill(p, br, br.h / 2.0, [0.0, 0.0, 0.0, 0.75]);
                        let is = s as f32 * 1.1;
                        ui::icon::draw(p, icon, Rect::new(br.x + s as f32 * 0.8, br.y + (br.h - is) / 2.0, is, is), ui::accent(), true);
                        ui::text_raw(p, text, br.x + s as f32 * 2.2, br.y + (br.h - s as f32 * 1.3) / 2.0, s, ui::WHITE, ui::ALIGN_LEFT, true);
                    }
                }
                if let Media::Gallery(g) = &it.media {
                    if g.len() > 1 {
                        common::badge(p, m, inner.x + 14.0, inner.y + 14.0, &format!("1 / {}", g.len()), common::BADGE_BG, ui::WHITE, ui::sz(18));
                    }
                }
            }
            PostPart::Body => {
                let laid = self.body_laid(it, r.w);
                let reveal = self.revealed.borrow().contains(&format!("post:{}", it.post.id));
                richtext::draw(p, &laid, r.x + PAD, r.y + PAD, self.left.rect, [0.92, 0.92, 0.94, 1.0], reveal);
            }
        }
    }

    fn draw_comment(&self, f: &mut DrawFrame<'_, '_, RdHost>, c: &CItem, r: Rect, focused: bool, cx: &Cx<'_, RdHost>) {
        let p = f.painter;
        let m = f.measure;
        let prefs = &cx.views.store.prefs;
        let ms = ui::sz(21);
        let (depth, x0) = match c {
            CItem::Comment { depth, .. } | CItem::More { depth, .. } => (*depth, r.x + Self::indent(*depth)),
        };
        // Depth rails.
        for d in 0..depth.min(10) {
            let rx = r.x + Self::indent(d) + 6.0;
            ui::fill(p, Rect::new(rx, r.y - 6.0, 3.0, r.h + 12.0), 1.5, ui::with_alpha(RAILS[d as usize % RAILS.len()], 0.45));
        }
        let card = Rect::new(x0, r.y, r.x + r.w - x0, r.h);
        if focused {
            let t = self.right.focus_t(cx.views.now_ms);
            ui::fill(p, card, 14.0, ui::surface(0.05 + 0.09 * t));
            p.rring(card, 14.0, 3.0, ui::with_alpha(ui::accent(), 0.25 + 0.65 * t));
        } else {
            ui::fill(p, card, 14.0, ui::surface(0.05));
        }
        let rail = RAILS[depth as usize % RAILS.len()];
        ui::fill(p, Rect::new(card.x, card.y + 10.0, 5.0, card.h - 20.0), 2.5, rail);
        let x = card.x + 20.0;
        match c {
            CItem::Comment { name, author, body, score, score_hidden, created, op, mod_, likes, collapsed, hidden, flair, stickied, .. } => {
                let y = card.y + 12.0;
                let mut hx = x;
                let author_col = if *mod_ {
                    [0.3, 0.85, 0.45, 1.0]
                } else if *op {
                    [0.45, 0.65, 1.0, 1.0]
                } else {
                    [1.0, 1.0, 1.0, 0.9]
                };
                hx += ui::text_raw(p, author, hx, y, ms, author_col, ui::ALIGN_LEFT, true) + 10.0;
                let bs = (ms as f32 * 0.82) as i32;
                if *op {
                    hx += common::badge(p, m, hx, y + 2.0, "OP", [0.25, 0.45, 0.9, 1.0], ui::WHITE, bs) + 8.0;
                }
                if *mod_ {
                    hx += common::badge(p, m, hx, y + 2.0, "MOD", [0.15, 0.6, 0.3, 1.0], ui::WHITE, bs) + 8.0;
                }
                if *stickied {
                    hx += common::badge(p, m, hx, y + 2.0, "PINNED", [0.15, 0.6, 0.3, 1.0], ui::WHITE, bs) + 8.0;
                }
                if let (Some(fl), true) = (flair, prefs.on("show_flair")) {
                    let fl: String = fl.chars().take(30).collect();
                    hx += common::badge(p, m, hx, y + 2.0, &fl, [1.0, 1.0, 1.0, 0.12], ui::DIM, bs) + 8.0;
                }
                let mut meta = Vec::new();
                if prefs.on("show_scores") {
                    meta.push(if *score_hidden { "\u{2022} points".to_string() } else { format!("{} points", common::short_count(*score)) });
                }
                meta.push(rd_core::listing::age(*created, rd_core::now_secs() as i64));
                if *collapsed && *hidden > 0 {
                    meta.push(format!("+{hidden} hidden"));
                }
                let col = match likes {
                    Some(true) => ui::UPVOTE,
                    Some(false) => ui::DOWNVOTE,
                    None => ui::FAINT,
                };
                ui::text_cut(p, m, &meta.join("  \u{b7}  "), hx + 6.0, y, card.x + card.w - hx - 20.0, ms, col, ui::ALIGN_LEFT, false);
                if !*collapsed {
                    // The same width `comment_h` laid it out at.
                    let laid = richtext::cached(name, body, r.x + r.w - x - 34.0, ui::csz(25));
                    let reveal = self.revealed.borrow().contains(name);
                    let ty = y + ms as f32 * 1.42 + 6.0;
                    richtext::draw(p, &laid, x, ty, self.right.rect, [0.93, 0.93, 0.95, 1.0], reveal);
                    // Pictures linked in the comment, under its text.
                    let mut py = ty + laid.h + 10.0;
                    for url in comment_pictures(body) {
                        let box_ = Rect::new(x, py, (r.x + r.w - x - 34.0).min(COMMENT_PIC_H * 1.8), COMMENT_PIC_H);
                        ui::fill(p, box_, 10.0, [0.0, 0.0, 0.0, 0.35]);
                        if let images::Drawn::Loading = images::draw(p, images::THUMB, &url, box_, 10.0, Fit::Contain, ui::WHITE) {
                            ui::spinner(p, cx.views.now_ms, box_.x + box_.w / 2.0, box_.y + box_.h / 2.0);
                        }
                        py += COMMENT_PIC_H + 12.0;
                    }
                }
            }
            CItem::More { count, cont, id, .. } => {
                let loading = cx.views.store.thread(&self.id).is_some_and(|t| t.loading_more.contains(id));
                let label = if loading {
                    "Loading\u{2026}".to_string()
                } else if *cont {
                    "Continue this thread \u{203a}".to_string()
                } else if *count > 0 {
                    format!("Load {} more {}", count, if *count == 1 { "reply" } else { "replies" })
                } else {
                    "Load more replies".to_string()
                };
                ui::text_raw(p, &label, x, card.y + 12.0, ms, ui::accent(), ui::ALIGN_LEFT, true);
            }
        }
    }
}

impl Screen<RdHost> for PostScreen {
    fn name(&self) -> &'static str {
        "post"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let cx = f.cx;
        self.relayout_left(cx);
        self.relayout_right(cx);
        self.left.animate(cx.views.now_ms);
        self.right.animate(cx.views.now_ms);
        let p = f.painter;
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        // The divider.
        ui::fill(p, Rect::new(split_x(), top_h() + 10.0, 2.0, 1080.0 - top_h() - 40.0), 1.0, [1.0, 1.0, 1.0, 0.07]);
        match self.item(cx) {
            Some(it) => {
                let parts = self.parts(cx);
                let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(0.0, self.left.rect.y - 8.0, split_x(), 1088.0 - self.left.rect.y));
                for i in self.left.visible() {
                    if let (Some(r), Some(part)) = (self.left.screen_rect(i), parts.get(i)) {
                        self.draw_post_part(f, it, *part, r, focus == Some(self.left.elem(i)));
                    }
                }
            }
            None => ui::spinner(p, cx.views.now_ms, split_x() / 2.0, 500.0),
        }
        let thread = cx.views.store.thread(&self.id);
        let rows = self.rows.borrow().clone();
        if rows.is_empty() {
            let area = self.right.rect;
            match thread {
                Some(t) if t.loading => ui::spinner(p, cx.views.now_ms, area.x + area.w / 2.0, 500.0),
                Some(t) if t.error.is_some() => common::message(p, f.measure, area, "Couldn't load the comments", t.error.as_deref().unwrap_or_default()),
                Some(_) => common::message(p, f.measure, area, "No comments yet", ""),
                None => ui::spinner(p, cx.views.now_ms, area.x + area.w / 2.0, 500.0),
            }
        } else {
            let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(split_x() + 4.0, self.right.rect.y - 8.0, 1920.0 - split_x(), 1088.0 - self.right.rect.y));
            for i in self.right.visible() {
                if let (Some(r), Some(c)) = (self.right.screen_rect(i), rows.get(i)) {
                    self.draw_comment(f, c, r, focus == Some(self.right.elem(i)), cx);
                }
            }
            if thread.is_some_and(|t| t.loading) {
                ui::spinner(p, cx.views.now_ms, 1860.0, top_h() + 40.0);
            }
        }
        if self.choice.is_none() {
            self.left.stops(f);
            self.right.stops(f);
        }
        self.right.draw_more_marks(p);
        self.draw_bar(f);
        if let Some(b) = &self.choice {
            ui::fill(f.painter, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.35]);
            b.draw(f);
        }
        common::toast(f.painter, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

#[allow(dead_code)]
fn unused(_: Painter) {}
