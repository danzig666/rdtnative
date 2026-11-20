//! A post and its comments. A bar of buttons on top (vote, save, reply, comment sort, Live, new
//! comments, the subreddit, the author, links, refresh); the post on the left (title, media,
//! text) and the comment tree on the right. Up/Down step through comments (a long one is paged
//! through), Left goes to the parent comment, OK opens or closes a thread, holding OK opens a
//! comment's menu, and the colour buttons vote and save. "Load more" rows fetch in place.
//!
//! A comment link opens that comment's own thread, focused on it ("All comments" swaps in the
//! whole one). Comments written since the last visit are marked new: CH▼ (⏩, N on a keyboard)
//! jumps to the next one, CH▲ (⏪, P) to the one before. Live mode, on by itself for a young
//! match or game thread, sorts by new and polls, merging what arrives without moving what is
//! being read.

use super::common::{self, Color, State};
use crate::flow::Flow;
use crate::host::{MenuKind, RdArg, RdFx, RdHost, RdMsg, Typing};
use crate::i18n::tr;
use crate::images::{self, Fit};
use crate::richtext;
use crate::store::{self, PostItem, ThreadState};
use crate::ui::{self, icon::Kind as Icon, ChoiceBox};
use plx_machine::machine::{By, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::comments::{self, Row as CRow};
use rd_core::listing::{CommentSort, Feed};
use rd_core::media::Media;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

const G_BAR: GroupId = GroupId(1);
const G_POST: GroupId = GroupId(2);
const G_COMMENTS: GroupId = GroupId(3);
const BAR_BASE: u32 = 0x3000_0000;
const POST_BASE: u32 = 0x1000_0000;
const COM_BASE: u32 = 0x2000_0000;

const PAD: f32 = 22.0;

/// How long a comment that came in with a reload stays highlighted.
const ARRIVED_FADE_MS: u32 = 6000;

/// The Live header's dot.
const LIVE_RED: [f32; 4] = [0.95, 0.22, 0.25, 1.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bar {
    Up,
    Down,
    Save,
    Reply,
    /// A comment's thread: the post's whole one instead.
    AllComments,
    Sort,
    Live,
    /// "N new": the next new comment.
    New,
    Sub,
    User,
    Links,
    Refresh,
    /// Back to the start page.
    Home,
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
    Comment {
        name: String,
        author: String,
        body: String,
        score: i64,
        score_hidden: bool,
        created: i64,
        depth: u32,
        op: bool,
        mod_: bool,
        likes: Option<bool>,
        collapsed: bool,
        hidden: usize,
        flair: Option<String>,
        stickied: bool,
        saved: bool,
        /// Written since the last visit, or brought in by a reload.
        new: bool,
        /// Brought in by a reload while the post was open (highlighted a moment).
        arrived: bool,
    },
    More { id: String, depth: u32, count: i64, cont: bool, parent: String },
}

/// Live mode: the comments sorted by new and polled while the page is on screen.
#[derive(Default)]
struct Live {
    on: bool,
    /// Whether the post was looked at yet (a live-looking one starts in Live mode once).
    decided: bool,
    /// The sort before Live, put back when it is turned off.
    sort_before: Option<CommentSort>,
    /// When the next poll is due (ms).
    next_ms: u32,
    /// The second the header's "updated N s ago" was last drawn for.
    shown_secs: u64,
}

pub struct PostScreen {
    entry: EntryId,
    id: String,
    /// The comment this page shows the thread of (a comment link, "Continue this thread");
    /// `None` is the post's whole thread.
    comment: Option<String>,
    /// The thread's key in the store (`store::thread_key`).
    key: String,
    /// Opened for its comments: the first focus goes to the first comment.
    to_comments: bool,
    left: Flow,
    right: Flow,
    sig_left: Cell<(u32, u64)>,
    sig_right: Cell<(u32, u32, u64)>,
    rows: RefCell<Vec<CItem>>,
    bar_rects: RefCell<Vec<Rect>>,
    /// The bar as last drawn: focus stays on its button when buttons come or go beside it.
    bar_last: RefCell<Vec<Bar>>,
    choice: Option<ChoiceBox>,
    focus_req: Cell<Option<u32>>,
    /// Comments whose spoilers were revealed.
    revealed: RefCell<HashSet<String>>,
    initial_focus_done: Cell<bool>,
    /// The comments got their first focus (the linked comment, or the first one), or the user
    /// moved before they arrived.
    landed: Cell<bool>,
    /// The last visit (Unix seconds): comments written after it are new.
    since: Option<i64>,
    /// When comments that came in with a reload were first drawn (their highlight fades).
    arrived_at: RefCell<HashMap<String, u32>>,
    /// Comments that came in with reloads before this page opened (seen on an earlier visit).
    arrived_before: HashSet<String>,
    live: Live,
    state: State,
}

fn top_h() -> f32 {
    ui::ls(110.0).max(100.0)
}

fn split_x() -> f32 {
    760.0
}

/// The Live header's band over the comments.
fn live_h() -> f32 {
    ui::sz(22) as f32 * 1.42 + 20.0
}

/// The comment column: under the Live header while it shows.
fn comments_rect(live: bool) -> Rect {
    let top = top_h() + if live { live_h() } else { 0.0 };
    // The key hints' band is under the comments.
    Rect::new(split_x() + 20.0, top, 1920.0 - split_x() - 20.0 - 60.0, 1080.0 - top - common::hint_band())
}

/// The row after `at` (before it when not `forward`) whose flag is set, round the ends; from the
/// first (last) row when nothing is focused.
fn next_flagged(flags: &[bool], at: Option<usize>, forward: bool) -> Option<usize> {
    let n = flags.len();
    if n == 0 {
        return None;
    }
    let start = at.unwrap_or(if forward { n - 1 } else { 0 }).min(n - 1);
    (1..=n).map(|k| if forward { (start + k) % n } else { (start + n - k % n) % n }).find(|&j| flags[j])
}

/// Widths for a row of buttons that must fit in `room` (gaps left out): the widest give way
/// first, down to `min` (their labels are cut).
fn squeeze(widths: &[f32], room: f32, min: f32) -> Vec<f32> {
    let over = widths.iter().sum::<f32>() - room;
    if over <= 0.0 {
        return widths.to_vec();
    }
    // The cap at which trimming every wider button to it frees `over`.
    let freed = |cap: f32| widths.iter().map(|w| (w - cap).max(0.0)).sum::<f32>();
    let (mut lo, mut hi) = (min, widths.iter().cloned().fold(min, f32::max));
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if freed(mid) >= over {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    widths.iter().map(|w| w.min(lo)).collect()
}

/// A comment's layout cache key: its name and text (a reload may bring an edit).
fn laid_key(name: &str, body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    format!("{name}:{:x}", h.finish())
}

/// CH▼ / ⏩ (N on a keyboard): the next new comment (`true`); CH▲ / ⏪ (P): the one before.
fn new_comment_key(ev: &ScreenEvent<RdHost>) -> Option<bool> {
    use plx_ui::consts::{WCODE_CH_DOWN_KEY, WCODE_CH_UP_KEY, WCODE_FASTFORWARD, WCODE_REWIND};
    let ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Other, sym, wcode, edge: Edge::Down | Edge::Repeat, .. }, .. }) = ev else {
        return None;
    };
    match (*wcode, *sym) {
        (WCODE_CH_DOWN_KEY | WCODE_FASTFORWARD, _) | (_, 0x6e) => Some(true),
        (WCODE_CH_UP_KEY | WCODE_REWIND, _) | (_, 0x70) => Some(false),
        _ => None,
    }
}

impl PostScreen {
    pub fn new(entry: EntryId, id: String, comment: Option<String>) -> Self {
        let top = top_h();
        // The key hints' band is under the columns.
        let h = 1080.0 - top - common::hint_band();
        let mut left = Flow::new(entry, G_POST, POST_BASE, Rect::new(60.0, top, split_x() - 60.0 - 30.0, h));
        left.row_gap = 20.0;
        left.pad_top = 6.0;
        left.margin = 30.0;
        let mut right = Flow::new(entry, G_COMMENTS, COM_BASE, comments_rect(false));
        right.row_gap = 10.0;
        right.pad_top = 6.0;
        right.margin = 40.0;
        let key = store::thread_key(&id, comment.as_deref());
        PostScreen {
            entry,
            state: State(format!("post {key}")),
            id,
            comment,
            key,
            to_comments: false,
            left,
            right,
            sig_left: Cell::new((u32::MAX, 0)),
            sig_right: Cell::new((u32::MAX, 0, 0)),
            rows: Default::default(),
            bar_rects: Default::default(),
            bar_last: Default::default(),
            choice: None,
            focus_req: Cell::new(None),
            revealed: Default::default(),
            initial_focus_done: Cell::new(false),
            landed: Cell::new(false),
            since: None,
            arrived_at: Default::default(),
            arrived_before: HashSet::new(),
            live: Live::default(),
        }
    }

    /// Opens on the first comment rather than the post (Blue on a feed post, "Open comments").
    pub fn to_comments(mut self) -> Self {
        self.to_comments = true;
        self
    }

    fn item<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a PostItem> {
        cx.views.store.post(&self.id)
    }

    fn thread<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a ThreadState> {
        cx.views.store.thread(&self.key)
    }

    /// The linked comment's name.
    fn target(&self) -> Option<String> {
        self.comment.as_ref().map(|c| format!("t1_{c}"))
    }

    fn default_sort(cx: &Cx<'_, RdHost>) -> CommentSort {
        CommentSort::from_key(cx.views.store.prefs.get("comment_sort")).unwrap_or(CommentSort::Confidence)
    }

    fn sort(&self, cx: &Cx<'_, RdHost>) -> CommentSort {
        self.thread(cx).map_or_else(|| Self::default_sort(cx), |t| t.sort)
    }

    fn new_count(&self) -> usize {
        self.rows.borrow().iter().filter(|r| matches!(r, CItem::Comment { new: true, .. })).count()
    }

    /// The bar's buttons as things stand.
    fn bar(&self, cx: &Cx<'_, RdHost>) -> Vec<Bar> {
        let mut v = vec![Bar::Up, Bar::Down, Bar::Save];
        if cx.views.store.signed_in() {
            v.push(Bar::Reply);
        }
        if self.comment.is_some() {
            v.push(Bar::AllComments);
        }
        v.push(Bar::Sort);
        if self.comment.is_none() {
            v.push(Bar::Live);
        }
        if self.new_count() > 0 {
            v.push(Bar::New);
        }
        v.extend([Bar::Sub, Bar::User, Bar::Links, Bar::Refresh, Bar::Home]);
        v
    }

    /// Button `b`'s element (the first button's when it is not on the bar).
    fn bar_at(&self, b: Bar, cx: &Cx<'_, RdHost>) -> u32 {
        BAR_BASE + self.bar(cx).iter().position(|x| *x == b).unwrap_or(0) as u32
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
        let Some(t) = self.thread(cx) else { return vec![] };
        let op = self.item(cx).map(|i| i.post.author.clone()).unwrap_or_default();
        let me = cx.views.store.username().unwrap_or_default();
        comments::flatten(&t.tree, &t.toggled)
            .into_iter()
            .map(|r| match r {
                CRow::Comment { c, depth, collapsed, hidden_replies } => {
                    let arrived = t.arrived.contains(&c.name) && !self.arrived_before.contains(&c.name);
                    // The user's own comments are never news to them.
                    let mine = !me.is_empty() && c.author.eq_ignore_ascii_case(me);
                    CItem::Comment {
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
                        saved: c.saved,
                        new: !mine && (arrived || self.since.is_some_and(|s| c.created_utc > s)),
                        arrived,
                    }
                }
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
                let laid = richtext::cached(&laid_key(name, body), body, w - x - 34.0, ui::csz(25));
                let pics = comment_pictures(body).len() as f32;
                12.0 + head + 6.0 + laid.h + pics * (COMMENT_PIC_H + 12.0) + 14.0
            }
            CItem::More { .. } => ms as f32 * 1.42 + 24.0,
        }
    }

    fn relayout_right(&self, cx: &Cx<'_, RdHost>) {
        let gen = self.thread(cx).map_or(0, |t| t.gen);
        let sig = (gen, cx.views.store.gen, Self::prefs_sig());
        if sig == self.sig_right.get() {
            return;
        }
        self.sig_right.set(sig);
        // The comment being read keeps its place on screen while comments come in above it
        // (unless focus is already on its way somewhere).
        let anchor = cx
            .focus
            .current
            .filter(|k| k.entry == self.entry && self.focus_req.get().is_none())
            .and_then(|k| self.right.index(k.elem))
            .and_then(|i| match self.rows.borrow().get(i) {
                Some(CItem::Comment { name, .. }) => Some((name.clone(), self.right.top(i)?)),
                _ => None,
            });
        let rows = self.comment_rows(cx);
        let w = self.right.col_w();
        let heights: Vec<f32> = rows.iter().map(|c| self.comment_h(c, w)).collect();
        self.right.set_heights(&heights);
        if let Some((name, was)) = anchor {
            if let Some(j) = rows.iter().position(|r| matches!(r, CItem::Comment { name: n, .. } if *n == name)) {
                self.right.shift(self.right.top(j).unwrap_or(was) - was);
                self.focus_req.set(Some(self.right.elem(j)));
            }
        }
        *self.rows.borrow_mut() = rows;
    }

    fn bar_label(&self, b: Bar, cx: &Cx<'_, RdHost>) -> (String, Icon, bool) {
        let it = self.item(cx);
        let sort = self.sort(cx);
        match b {
            Bar::Up => (it.map_or(String::new(), |i| common::short_count(i.post.score)), Icon::ThumbUp, it.is_some_and(|i| i.post.likes == Some(true))),
            Bar::Down => (String::new(), Icon::ThumbDown, it.is_some_and(|i| i.post.likes == Some(false))),
            Bar::Save => (if it.is_some_and(|i| i.post.saved) { "Saved" } else { "Save" }.into(), Icon::Star, it.is_some_and(|i| i.post.saved)),
            Bar::Reply => ("Reply".into(), Icon::Chat, false),
            Bar::AllComments => (tr("All comments").into(), Icon::Chat, false),
            Bar::Sort => (sort.label().into(), Icon::Trending, false),
            Bar::Live => (tr("Live").into(), Icon::Live, self.live.on),
            Bar::New => (tr("{} new").replace("{}", &self.new_count().to_string()), Icon::Bell, true),
            Bar::Sub => (it.map_or(String::new(), |i| format!("r/{}", i.post.subreddit)), Icon::Subs, false),
            Bar::User => (it.map_or(String::new(), |i| format!("u/{}", i.post.author)), Icon::User, false),
            Bar::Links => ("Links".into(), Icon::Leave, false),
            Bar::Refresh => (String::new(), Icon::Repeat, false),
            Bar::Home => (String::new(), Icon::Home, false),
        }
    }

    fn activate_bar(&mut self, i: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if let Some(b) = self.bar(cx).get(i).copied() {
            self.press_bar(b, i, cx, fx);
        }
    }

    /// Bar button `b` (at `i` on the bar) was pressed.
    fn press_bar(&mut self, b: Bar, i: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        match b {
            Bar::AllComments => fx.push(Fx::Nav(NavOp::Replace(RdArg::Post { id: self.id.clone(), comment: None }))),
            Bar::Sort => self.open_sort(i, cx),
            Bar::Live => self.set_live(!self.live.on, cx, fx),
            Bar::New => self.jump_new(true, cx, fx),
            Bar::Links => fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Links(self.id.clone(), String::new()))))),
            Bar::Refresh => fx.push(Fx::App(RdFx::LoadThread { id: self.key.clone(), sort: None, reload: true })),
            // The start page as it was left, under the pages opened since.
            Bar::Home => fx.push(Fx::Nav(NavOp::PopToArg(RdArg::Home))),
            Bar::Up | Bar::Down | Bar::Save | Bar::Reply | Bar::Sub | Bar::User => {
                let Some(it) = self.item(cx) else { return };
                let name = it.post.name.clone();
                fx.push(match b {
                    Bar::Up => Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(true) { 0 } else { 1 } }),
                    Bar::Down => Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(false) { 0 } else { -1 } }),
                    Bar::Save => Fx::App(RdFx::Save { fullname: name, on: !it.post.saved }),
                    Bar::Reply => Fx::Nav(NavOp::Push(RdArg::Type(Typing::Reply { parent: name, post: self.id.clone() }))),
                    Bar::Sub => Fx::Nav(NavOp::Push(RdArg::Feed(Feed::Sub(it.post.subreddit.clone())))),
                    _ => Fx::Nav(NavOp::Push(RdArg::Profile(it.post.author.clone()))),
                });
            }
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// The comment sort drop-down. A sort picked there is this post's only; "Use as default"
    /// makes the current one every post's.
    fn open_sort(&mut self, i: usize, cx: &Cx<'_, RdHost>) {
        let anchor = self.bar_rects.borrow().get(i).copied().unwrap_or(Rect::new(900.0, 30.0, 200.0, 60.0));
        let cur = self.sort(cx);
        let mut options: Vec<String> = CommentSort::ALL.iter().map(|s| s.label().to_string()).collect();
        if cur != Self::default_sort(cx) {
            options.push("Use as default".into());
        }
        let b = ChoiceBox::new(self.entry, i, options, CommentSort::ALL.iter().position(|s| *s == cur).unwrap_or(0), anchor);
        self.focus_req.set(Some(b.list.elem(b.current)));
        ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.choice = Some(b);
    }

    /// Option `i` of the sort drop-down was picked.
    fn pick_sort(&mut self, i: usize, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        match CommentSort::ALL.get(i).copied() {
            Some(s) => {
                // Another sort ends Live mode (which sorts by new).
                if s != CommentSort::New && self.live.on {
                    self.live.sort_before = None;
                    self.show_live(false);
                }
                fx.push(Fx::App(RdFx::LoadThread { id: self.key.clone(), sort: Some(s), reload: true }));
                self.right.to_top();
            }
            None => {
                let cur = self.sort(cx);
                fx.push(Fx::App(RdFx::EditPrefs(|p, v| p.set("comment_sort", v), cur.key().to_string())));
                fx.push(Fx::App(RdFx::Toast(tr("Posts now open with comments sorted by {}").replace("{}", tr(cur.label())))));
            }
        }
    }

    /// Shows or hides the Live header (the comment column moves under it).
    fn show_live(&mut self, on: bool) {
        self.live.on = on;
        self.right.rect = comments_rect(on);
        self.sig_right.set((u32::MAX, 0, 0));
    }

    /// Turns Live mode on (the comments sorted by new, polled) or off (the sort before it back).
    fn set_live(&mut self, on: bool, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if on == self.live.on {
            return;
        }
        let cur = self.sort(cx);
        self.show_live(on);
        if on {
            self.live.sort_before = Some(cur);
            self.live.next_ms = cx.views.now_ms.wrapping_add(Self::live_period(cx));
            fx.push(Fx::App(RdFx::LoadThread { id: self.key.clone(), sort: Some(CommentSort::New), reload: cur != CommentSort::New }));
            if cur != CommentSort::New {
                self.right.to_top();
            }
        } else if let Some(s) = self.live.sort_before.take().filter(|s| *s != CommentSort::New) {
            fx.push(Fx::App(RdFx::LoadThread { id: self.key.clone(), sort: Some(s), reload: true }));
            self.right.to_top();
        }
    }

    fn live_period(cx: &Cx<'_, RdHost>) -> u32 {
        cx.views.store.prefs.num("live_refresh").clamp(15, 60) as u32 * 1000
    }

    /// A whole thread that reads like a match or game thread and is young starts in Live mode.
    fn looks_live(&self, it: &PostItem) -> bool {
        self.comment.is_none() && comments::is_live_thread(&it.post.title, it.post.link_flair_text.as_deref(), it.post.created_utc, rd_core::now_secs() as i64)
    }

    /// Live mode's clock, on every tick: the first look at the post, the polls, the header.
    fn tick_live(&mut self, now_ms: u32, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if !self.live.decided {
            let Some(it) = self.item(cx) else { return };
            self.live.decided = true;
            if self.looks_live(it) {
                self.set_live(true, cx, fx);
            }
        }
        if !self.live.on {
            return;
        }
        // A sort asked for while a load was on its way did not happen: ask again.
        if self.thread(cx).is_some_and(|t| !t.loading && t.sort != CommentSort::New) {
            fx.push(Fx::App(RdFx::LoadThread { id: self.key.clone(), sort: Some(CommentSort::New), reload: true }));
        }
        // No polls while a menu covers the page or the app is in the background (covered by
        // another page, it gets no ticks at all).
        let paused = !crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed) || ui::menu_open() || self.choice.is_some();
        if !paused && now_ms.wrapping_sub(self.live.next_ms) < u32::MAX / 2 {
            self.live.next_ms = now_ms.wrapping_add(Self::live_period(cx));
            fx.push(Fx::App(RdFx::RefreshThread { id: self.key.clone() }));
        }
        // "updated N s ago" moves once a second.
        let secs = rd_core::now_secs();
        if secs != self.live.shown_secs {
            self.live.shown_secs = secs;
            fx.invalidate(plx_machine::present::Provenance::Lifecycle);
        }
    }

    /// Focus the next new comment (or the one before), round the ends.
    fn jump_new(&self, forward: bool, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let flags: Vec<bool> = self.rows.borrow().iter().map(|r| matches!(r, CItem::Comment { new: true, .. })).collect();
        let at = cx.focus.current.filter(|k| k.entry == self.entry).and_then(|k| self.right.index(k.elem));
        match next_flagged(&flags, at, forward) {
            Some(j) => {
                self.right.reveal(j);
                self.focus_req.set(Some(self.right.elem(j)));
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            None => fx.push(Fx::App(RdFx::Toast("No new comments".into()))),
        }
    }

    fn save_comment(&self, i: usize, fx: &mut Effects<'_, RdHost>) {
        if let Some(CItem::Comment { name, saved, .. }) = self.comment_at(i) {
            fx.push(Fx::App(RdFx::Save { fullname: name, on: !saved }));
        }
    }

    /// How strong a comment that came in with a reload is highlighted (fading from its first draw).
    fn arrived_glow(&self, name: &str, now_ms: u32) -> f32 {
        let at = *self.arrived_at.borrow_mut().entry(name.to_string()).or_insert(now_ms);
        let t = now_ms.wrapping_sub(at);
        if t >= ARRIVED_FADE_MS {
            return 0.0;
        }
        ui::animating();
        1.0 - t as f32 / ARRIVED_FADE_MS as f32
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
            // A GIF's still from Reddit is its .gif address asked for as a PNG or JPEG.
            let still = path.ends_with(".gif") && (u.contains("format=png") || u.contains("format=pjpg") || u.contains("format=jpg"));
            let pic = still || [".jpg", ".jpeg", ".png", ".webp"].iter().any(|e| path.ends_with(e));
            pic && matches!(host.as_str(), "i.redd.it" | "preview.redd.it" | "external-preview.redd.it" | "i.imgur.com")
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
        RedditLink::User(u) => RdArg::Profile(u),
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
                    fx.push(Fx::Nav(NavOp::Present(RdArg::Menu(MenuKind::Comment(self.key.clone(), name)))));
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
            if !cx.views.store.prefs.on("color_keys") || common::sign_in_first(c, cx, fx) {
                return Handled::Yes;
            }
            let focus = cx.focus.current.map(|k| k.elem);
            let on_comment = focus.and_then(|e| self.right.index(e));
            match (c, on_comment) {
                (Color::Green, Some(i)) => self.vote_comment(i, 1, fx),
                (Color::Red, Some(i)) => self.vote_comment(i, -1, fx),
                (Color::Yellow, Some(i)) => self.save_comment(i, fx),
                (Color::Green, None) => self.press_bar(Bar::Up, 0, cx, fx),
                (Color::Red, None) => self.press_bar(Bar::Down, 1, cx, fx),
                (Color::Yellow, None) => self.press_bar(Bar::Save, 2, cx, fx),
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
        if self.choice.is_none() {
            if let Some(forward) = new_comment_key(ev) {
                self.jump_new(forward, cx, fx);
                return Handled::Yes;
            }
        }
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::App(RdFx::MarkRead(self.id.clone())));
                self.since = cx.views.store.seen.get(&self.id).map(|v| v.at);
                self.arrived_before = self.thread(cx).map(|t| t.arrived.clone()).unwrap_or_default();
                let live = self.item(cx).map(|it| self.looks_live(it));
                self.live.decided = live.is_some();
                if live == Some(true) {
                    self.set_live(true, cx, fx);
                } else {
                    fx.push(Fx::App(RdFx::LoadThread { id: self.key.clone(), sort: None, reload: false }));
                }
                if let Some(it) = self.item(cx) {
                    if matches!(it.media, Media::Imgur(_)) {
                        fx.push(Fx::App(RdFx::ResolveImgur(self.id.clone())));
                    }
                }
            }
            ScreenEvent::Unmount => {
                // The visit, for the next one's new comments (a comment's thread is not the
                // whole post).
                if let (None, Some(t), Some(it)) = (&self.comment, self.thread(cx), self.item(cx)) {
                    if t.fetched_at > 0 && !t.partial {
                        fx.push(Fx::App(RdFx::MarkSeen { id: self.id.clone(), at: t.fetched_at as i64, comments: it.post.num_comments }));
                    }
                }
            }
            ScreenEvent::Tick(t) => self.tick_live(t.ms, cx, fx),
            ScreenEvent::Async(_, RdMsg::Changed) | ScreenEvent::Uncover => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::FocusMoved { to, by, .. } => {
                // The user moved first: the comments, when they come, do not take the focus.
                if *by == By::Dir {
                    self.landed.set(true);
                }
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
                            self.pick_sort(i, cx, fx);
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
                                fx.push(Fx::App(RdFx::ToggleComment { post: self.key.clone(), name }));
                            }
                        }
                        Some(CItem::More { id, cont, parent, .. }) => {
                            if cont {
                                // "Continue this thread": the parent comment's own thread, on a
                                // page of its own (never this page again).
                                let c = parent.trim_start_matches("t1_").to_string();
                                if self.comment.as_deref() != Some(c.as_str()) {
                                    fx.push(Fx::Nav(NavOp::Push(RdArg::Post { id: self.id.clone(), comment: Some(c) })));
                                }
                            } else {
                                fx.push(Fx::App(RdFx::MoreComments { post: self.key.clone(), more_id: id }));
                            }
                        }
                        None => {}
                    }
                } else if let Some(i) = self.left.index(e) {
                    match self.parts(cx).get(i) {
                        Some(PostPart::Media) => self.open_media(cx, fx),
                        Some(PostPart::Body) => {
                            // OK reveals hidden spoilers first, if there are any; else the links.
                            let k = format!("post:{}", self.id);
                            let spoilers = self.item(cx).is_some_and(|it| it.post.selftext.contains(">!"));
                            if !spoilers || !self.revealed.borrow_mut().insert(k) {
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
            len: self.bar(cx).len(),
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
            let bar = self.bar(cx);
            let n = bar.len() as u32;
            // The post's own buttons lead down to the post, the others to the comments.
            let to_post = matches!(bar.get(i as usize), Some(Bar::Up | Bar::Down | Bar::Save));
            return match dir {
                Dir::Left if i > 0 => k(key.elem - 1),
                Dir::Right if i + 1 < n => k(key.elem + 1),
                Dir::Down if self.right.len() > 0 && !to_post => {
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
                // Up from the top comment: the comment sort, signed in or not.
                Dir::Up => match self.right.neighbour(key, dir) {
                    Step::Edge => k(self.bar_at(Bar::Sort, cx)),
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
        let mut want = match self.focus_req.take() {
            Some(e) => FocusKey { entry: self.entry, elem: e },
            None => want,
        };
        if let Some(b) = &self.choice {
            return if b.owns(want.elem) { want } else { b.list.key(b.current) };
        }
        let bar = self.bar(cx);
        // A bar button keeps the focus when buttons come or go beside it ("N new").
        if want.elem >= BAR_BASE && want.elem < ui::CHOICE_BASE {
            let was = self.bar_last.borrow().get((want.elem - BAR_BASE) as usize).copied();
            if let Some(j) = was.and_then(|b| bar.iter().position(|x| *x == b)) {
                want.elem = BAR_BASE + j as u32;
            }
        }
        // The comments' first focus: the linked comment, or the first one for a post opened
        // for its comments (once, and not after the user moved).
        if !self.landed.get() && self.right.len() > 0 && (self.comment.is_some() || self.to_comments) {
            self.landed.set(true);
            let target = self.target();
            let at = match &target {
                Some(t) => self.rows.borrow().iter().position(|r| matches!(r, CItem::Comment { name, .. } if name == t)),
                None => Some(0),
            };
            if let Some(i) = at {
                self.initial_focus_done.set(true);
                self.right.reveal(i);
                return self.right.key(i);
            }
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
            ((want.elem - BAR_BASE) as usize) < bar.len()
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
        // No post to show (yet, or it failed): Refresh, where OK tries again.
        FocusKey { entry: self.entry, elem: self.bar_at(Bar::Refresh, cx) }
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
        let items = self.bar(cx);
        let labels: Vec<(String, Icon, bool)> = items.iter().map(|b| self.bar_label(*b, cx)).collect();
        let bh = ui::ls(58.0).max(52.0);
        let (x0, gap) = (60.0, 12.0);
        let mut rects = common::button_row(f.measure, &labels.iter().map(|(l, _, _)| (l.as_str(), true)).collect::<Vec<_>>(), x0, 30.0, bh, gap, None);
        // Too many for one row (long names, extra buttons): the widest give way.
        let room = 1920.0 - x0 - 40.0 - gap * rects.len().saturating_sub(1) as f32;
        let widths = squeeze(&rects.iter().map(|r| r.w).collect::<Vec<_>>(), room, bh * 2.4);
        let mut x = x0;
        for (r, w) in rects.iter_mut().zip(widths) {
            *r = Rect::new(x, r.y, w, r.h);
            x += w + gap;
        }
        *self.bar_last.borrow_mut() = items;
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
                        common::badge(p, m, inner.x + 14.0, inner.y + 14.0, &format!("1 / {}", g.len()), common::BADGE_BG, ui::WHITE, ui::sz(20));
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

    /// The Live header over the comments: "● Live · updated 5 s ago · 3 new".
    fn draw_live(&self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let (p, m, cx) = (f.painter, f.measure, f.cx);
        let band = Rect::new(self.right.rect.x, top_h(), self.right.rect.w, live_h());
        let bg = ui::bg_rgb();
        ui::fill(p, band, 0.0, [bg.0, bg.1, bg.2, 1.0]);
        let s = ui::sz(22);
        let y = band.y + (band.h - s as f32 * 1.3) / 2.0;
        let dot = s as f32 * 0.6;
        ui::fill(p, Rect::new(band.x + 6.0, y + (s as f32 * 1.3 - dot) / 2.0, dot, dot), dot / 2.0, LIVE_RED);
        let mut x = band.x + 6.0 + dot + 12.0;
        x += ui::text(p, "Live", x, y, s, ui::WHITE, ui::ALIGN_LEFT, true);
        let t = self.thread(cx);
        let mut parts = vec![match t {
            Some(t) if t.loading || t.fetched_at == 0 => tr("updating\u{2026}").to_string(),
            Some(t) if t.error.is_some() => tr("couldn't update").to_string(),
            Some(t) => {
                let ago = rd_core::now_secs().saturating_sub(t.fetched_at);
                if ago < 60 {
                    tr("updated {} s ago").replace("{}", &ago.to_string())
                } else {
                    tr("updated {} min ago").replace("{}", &(ago / 60).to_string())
                }
            }
            None => tr("updating\u{2026}").to_string(),
        }];
        let n = self.new_count();
        if n > 0 {
            parts.push(tr("{} new").replace("{}", &n.to_string()));
        }
        let rest = format!("  \u{b7}  {}", parts.join("  \u{b7}  "));
        ui::text_cut(p, m, &rest, x, y, band.x + band.w - x, s, ui::DIM, ui::ALIGN_LEFT, false);
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
        if let CItem::Comment { name, arrived, .. } = c {
            // Just came in with a reload: a glow that fades.
            let glow = if *arrived { self.arrived_glow(name, cx.views.now_ms) } else { 0.0 };
            if glow > 0.0 {
                ui::fill(p, card, 14.0, ui::with_alpha(ui::accent(), 0.18 * glow));
            }
            // The comment a link pointed at stays marked.
            if !focused && self.target().as_ref() == Some(name) {
                ui::fill(p, card, 14.0, ui::with_alpha(ui::accent(), 0.08));
                p.rring(card, 14.0, 2.0, ui::with_alpha(ui::accent(), 0.5));
            }
        }
        let rail = RAILS[depth as usize % RAILS.len()];
        ui::fill(p, Rect::new(card.x, card.y + 10.0, 5.0, card.h - 20.0), 2.5, rail);
        let x = card.x + 20.0;
        match c {
            CItem::Comment { name, author, body, score, score_hidden, created, op, mod_, likes, collapsed, hidden, flair, stickied, saved, new, .. } => {
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
                let bs = (ms as f32 * 0.9) as i32;
                if *new {
                    hx += common::badge(p, m, hx, y + 2.0, tr("NEW"), ui::accent(), ui::WHITE, bs) + 8.0;
                }
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
                if *saved {
                    let s = ms as f32 * 1.05;
                    ui::icon::draw(p, Icon::Star, Rect::new(hx, y + ms as f32 * 0.12, s, s), common::SAVED_STAR, true);
                    hx += s + 8.0;
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
                    let laid = richtext::cached(&laid_key(name, body), body, r.x + r.w - x - 34.0, ui::csz(25));
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
                let loading = self.thread(cx).is_some_and(|t| t.loading_more.contains(id));
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
            // The post never came: say why rather than spin (Refresh tries again).
            None => match self.thread(cx).filter(|t| !t.loading).and_then(|t| t.error.as_deref()) {
                Some(e) => common::message(p, f.measure, self.left.rect, "Couldn't open this post", e),
                None => ui::spinner(p, cx.views.now_ms, split_x() / 2.0, 500.0),
            },
        }
        let thread = self.thread(cx);
        let rows = self.rows.borrow().clone();
        if rows.is_empty() {
            let area = self.right.rect;
            match thread {
                Some(t) if t.loading => ui::spinner(p, cx.views.now_ms, area.x + area.w / 2.0, 500.0),
                Some(t) if t.error.is_some() && self.item(cx).is_some() => {
                    common::message(p, f.measure, area, "Couldn't load the comments", t.error.as_deref().unwrap_or_default())
                }
                Some(t) if t.error.is_some() => {}
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
            // (Live mode says it in its header.)
            if thread.is_some_and(|t| t.loading) && !self.live.on {
                ui::spinner(p, cx.views.now_ms, 1860.0, top_h() + 40.0);
            }
        }
        if self.choice.is_none() {
            self.left.stops(f);
            self.right.stops(f);
        }
        self.right.draw_more_marks(p);
        if self.live.on {
            self.draw_live(f);
        }
        // With new comments, CH▲ / CH▼ jump between them.
        let new = self.new_count() > 0;
        let hints = |base: &[common::Hint]| if new { common::with_hint(base, common::NEW_COMMENTS_HINT) } else { base.to_vec() };
        match focus {
            Some(e) if self.right.index(e).is_some() => common::key_hints(f.painter, f.measure, cx, 1080.0, &hints(common::COMMENT_HINTS)),
            Some(e) if self.left.index(e).is_some() => common::key_hints(f.painter, f.measure, cx, 1080.0, &hints(common::POST_HINTS)),
            _ => {}
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_comment_jumps_go_round_the_ends() {
        let f = [false, true, false, true, false];
        assert_eq!(next_flagged(&f, None, true), Some(1));
        assert_eq!(next_flagged(&f, Some(1), true), Some(3));
        assert_eq!(next_flagged(&f, Some(3), true), Some(1));
        assert_eq!(next_flagged(&f, None, false), Some(3));
        assert_eq!(next_flagged(&f, Some(3), false), Some(1));
        assert_eq!(next_flagged(&f, Some(1), false), Some(3));
        assert_eq!(next_flagged(&[false; 3], Some(1), true), None);
        assert_eq!(next_flagged(&[], None, true), None);
    }

    #[test]
    fn a_crowded_bar_narrows_its_widest_buttons() {
        assert_eq!(squeeze(&[100.0, 200.0], 400.0, 120.0), vec![100.0, 200.0]);
        let w = squeeze(&[100.0, 400.0, 300.0, 150.0], 800.0, 120.0);
        assert!((w.iter().sum::<f32>() - 800.0).abs() < 0.5);
        assert_eq!((w[0], w[3]), (100.0, 150.0));
        assert!((w[1] - 275.0).abs() < 0.5 && (w[2] - 275.0).abs() < 0.5);
        // Never below the minimum, even when the row still does not fit.
        assert!(squeeze(&[300.0, 300.0], 100.0, 120.0).iter().all(|w| (*w - 120.0).abs() < 0.5));
    }
}
