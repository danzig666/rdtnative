//! Full-screen media. Pictures fill the screen; OK zooms in (and again, further), the arrows
//! then pan and BACK zooms out; Left/Right step through a gallery with its counter, caption and a
//! strip of thumbnails; Up/Down show or hide the caption; Blue opens the post's comments. Videos,
//! GIFs and YouTube links show their poster with a QR code to watch them on a phone, and a post
//! with nothing the TV can show a QR code of the post. `LinkScreen` shows any other web link as a
//! QR code; OK opens it in the TV's browser.

use super::common::{self, Color, State};
use crate::host::{RdArg, RdFx, RdHost, RdMsg};
use crate::i18n::tr;
use crate::images::{self, Fit};
use crate::store::PostItem;
use crate::ui;
use plx_machine::machine::{Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::qr::QrCode;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, ScreenArg, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::media::{Item, Media, Pic};
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const PREV: u32 = 1;
const CENTER: u32 = 2;
const NEXT: u32 = 3;

/// One page of the viewer.
#[derive(Clone, Debug)]
enum Page {
    Pic(Pic, Option<String>),
    /// A clip: its poster, the stream the TV plays (`None`: not playable here) and the link a
    /// phone opens.
    Clip { poster: Option<Pic>, play: Option<String>, url: String, caption: Option<String>, gif: bool },
    Link(String, Option<Pic>),
    Nothing,
}

fn pages(it: &PostItem, stream: &str) -> Vec<Page> {
    let permalink = format!("https://www.reddit.com{}", it.post.permalink);
    match &it.media {
        Media::Image(p) => vec![Page::Pic(p.clone(), None)],
        Media::Gallery(items) => items
            .iter()
            .map(|Item { pic, mp4, caption, .. }| match mp4 {
                Some(u) => Page::Clip { poster: Some(pic.clone()), play: Some(u.clone()), url: u.clone(), caption: caption.clone(), gif: true },
                None => Page::Pic(pic.clone(), caption.clone()),
            })
            .collect(),
        Media::Video(v) => vec![Page::Clip {
            poster: v.poster.clone(),
            play: crate::player::pick_url(v, stream),
            url: if it.post.url.contains("v.redd.it") || v.hls.is_some() { permalink } else { v.mp4.clone().unwrap_or(permalink) },
            caption: None,
            gif: v.gif,
        }],
        Media::YouTube { url, id } => vec![Page::Clip { poster: Some(Pic { url: format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"), w: 480, h: 360 }), play: None, url: url.clone(), caption: None, gif: false }],
        Media::Link { url, preview } => vec![Page::Link(url.clone(), preview.clone())],
        Media::Imgur(_) => vec![Page::Nothing],
        Media::None => rd_core::media::preview(&it.post, 1920).map(|p| vec![Page::Pic(p, None)]).unwrap_or(vec![Page::Nothing]),
    }
}

pub struct ViewerScreen {
    entry: EntryId,
    id: String,
    index: usize,
    zoom: f32,
    pan: (f32, f32),
    info: bool,
    revealed: bool,
    qr: Option<(String, QrCode)>,
    /// The page under the viewer is this post's page: Blue goes back to it instead of opening it again.
    over_post: bool,
    /// BACK went down to zoom out: its up edge is ours too.
    back_taken: bool,
    state: State,
}

const QR_FRAME: Rect = Rect::new(1380.0, 300.0, 400.0, 400.0);

impl ViewerScreen {
    pub fn new(entry: EntryId, id: String, index: usize) -> Self {
        ViewerScreen {
            entry,
            state: State(format!("viewer {id}")),
            id,
            index,
            zoom: 1.0,
            pan: (0.0, 0.0),
            info: true,
            revealed: false,
            qr: None,
            over_post: false,
            back_taken: false,
        }
    }

    /// The post's comments: back to its page, or its page in place of the viewer.
    fn comments(&self, fx: &mut Effects<'_, RdHost>) {
        let op = if self.over_post { NavOp::Pop } else { NavOp::Replace(RdArg::PostComments { id: self.id.clone() }) };
        fx.push(Fx::Nav(op));
    }

    fn item<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a PostItem> {
        cx.views.store.post(&self.id)
    }

    fn step_page(&mut self, d: i32, n: usize) {
        if n == 0 {
            return;
        }
        let i = (self.index as i32 + d).clamp(0, n as i32 - 1) as usize;
        if i != self.index {
            self.index = i;
            self.zoom = 1.0;
            self.pan = (0.0, 0.0);
        }
    }

    fn blurred(&self, cx: &Cx<'_, RdHost>, it: &PostItem) -> bool {
        let prefs = &cx.views.store.prefs;
        !self.revealed && ((it.post.over_18 && prefs.get("nsfw") == "blur") || (it.post.spoiler && prefs.get("spoilers") == "blur"))
    }
}

impl Machine<RdHost> for ViewerScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        let n = self.item(cx).map_or(0, |it| pages(it, cx.views.store.prefs.get("video_stream")).len());
        if common::color_key(ev) == Some(Color::Blue) && cx.views.store.prefs.on("color_keys") {
            self.comments(fx);
            return Handled::Yes;
        }
        // BACK on a zoomed picture zooms out first; the next BACK leaves.
        if common::back_any(ev) && (self.back_taken || self.zoom > 1.01) {
            if common::back_down(ev) {
                self.zoom = 1.0;
                self.pan = (0.0, 0.0);
                self.back_taken = true;
                fx.invalidate(plx_machine::present::Provenance::Input);
            } else if matches!(ev, ScreenEvent::Input(InputEvent { kind: InputKind::Key { edge: Edge::Up, .. }, .. })) {
                self.back_taken = false;
            }
            return Handled::Yes;
        }
        match ev {
            ScreenEvent::Mount => {
                if self.item(cx).is_some_and(|it| matches!(it.media, Media::Imgur(_))) {
                    fx.push(plx_machine::machine::Fx::App(RdFx::ResolveImgur(self.id.clone())));
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge, .. }, .. }) if *key != Key::Back && *key != Key::Other => {
                if *edge == Edge::Up {
                    return Handled::Yes;
                }
                let zoomed = self.zoom > 1.01;
                let step = 160.0;
                match key {
                    Key::Left if zoomed => self.pan.0 += step,
                    Key::Right if zoomed => self.pan.0 -= step,
                    Key::Up if zoomed => self.pan.1 += step,
                    Key::Down if zoomed => self.pan.1 -= step,
                    Key::Left => self.step_page(-1, n),
                    Key::Right => self.step_page(1, n),
                    Key::Up | Key::Down => self.info = !self.info,
                    Key::Ok if *edge == Edge::Down => {
                        if let Some(id) = self.youtube(cx) {
                            fx.push(plx_machine::machine::Fx::App(RdFx::OpenYouTube(id)));
                        } else if let Some(arg) = self.play_arg(cx) {
                            fx.push(plx_machine::machine::Fx::Nav(plx_machine::machine::NavOp::Push(arg)));
                        } else {
                            self.ok(cx);
                        }
                    }
                    _ => {}
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                match *e {
                    PREV => self.step_page(-1, n),
                    NEXT => self.step_page(1, n),
                    _ => match self.play_arg(cx) {
                        Some(arg) => fx.push(plx_machine::machine::Fx::Nav(plx_machine::machine::NavOp::Push(arg))),
                        None => self.ok(cx),
                    },
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
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

impl ViewerScreen {
    /// The player for the current page, when it is a playable clip.
    fn play_arg(&self, cx: &Cx<'_, RdHost>) -> Option<crate::host::RdArg> {
        let it = self.item(cx)?;
        if self.blurred(cx, it) {
            return None;
        }
        match pages(it, cx.views.store.prefs.get("video_stream")).get(self.index)? {
            Page::Clip { play: Some(u), gif, .. } => Some(crate::host::RdArg::Play { url: u.clone(), title: it.post.title.clone(), looping: *gif }),
            Page::Clip { play: None, .. } => None,
            _ => None,
        }
    }

    /// A YouTube page: the video's ID (OK hands it to ytnative).
    fn youtube(&self, cx: &Cx<'_, RdHost>) -> Option<String> {
        match &self.item(cx)?.media {
            Media::YouTube { id, .. } => Some(id.clone()),
            _ => None,
        }
    }

    fn ok(&mut self, cx: &Cx<'_, RdHost>) {
        let Some(it) = self.item(cx) else { return };
        if self.blurred(cx, it) {
            self.revealed = true;
            return;
        }
        let pg = pages(it, cx.views.store.prefs.get("video_stream"));
        if matches!(pg.get(self.index), Some(Page::Pic(..))) {
            self.zoom = if self.zoom < 1.5 {
                2.0
            } else if self.zoom < 3.0 {
                3.5
            } else {
                1.0
            };
            if self.zoom == 1.0 {
                self.pan = (0.0, 0.0);
            }
        }
    }
}

impl Focusable<RdHost> for ViewerScreen {
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: G,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            len: 3,
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        let r = zone(*key);
        Some(Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(*key - 1) })
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        if (PREV..=NEXT).contains(&want.elem) {
            want
        } else {
            FocusKey { entry: self.entry, elem: CENTER }
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: CENTER }
    }
}

fn zone(e: u32) -> Rect {
    match e {
        PREV => Rect::new(0.0, 200.0, 260.0, 680.0),
        NEXT => Rect::new(1660.0, 200.0, 260.0, 680.0),
        _ => Rect::new(260.0, 0.0, 1400.0, 1080.0),
    }
}

impl Screen<RdHost> for ViewerScreen {
    fn name(&self) -> &'static str {
        "viewer"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, RdHost>) {
        let post_ids = [RdArg::Post { id: String::new(), comment: None }.id(), RdArg::PostComments { id: String::new() }.id()];
        self.over_post = cx.views.beneath.is_some_and(|b| post_ids.contains(&b));
        let want = self.item(cx).and_then(|it| match pages(it, cx.views.store.prefs.get("video_stream")).get(self.index) {
            Some(Page::Clip { url, .. }) | Some(Page::Link(url, _)) => Some(url.clone()),
            // Nothing the TV can show: the post itself, for a phone.
            Some(Page::Nothing) => Some(format!("https://www.reddit.com{}", it.post.permalink)),
            _ => None,
        });
        match want {
            Some(u) if self.qr.as_ref().is_none_or(|q| q.0 != u) => {
                self.qr = QrCode::new(&u).ok().map(|mut q| {
                    q.prepare(QR_FRAME);
                    (u, q)
                });
            }
            None => self.qr = None,
            _ => {}
        }
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let cx = f.cx;
        let p = f.painter;
        ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 1.0]);
        let Some(it) = self.item(cx) else {
            ui::spinner(p, cx.views.now_ms, 960.0, 540.0);
            return;
        };
        let pg = pages(it, cx.views.store.prefs.get("video_stream"));
        let n = pg.len();
        self.index = self.index.min(n.saturating_sub(1));
        let blurred = self.blurred(cx, it);
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        match pg.get(self.index).cloned().unwrap_or(Page::Nothing) {
            Page::Pic(pic, _) => {
                if blurred {
                    let small = rd_core::media::preview(&it.post, 140).map_or(pic.url.clone(), |p| p.url);
                    images::draw(p, images::BLUR, &small, screen, 0.0, Fit::Cover, [0.6, 0.6, 0.6, 1.0]);
                    common::message(p, f.measure, Rect::new(0.0, 300.0, 1920.0, 400.0), if it.post.over_18 { "NSFW" } else { "Spoiler" }, "Press OK to show it.");
                } else {
                    let z = self.zoom;
                    let (iw, ih) = images::size_of(images::FULL, &pic.url).unwrap_or((pic.w.max(1) as f32, pic.h.max(1) as f32));
                    let k = (1920.0 / iw).min(1080.0 / ih) * z;
                    let (dw, dh) = (iw * k, ih * k);
                    // Keep the picture covering the screen while panning a zoom.
                    let max_x = ((dw - 1920.0) / 2.0).max(0.0);
                    let max_y = ((dh - 1080.0) / 2.0).max(0.0);
                    self.pan.0 = self.pan.0.clamp(-max_x, max_x);
                    self.pan.1 = self.pan.1.clamp(-max_y, max_y);
                    let r = Rect::new((1920.0 - dw) / 2.0 + self.pan.0, (1080.0 - dh) / 2.0 + self.pan.1, dw, dh);
                    match images::draw(p, images::FULL, &pic.url, r, 0.0, Fit::Contain, ui::WHITE) {
                        images::Drawn::Loading => {
                            // The feed's smaller copy while the full one loads.
                            if let Some(pv) = rd_core::media::preview(&it.post, 640) {
                                images::draw(p, images::THUMB, &pv.url, screen, 0.0, Fit::Contain, [0.7, 0.7, 0.7, 1.0]);
                            }
                            ui::spinner(p, cx.views.now_ms, 960.0, 540.0);
                        }
                        images::Drawn::Failed => common::message(p, f.measure, Rect::new(0.0, 300.0, 1920.0, 400.0), "Couldn't load the picture", &pic.url),
                        _ => {}
                    }
                }
            }
            Page::Clip { poster, gif, url, play, .. } => {
                if let Some(pp) = &poster {
                    let srv = if blurred { images::BLUR } else { images::FULL };
                    images::draw(p, srv, &pp.url, Rect::new(80.0, 140.0, 1180.0, 800.0), 12.0, Fit::Contain, [0.75, 0.75, 0.75, 1.0]);
                }
                let s = ui::sz(36);
                ui::text(p, if gif { "Animated picture" } else { "Video" }, 1380.0, 150.0, s, ui::WHITE, ui::ALIGN_LEFT, true);
                ui::text(p, if play.is_some() {
                    "OK plays it  \u{b7}  or scan to watch on a phone"
                } else if !gif {
                    "OK opens it in ytnative  \u{b7}  or scan with a phone"
                } else {
                    "Scan to watch it on your phone"
                }, 1380.0, 150.0 + s as f32 * 1.4, ui::sz(24), ui::DIM, ui::ALIGN_LEFT, false);
                if play.is_some() {
                    let c = Rect::new(80.0 + 590.0 - 70.0, 140.0 + 400.0 - 70.0, 140.0, 140.0);
                    ui::fill(p, c, 70.0, [0.0, 0.0, 0.0, 0.6]);
                    ui::icon::draw(p, ui::icon::Kind::Play, Rect::new(c.x + 42.0, c.y + 38.0, 64.0, 64.0), ui::WHITE, true);
                }
                self.draw_qr(p);
                ui::text_cut(p, f.measure, &url, 1380.0, QR_FRAME.y + QR_FRAME.h + 40.0, 460.0, ui::sz(20), ui::FAINT, ui::ALIGN_LEFT, false);
            }
            Page::Link(url, preview) => {
                if let Some(pp) = &preview {
                    images::draw(p, images::FULL, &pp.url, Rect::new(80.0, 140.0, 1180.0, 800.0), 12.0, Fit::Contain, [0.8, 0.8, 0.8, 1.0]);
                }
                ui::text_raw(p, &rd_core::media::host(&url), 1380.0, 150.0, ui::sz(36), ui::WHITE, ui::ALIGN_LEFT, true);
                ui::text(p, "Scan to open it on your phone", 1380.0, 150.0 + ui::sz(36) as f32 * 1.4, ui::sz(24), ui::DIM, ui::ALIGN_LEFT, false);
                self.draw_qr(p);
                for (i, l) in ui::wrap_lines(f.measure, &url, 460.0, ui::sz(20), false, 4).iter().enumerate() {
                    ui::text_raw(p, l, 1380.0, QR_FRAME.y + QR_FRAME.h + 40.0 + i as f32 * 30.0, ui::sz(20), ui::FAINT, ui::ALIGN_LEFT, false);
                }
            }
            Page::Nothing => {
                if cx.views.store.imgur_pending.contains(&self.id) {
                    ui::spinner(p, cx.views.now_ms, 960.0, 540.0);
                } else {
                    common::message(p, f.measure, Rect::new(80.0, 300.0, 1200.0, 400.0), "Nothing to show", tr("This post has no picture or video the TV can show. Scan the code to open the post on a phone."));
                    self.draw_qr(p);
                }
            }
        }
        // Captions, counter, thumbnails.
        if self.info && self.zoom <= 1.01 {
            let caption = match pg.get(self.index) {
                Some(Page::Pic(_, c)) | Some(Page::Clip { caption: c, .. }) => c.clone(),
                _ => None,
            };
            let ts = ui::sz(26);
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 110.0), 0.0, [0.0, 0.0, 0.0, 0.0]);
            p.rect(Rect::new(0.0, 0.0, 1920.0, 140.0), 0.0, [0.0, 0.0, 0.0, 0.75], [0.0, 0.0, 0.0, 0.0], 0.0);
            ui::text_cut(p, f.measure, &it.post.title, 60.0, 30.0, 1500.0, ts, ui::WHITE, ui::ALIGN_LEFT, true);
            if n > 1 {
                ui::text_raw(p, &format!("{} / {}", self.index + 1, n), 1860.0, 30.0, ts, ui::WHITE, ui::ALIGN_RIGHT, true);
            }
            let mut bottom = 1080.0;
            if n > 1 {
                // A strip of thumbnails.
                let tw = 120.0;
                let th = 80.0;
                let total = n.min(13) as f32 * (tw + 12.0);
                let first = self.index.saturating_sub(6).min(n.saturating_sub(13));
                let x0 = (1920.0 - total) / 2.0;
                let y = 1080.0 - th - 30.0;
                p.rect(Rect::new(0.0, y - 40.0, 1920.0, th + 70.0), 0.0, [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.75], 0.0);
                for (k, i) in (first..n).take(13).enumerate() {
                    let r = Rect::new(x0 + k as f32 * (tw + 12.0), y, tw, th);
                    ui::fill(p, r, 8.0, ui::surface(0.1));
                    let url = match &pg[i] {
                        Page::Pic(pp, _) => Some(pp.url.clone()),
                        Page::Clip { poster: Some(pp), .. } => Some(pp.url.clone()),
                        _ => None,
                    };
                    if let Some(u) = url {
                        images::draw(p, if blurred { images::BLUR } else { images::THUMB }, &u, r, 8.0, Fit::Cover, if i == self.index { ui::WHITE } else { [0.55, 0.55, 0.55, 1.0] });
                    }
                    if i == self.index {
                        p.rring(r, 8.0, 3.0, ui::accent());
                    }
                }
                bottom = y - 40.0;
            }
            if matches!(pg.get(self.index), Some(Page::Pic(..))) && !blurred {
                common::key_hints(p, f.measure, cx, bottom, common::VIEWER_HINTS);
                bottom -= common::hint_band();
            }
            if let Some(c) = caption {
                let lines = ui::wrap_lines(f.measure, &c, 1500.0, ts, false, 3);
                let h = lines.len() as f32 * ts as f32 * 1.4 + 30.0;
                ui::fill(p, Rect::new(180.0, bottom - h - 10.0, 1560.0, h), 14.0, [0.0, 0.0, 0.0, 0.7]);
                for (i, l) in lines.iter().enumerate() {
                    ui::text_raw(p, l, 960.0, bottom - h + 5.0 + i as f32 * ts as f32 * 1.4, ts, ui::WHITE, ui::ALIGN_CENTER, false);
                }
            }
            if n > 1 {
                let s = ui::sz(60);
                if self.index > 0 {
                    ui::text_raw(p, "\u{2039}", 60.0, 480.0, s, [1.0, 1.0, 1.0, 0.6], ui::ALIGN_CENTER, false);
                }
                if self.index + 1 < n {
                    ui::text_raw(p, "\u{203A}", 1860.0, 480.0, s, [1.0, 1.0, 1.0, 0.6], ui::ALIGN_CENTER, false);
                }
            }
        }
        if self.zoom > 1.01 {
            common::badge(p, f.measure, 40.0, 1020.0, &format!("{:.1}\u{d7}  \u{b7}  {}  \u{b7}  {}  \u{b7}  {}", self.zoom, tr("arrows move"), tr("OK zooms"), tr("BACK zooms out")), common::BADGE_BG, ui::WHITE, ui::sz(20));
        }
        for e in [PREV, CENTER, NEXT] {
            ui::stop(f, FocusKey { entry: self.entry, elem: e }, zone(e), screen);
        }
        common::toast(f.painter, f.measure, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl ViewerScreen {
    fn draw_qr(&self, p: Painter) {
        if let Some((_, qr)) = &self.qr {
            ui::fill(p, Rect::new(QR_FRAME.x - 20.0, QR_FRAME.y - 20.0, QR_FRAME.w + 40.0, QR_FRAME.h + 40.0), 20.0, ui::WHITE);
            qr.draw(p, QR_FRAME);
        }
    }
}

/// A web link: its address and a QR code to open it on a phone.
pub struct LinkScreen {
    entry: EntryId,
    url: String,
    qr: Option<QrCode>,
    state: State,
}

const LINK_QR: Rect = Rect::new(710.0, 330.0, 500.0, 500.0);

impl LinkScreen {
    pub fn new(entry: EntryId, url: String) -> Self {
        LinkScreen { entry, state: State(format!("link {url}")), url, qr: None }
    }
}

impl Machine<RdHost> for LinkScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        match ev {
            // OK (or a click) opens the link in the TV's browser.
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Ok, edge, .. }, .. }) => {
                if *edge == Edge::Down {
                    fx.push(plx_machine::machine::Fx::App(RdFx::OpenBrowser(self.url.clone())));
                }
                Handled::Yes
            }
            ScreenEvent::Activate(_) => {
                fx.push(plx_machine::machine::Fx::App(RdFx::OpenBrowser(self.url.clone())));
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<RdHost> for LinkScreen {
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: G,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: LINK_QR,
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
        Some(Placed { rect: LINK_QR, rest_rect: LINK_QR, clip: LINK_QR, index: Some(0) })
    }
    fn reconcile(&self, _want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
}

impl Screen<RdHost> for LinkScreen {
    fn name(&self) -> &'static str {
        "link"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {
        if self.qr.is_none() {
            self.qr = QrCode::new(&self.url).ok().map(|mut q| {
                q.prepare(LINK_QR);
                q
            });
        }
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let p = f.painter;
        let s = ui::sz(44);
        ui::text_raw(p, &rd_core::media::host(&self.url), 960.0, 90.0, s, ui::WHITE, ui::ALIGN_CENTER, true);
        ui::text(p, "OK opens it in the TV's browser, or scan the code with a phone", 960.0, 90.0 + s as f32 * 1.5, ui::sz(28), ui::DIM, ui::ALIGN_CENTER, false);
        if let Some(qr) = &self.qr {
            ui::fill(p, Rect::new(LINK_QR.x - 24.0, LINK_QR.y - 24.0, LINK_QR.w + 48.0, LINK_QR.h + 48.0), 24.0, ui::WHITE);
            qr.draw(p, LINK_QR);
        }
        for (i, l) in ui::wrap_lines(f.measure, &self.url, 1400.0, ui::sz(22), false, 3).iter().enumerate() {
            ui::text_raw(p, l, 960.0, LINK_QR.y + LINK_QR.h + 60.0 + i as f32 * 34.0, ui::sz(22), ui::FAINT, ui::ALIGN_CENTER, false);
        }
        ui::text(p, "BACK returns", 960.0, 1020.0, ui::sz(22), ui::FAINT, ui::ALIGN_CENTER, false);
        let _ = &self.entry;
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
