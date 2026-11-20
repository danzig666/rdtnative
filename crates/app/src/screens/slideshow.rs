//! Lean-back: a feed's pictures and videos one after another, full screen. Three modes share the
//! screen: the slideshow (pictures, galleries, GIFs and videos), the video channel (videos and
//! GIFs only, with a sound switch) and the idle screen (a subreddit's pictures after a while
//! without a button; any button returns, and is not acted on).
//!
//! The show walks the feed's posts in the feed's order with the user's filters applied, asks for
//! the next page before it runs out, and skips posts with nothing to show, NSFW posts unless they
//! are shown unblurred, and spoilers likewise. Pictures fit the screen and cross-fade; each stays
//! for the "Slideshow interval" while the next one is fetched. Videos and GIFs play through the
//! TV's player and move on at their end; a GIF loops for at least one interval, and a long video
//! moves on after the "Longest video in a slideshow" setting. A title overlay fades after a few
//! seconds and comes back on any key. Nothing moves on while the show is paused, covered (the
//! comments opened with Blue) or the app is in the background.

use super::common::{self, Color, State};
use crate::host::{RdArg, RdFx, RdHost};
use crate::images::{self, Fit};
use crate::player::Phase;
use crate::prefs::Prefs;
use crate::store::{FeedKey, PostItem};
use crate::ui;
use plx_machine::machine::{Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, Machine, NavOp, ScreenEvent};
use plx_machine::present::Provenance;
use plx_ui::consts::{classify_input, Key as K};
use plx_ui::frame::Budget;
use plx_ui::screen::{AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step};
use plx_ui::{Painter, Rect};
use rd_core::listing::{Feed, Sort, Time};
use rd_core::media::{Item, Media, Pic};
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const G: GroupId = GroupId(1);
const FULL: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);
const MESSAGE: Rect = Rect::new(0.0, 300.0, 1920.0, 400.0);
/// The overlay stays this long after a key or a new post, then fades.
const HUD_MS: u32 = 3000;
/// A picture's cross-fade over the one before.
const FADE_MS: f32 = 450.0;
/// A picture that cannot be loaded, a video that cannot be played: shown this long, then skipped.
const SKIP_MS: u64 = 1500;
/// The player takes a moment to answer a pause or a sound switch: it is not asked again sooner.
const SETTLE_MS: u32 = 1500;
/// Pages in a row that brought nothing to show (a subreddit without videos): the show stops asking.
const MAX_DRY: u8 = 5;
/// Posts ahead whose Imgur or RedGIFs media is looked up.
const LOOKAHEAD: usize = 12;
/// A feed request that failed is tried again after this long (doubling on each failure, up to
/// [`RETRY_MAX_MS`]), at most [`MAX_TRIES`] times; a rate limit says itself how long to wait.
const RETRY_MS: u32 = 30_000;
const RETRY_MAX_MS: u32 = 600_000;
const MAX_TRIES: u8 = 5;

/// Slideshows on the navigation stack: the idle screen does not open while one is.
static OPEN: AtomicUsize = AtomicUsize::new(0);
/// A slideshow on top is moving on by itself: the TV's screensaver is held off meanwhile.
static ADVANCING: AtomicBool = AtomicBool::new(false);
/// The idle screen is the page on top.
static IDLE_TOP: AtomicBool = AtomicBool::new(false);

/// A slideshow (any mode) is open.
pub fn open() -> bool {
    OPEN.load(Ordering::Relaxed) > 0
}

/// A slideshow is on screen and moving on by itself (not paused, covered or in the background).
pub fn advancing() -> bool {
    ADVANCING.load(Ordering::Relaxed)
}

/// The idle screen is on top: a page asked for meanwhile replaces it rather than going over it.
pub fn idle_on_top() -> bool {
    IDLE_TOP.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowMode {
    /// Pictures, galleries, GIFs and videos.
    Media,
    /// The video channel: videos and GIFs only, with a sound switch.
    Videos,
    /// The idle screen: pictures only; any key leaves.
    Idle,
}

/// The idle screen: the hot posts of the subreddit the settings name.
pub fn idle_arg(prefs: &Prefs) -> RdArg {
    let key = FeedKey { feed: Feed::Sub(prefs.idle_sub().to_string()), sort: Sort::Hot, time: Time::Day };
    RdArg::Slideshow { key, start: None, mode: ShowMode::Idle }
}

/// One thing the show puts on screen.
#[derive(Clone, Debug, PartialEq)]
enum Slide {
    Pic(Pic),
    /// `url`: the stream addresses (`player::pick_url`); `secs`: its length when Reddit says.
    Clip { url: String, poster: Option<Pic>, gif: bool, secs: u32 },
}

/// What a post adds to the show in `mode`: nothing for text, links, YouTube and unresolved Imgur.
fn slides(it: &PostItem, stream: &str, mode: ShowMode) -> Vec<Slide> {
    let pics = mode != ShowMode::Videos;
    let clips = mode != ShowMode::Idle;
    match &it.media {
        Media::Image(p) if pics => vec![Slide::Pic(p.clone())],
        Media::Gallery(items) => items
            .iter()
            .filter_map(|Item { pic, mp4, .. }| match mp4 {
                Some(u) if clips => Some(Slide::Clip { url: u.clone(), poster: Some(pic.clone()), gif: true, secs: 0 }),
                None if pics => Some(Slide::Pic(pic.clone())),
                _ => None,
            })
            .collect(),
        Media::Video(v) if clips => crate::player::pick_url(v, stream).map(|url| Slide::Clip { url, poster: v.poster.clone(), gif: v.gif, secs: v.duration }).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// NSFW posts and spoilers only when the settings show them unblurred (the feed's own filters
/// already left out what the user hides).
fn allowed(it: &PostItem, prefs: &Prefs) -> bool {
    (!it.post.over_18 || prefs.get("nsfw") == "show") && (!it.post.spoiler || prefs.get("spoilers") == "show")
}

/// A post the show walks through, with its slides.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    slides: Vec<Slide>,
}

fn entries(items: &[&PostItem], prefs: &Prefs, mode: ShowMode) -> Vec<Entry> {
    let stream = prefs.get("video_stream");
    items
        .iter()
        .filter(|it| allowed(it, prefs))
        .filter_map(|it| {
            let slides = slides(it, stream, mode);
            (!slides.is_empty()).then(|| Entry { id: it.post.id.clone(), slides })
        })
        .collect()
}

/// Where the show is: a post (by ID, so pages landing or Imgur posts resolving do not move it) and
/// one of its slides. `hint` is the post's place when last seen: a post that left the list (read,
/// then hidden by the filters) is followed by the one now in its place.
#[derive(Clone, Debug, PartialEq)]
struct Spot {
    id: String,
    slide: usize,
    hint: usize,
}

impl Spot {
    fn at(list: &[Entry], i: usize, slide: usize) -> Spot {
        Spot { id: list[i].id.clone(), slide, hint: i }
    }
    fn index(&self, list: &[Entry]) -> Option<usize> {
        list.iter().position(|e| e.id == self.id)
    }
}

/// How long a failed feed request waits before it is tried again: what a rate limit asked for
/// (`limit_secs`), else [`RETRY_MS`] doubling with each failed try.
fn retry_wait(limit_secs: Option<u64>, tries: u8) -> u32 {
    match limit_secs {
        Some(secs) => (secs.min(3600) as u32).saturating_mul(1000).saturating_add(500),
        None => RETRY_MS.saturating_mul(1 << tries.min(5)).min(RETRY_MAX_MS),
    }
}

/// The spot after `cur`: its post's next slide, else the next post's first; `None` at the end.
fn next(list: &[Entry], cur: &Spot) -> Option<Spot> {
    match cur.index(list) {
        Some(i) if cur.slide + 1 < list[i].slides.len() => Some(Spot { slide: cur.slide + 1, hint: i, ..cur.clone() }),
        Some(i) => (i + 1 < list.len()).then(|| Spot::at(list, i + 1, 0)),
        None => (cur.hint < list.len()).then(|| Spot::at(list, cur.hint, 0)),
    }
}

/// The spot before `cur`: its post's slide before, else the last slide of the post before.
fn prev(list: &[Entry], cur: &Spot) -> Option<Spot> {
    match cur.index(list) {
        Some(i) if cur.slide > 0 => Some(Spot { slide: cur.slide - 1, hint: i, ..cur.clone() }),
        Some(i) => i.checked_sub(1).map(|j| Spot::at(list, j, list[j].slides.len() - 1)),
        None => cur.hint.checked_sub(1).filter(|&j| j < list.len()).map(|j| Spot::at(list, j, list[j].slides.len() - 1)),
    }
}

/// Where the show starts: post `start` (`ids` is the feed's order), or the first post after it
/// with something to show; the first post when there is no `start`. `None`: nothing at or after
/// it yet (the next page may bring some).
fn first(list: &[Entry], ids: &[&str], start: Option<&str>) -> Option<Spot> {
    let from = start.and_then(|s| ids.iter().position(|i| *i == s)).unwrap_or(0);
    let i = list.iter().position(|e| ids.iter().position(|x| *x == e.id).is_some_and(|p| p >= from))?;
    Some(Spot::at(list, i, 0))
}

/// The slide at `cur` (a gallery that lost items keeps to its last).
fn slide_at<'a>(list: &'a [Entry], cur: &Spot) -> Option<&'a Slide> {
    let e = &list[cur.index(list)?];
    e.slides.get(cur.slide.min(e.slides.len().saturating_sub(1)))
}

/// A clip's length: as the player reports it, else as Reddit said (0: not known).
fn length_ms(slide: &Slide, reported: u64) -> u64 {
    match slide {
        Slide::Clip { secs, .. } if reported == 0 => *secs as u64 * 1000,
        _ => reported,
    }
}

/// How long a slide stays, in ms it has been showing: a picture one interval; a GIF its length,
/// looped to at least one interval and cut at `cap` once that interval is over; a video until its
/// end (`u64::MAX`) or `cap`. `cap` 0 means whole videos; `length` 0 means not known.
fn hold_ms(slide: &Slide, length: u64, interval: u64, cap: u64) -> u64 {
    let cap = if cap == 0 { u64::MAX } else { cap };
    match slide {
        Slide::Pic(_) => interval,
        Slide::Clip { gif: true, .. } => length.min(cap).max(interval),
        Slide::Clip { .. } => cap,
    }
}

pub struct SlideshowScreen {
    entry: EntryId,
    key: FeedKey,
    mode: ShowMode,
    /// The post to start at (the one focused in the feed).
    start: Option<String>,
    /// The posts with something to show, in the feed's order (rebuilt when the store changes).
    list: Vec<Entry>,
    /// (feed gen, store gen) the list was built for.
    sig: (u32, u32),
    at: Option<Spot>,
    /// How long the current slide has been showing (ms; not while paused, loading or covered).
    shown_ms: u64,
    last_tick: u32,
    paused: bool,
    covered: bool,
    /// The stream addresses handed to the player for the current clip.
    playing: Option<String>,
    /// When the player was last asked to pause, resume or switch the sound.
    asked_player_at: u32,
    /// When the current clip failed (it is skipped shortly after, with a note).
    failed_at: Option<u32>,
    /// The picture fading out under the current one, and since when the current one shows.
    fade: Option<(String, Option<u32>)>,
    hud_at: u32,
    /// The video channel's sound switch.
    sound: bool,
    /// The video plane shows: the page is drawn transparent over it.
    video_up: bool,
    /// The next page asked for: (the feed's gen, entries in the show) then. Any page that lands
    /// or fails moves the gen, so a page of posts already listed does not leave it set.
    asked: Option<(u32, usize)>,
    /// Pages in a row that added nothing to show.
    dry: u8,
    /// Posts whose Imgur / RedGIFs media was looked up (a failed one stays unresolved).
    tried: HashSet<String>,
    /// The idle screen was told to leave.
    leaving: bool,
    /// The feed was gone (a sign-in, sign-out or account switch drops every feed) and was asked
    /// for again: not asked again until it is back.
    reloading: bool,
    /// The first page is being fetched afresh: the show starts on the new listing, not the old.
    wait_fresh: bool,
    /// A failed request is tried again at this time (ms); how many tries failed so far.
    retry_at: Option<u32>,
    tries: u8,
    state: State,
}

impl SlideshowScreen {
    pub fn new(entry: EntryId, key: FeedKey, start: Option<String>, mode: ShowMode) -> Self {
        SlideshowScreen {
            entry,
            state: State(format!("slideshow {mode:?} {}", key.feed.title())),
            key,
            mode,
            start,
            list: Vec::new(),
            sig: (u32::MAX, u32::MAX),
            at: None,
            shown_ms: 0,
            last_tick: 0,
            paused: false,
            covered: false,
            playing: None,
            asked_player_at: 0,
            failed_at: None,
            fade: None,
            hud_at: 0,
            sound: true,
            video_up: false,
            asked: None,
            dry: 0,
            tried: HashSet::new(),
            leaving: false,
            reloading: false,
            wait_fresh: false,
            retry_at: None,
            tries: 0,
        }
    }

    fn slide(&self) -> Option<&Slide> {
        slide_at(&self.list, self.at.as_ref()?)
    }

    fn item<'a>(&self, cx: &Cx<'a, RdHost>) -> Option<&'a PostItem> {
        cx.views.store.post(&self.at.as_ref()?.id)
    }

    /// The feed can bring no more posts to show.
    fn exhausted(&self, cx: &Cx<'_, RdHost>) -> bool {
        cx.views.store.feed(&self.key).is_some_and(|f| !f.loading && (f.done || f.error.is_some() || self.dry >= MAX_DRY))
    }

    /// Rebuilds the list when the store changed, finds the start, looks up the Imgur posts coming
    /// up and asks for the next page before the show runs out.
    fn refresh(&mut self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let st = cx.views.store;
        let Some(fd) = st.feed(&self.key) else {
            // The session was reset (a sign-in, sign-out or account switch): every feed is gone.
            if !std::mem::replace(&mut self.reloading, true) {
                // A sort only a signed-in user has (Best) falls back to the feed's own.
                if !self.key.feed.sorts(st.signed_in()).contains(&self.key.sort) {
                    self.key = st.key_for(self.key.feed.clone());
                }
                self.asked = None;
                self.dry = 0;
                self.tried.clear();
                self.retry_at = None;
                self.tries = 0;
                self.sig = (u32::MAX, u32::MAX);
                fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload: false }));
            }
            return;
        };
        self.reloading = false;
        if self.wait_fresh && !fd.loading {
            self.wait_fresh = false;
        }
        if (fd.gen, st.gen) != self.sig {
            self.sig = (fd.gen, st.gen);
            let items = st.feed_items(&self.key);
            self.list = entries(&items, &st.prefs, self.mode);
            if let Some((gen, n)) = self.asked {
                if !fd.loading && fd.gen != gen {
                    // A page that added nothing to show (all filtered, or posts already listed)
                    // counts as dry, so the show still ends at the end of the feed.
                    self.dry = if self.list.len() > n { 0 } else { self.dry.saturating_add(1) };
                    self.asked = None;
                }
            }
            let from = self.at.as_ref().and_then(|a| items.iter().position(|it| it.post.id == a.id)).unwrap_or(0);
            for it in items.iter().skip(from).take(LOOKAHEAD) {
                if matches!(it.media, Media::Imgur(_)) && allowed(it, &st.prefs) && self.tried.insert(it.post.id.clone()) {
                    fx.push(Fx::App(RdFx::ResolveImgur(it.post.id.clone())));
                }
            }
            match &self.at {
                // The listing is being fetched afresh: the show starts on the new one.
                None if self.wait_fresh => {}
                None => {
                    let ids: Vec<&str> = items.iter().map(|it| it.post.id.as_str()).collect();
                    let to = first(&self.list, &ids, self.start.as_deref());
                    let to = to.or_else(|| (!self.list.is_empty() && self.exhausted(cx)).then(|| Spot::at(&self.list, 0, 0)));
                    if let Some(to) = to {
                        self.go(to, cx, fx);
                    }
                }
                // The post left the list (hidden after it was read): the one now in its place.
                Some(a) if a.index(&self.list).is_none() => match next(&self.list, a).or_else(|| (!self.list.is_empty()).then(|| Spot::at(&self.list, 0, 0))) {
                    Some(to) => self.go(to, cx, fx),
                    None => self.at = None,
                },
                Some(_) => {}
            }
        }
        let left = match &self.at {
            Some(a) => a.index(&self.list).map_or(0, |i| self.list.len() - i - 1),
            None => 0,
        };
        if left < 5 && self.asked.is_none() && self.dry < MAX_DRY && !fd.loading && !fd.done && fd.error.is_none() && !fd.ids.is_empty() {
            self.asked = Some((fd.gen, self.list.len()));
            fx.push(Fx::App(RdFx::MoreFeed(self.key.clone())));
        }
    }

    /// A failed request (the network was down, Reddit asked to wait) is tried again a while
    /// later while the show runs; meanwhile the show goes round what it has.
    fn retry(&mut self, now: u32, running: bool, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some(fd) = cx.views.store.feed(&self.key) else { return };
        if fd.error.is_none() {
            self.retry_at = None;
            self.tries = 0;
            return;
        }
        // A refused sign-in does not get better by asking again, and a feed with no next page
        // (a failed reload of one that ended) has nothing to ask for.
        let nothing_next = !fd.ids.is_empty() && (fd.done || fd.after.is_none());
        if fd.loading || fd.auth_failed || nothing_next || self.asked.is_some() || !running || self.tries >= MAX_TRIES {
            return;
        }
        match self.retry_at {
            None => self.retry_at = Some(now.wrapping_add(retry_wait(fd.retry.map(|r| r.0), self.tries))),
            Some(t) if now.wrapping_sub(t) < u32::MAX / 2 => {
                self.retry_at = None;
                self.tries += 1;
                if fd.ids.is_empty() {
                    fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload: false }));
                } else {
                    // Never a reload: that would empty the listing under the show.
                    self.asked = Some((fd.gen, self.list.len()));
                    fx.push(Fx::App(RdFx::MoreFeed(self.key.clone())));
                }
            }
            Some(_) => {}
        }
    }

    /// Shows `to`. The overlay comes back for a new post (the idle screen shows it for its first
    /// only); a picture fades in over the picture before; a clip starts on the next tick.
    fn go(&mut self, to: Spot, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let before = self.slide().cloned();
        let new_post = self.at.as_ref().is_none_or(|a| a.id != to.id);
        if new_post && (self.mode != ShowMode::Idle || self.at.is_none()) {
            self.hud_at = cx.views.now_ms;
        }
        self.at = Some(to);
        self.shown_ms = 0;
        self.failed_at = None;
        self.fade = match (before, self.slide()) {
            (Some(Slide::Pic(b)), Some(Slide::Pic(a))) if b.url != a.url => Some((b.url, None)),
            _ => None,
        };
        if self.playing.take().is_some() {
            fx.push(Fx::App(RdFx::StopVideo));
        }
        fx.invalidate(Provenance::Lifecycle);
    }

    /// The next slide; at the end of a feed with nothing more to bring, the first one again.
    fn advance(&mut self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let Some(at) = self.at.clone() else { return };
        if let Some(to) = next(&self.list, &at) {
            self.go(to, cx, fx);
        } else if self.exhausted(cx) && !self.list.is_empty() {
            self.go(Spot::at(&self.list, 0, 0), cx, fx);
        }
    }

    fn back(&mut self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        if let Some(to) = self.at.as_ref().and_then(|a| prev(&self.list, a)) {
            self.go(to, cx, fx);
        }
    }

    fn set_paused(&mut self, on: bool, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        let now = cx.views.now_ms;
        self.paused = on;
        self.hud_at = now;
        let phase = &cx.views.store.play.phase;
        if self.playing.is_some() && ((on && *phase == Phase::Playing) || (!on && *phase == Phase::Paused)) {
            fx.push(Fx::App(RdFx::PauseVideo));
            self.asked_player_at = now;
        }
    }

    /// Flips the sound switch. The player hears of it only while the video plays (one still
    /// loading or paused would not take it); the tick sets it as soon as the video plays.
    fn switch_sound(&mut self, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        self.sound = !self.sound;
        if self.playing.is_some() && cx.views.store.play.phase == Phase::Playing && matches!(self.slide(), Some(Slide::Clip { gif: false, .. })) {
            fx.push(Fx::App(RdFx::MuteVideo(!self.sound)));
            self.asked_player_at = cx.views.now_ms;
        }
    }

    fn tick(&mut self, now: u32, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) {
        // A long gap (the page was covered, the loop stalled) does not count as showing time.
        let dt = now.wrapping_sub(self.last_tick).min(100) as u64;
        self.last_tick = now;
        self.refresh(cx, fx);
        let running = !self.paused && !self.covered && crate::FOREGROUND.load(Ordering::Relaxed);
        self.retry(now, running, cx, fx);
        // A show that cannot move on (at its last slide, with no page coming) never holds the
        // screensaver off over a still picture.
        let can_move =
            self.at.as_ref().is_some_and(|a| next(&self.list, a).is_some()) || self.exhausted(cx) || cx.views.store.feed(&self.key).is_some_and(|f| f.loading);
        ADVANCING.store(running && self.at.is_some() && can_move, Ordering::Relaxed);
        let Some(slide) = self.slide().cloned() else {
            self.video_up = false;
            return;
        };
        let prefs = &cx.views.store.prefs;
        let interval = prefs.num("slide_secs").max(1) as u64 * 1000;
        let cap = prefs.num("slide_video").max(0) as u64 * 1000;
        let play = &cx.views.store.play;
        let done = match &slide {
            Slide::Pic(p) => {
                let ready = images::size_of(images::FULL, &p.url).is_some();
                let failed = !ready && plx_ui::tex::failed_on(images::FULL, &p.url, 0, 0, false);
                if running && (ready || failed) {
                    self.shown_ms += dt;
                }
                self.shown_ms >= if failed { SKIP_MS } else { interval }
            }
            Slide::Clip { url, gif, .. } if self.playing.as_deref() != Some(url.as_str()) => {
                // Started only while the show runs: a paused show keeps the poster up.
                if running {
                    self.playing = Some(url.clone());
                    self.asked_player_at = now;
                    fx.push(Fx::App(RdFx::Play { url: url.clone(), looping: *gif }));
                }
                false
            }
            Slide::Clip { gif, .. } => {
                // The player follows the show: paused while the show is (or covered, or in the
                // background), and with the video channel's sound switch on every new video (each
                // starts with sound; the switch is set as soon as it plays).
                let settled = now.wrapping_sub(self.asked_player_at) > SETTLE_MS;
                let ask = match play.phase {
                    Phase::Playing if !running => settled.then_some(RdFx::PauseVideo),
                    Phase::Paused if running => settled.then_some(RdFx::PauseVideo),
                    Phase::Playing if !gif && play.muted == self.sound => Some(RdFx::MuteVideo(!self.sound)),
                    _ => None,
                };
                if let Some(a) = ask {
                    fx.push(Fx::App(a));
                    self.asked_player_at = now;
                }
                if running && play.phase == Phase::Playing {
                    self.shown_ms += dt;
                }
                match play.phase {
                    Phase::Ended => true,
                    Phase::Failed(_) => {
                        let at = *self.failed_at.get_or_insert_with(|| {
                            fx.push(Fx::App(RdFx::Toast("Couldn't play this video".into())));
                            now
                        });
                        now.wrapping_sub(at) as u64 > SKIP_MS
                    }
                    _ => self.shown_ms >= hold_ms(&slide, length_ms(&slide, play.duration_ms), interval, cap),
                }
            }
        };
        let up = matches!(slide, Slide::Clip { .. }) && self.playing.is_some() && matches!(play.phase, Phase::Playing | Phase::Paused | Phase::Ended);
        if up != self.video_up {
            self.video_up = up;
            fx.invalidate(Provenance::Lifecycle);
        }
        if done && running {
            self.advance(cx, fx);
        }
    }

    /// The share of the current slide's time that has gone.
    fn progress(&self, cx: &Cx<'_, RdHost>) -> f32 {
        let Some(slide) = self.slide() else { return 0.0 };
        let prefs = &cx.views.store.prefs;
        let interval = prefs.num("slide_secs").max(1) as u64 * 1000;
        let cap = prefs.num("slide_video").max(0) as u64 * 1000;
        let length = length_ms(slide, cx.views.store.play.duration_ms);
        let total = match slide {
            // A video ends at its end or the cap, whichever comes first.
            Slide::Clip { gif: false, .. } if length > 0 => length.min(hold_ms(slide, length, interval, cap)),
            _ => hold_ms(slide, length, interval, cap),
        };
        if total == 0 || total == u64::MAX {
            return 0.0;
        }
        (self.shown_ms as f32 / total as f32).clamp(0.0, 1.0)
    }

    fn draw_pic(&mut self, p: Painter, m: &dyn plx_machine::machine::Measure, url: &str, now: u32) {
        let ready = images::size_of(images::FULL, url).is_some();
        if let Some((from, since)) = self.fade.as_mut() {
            // The picture before stays up until this one is here, then fades into it.
            if !ready {
                images::draw(p, images::FULL, from, FULL, 0.0, Fit::Contain, ui::WHITE);
                return;
            }
            let a = ui::ease_out(now.wrapping_sub(*since.get_or_insert(now)) as f32 / FADE_MS);
            images::draw(p.alpha(1.0 - a), images::FULL, from, FULL, 0.0, Fit::Contain, ui::WHITE);
            images::draw(p.alpha(a), images::FULL, url, FULL, 0.0, Fit::Contain, ui::WHITE);
            if a < 1.0 {
                ui::animating();
            } else {
                self.fade = None;
            }
            return;
        }
        match images::draw(p, images::FULL, url, FULL, 0.0, Fit::Contain, ui::WHITE) {
            images::Drawn::Loading => ui::spinner(p, now, 960.0, 540.0),
            images::Drawn::Failed => common::message(p, m, MESSAGE, "Couldn't load the picture", ""),
            images::Drawn::At(_) => {}
        }
    }

    fn draw_empty(&self, p: Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, RdHost>) {
        match cx.views.store.feed(&self.key) {
            Some(f) if self.list.is_empty() && f.error.is_some() => common::message(p, m, MESSAGE, "Couldn't load this feed", f.error.as_deref().unwrap_or_default()),
            Some(_) if self.list.is_empty() && self.exhausted(cx) => common::message(
                p,
                m,
                MESSAGE,
                "Nothing to show",
                crate::i18n::tr(if self.mode == ShowMode::Videos { "This feed has no videos the TV can play." } else { "This feed has no pictures the TV can show." }),
            ),
            _ => ui::spinner(p, cx.views.now_ms, 960.0, 540.0),
        }
    }

    /// Title, subreddit, author and score on top; the keys and a thin line for the slide's time
    /// at the bottom. It fades a few seconds after the last key and stays while paused.
    fn draw_overlay(&self, p: Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, RdHost>) {
        let now = cx.views.now_ms;
        let since = now.wrapping_sub(self.hud_at) as f32;
        let a = if self.paused { 1.0 } else { 1.0 - ui::ease_out((since - HUD_MS as f32) / 400.0) };
        let Some(it) = self.item(cx).filter(|_| a > 0.01) else { return };
        // The time line moves and the overlay fades (paused, it holds still).
        if !self.paused {
            ui::animating();
        }
        let p = p.alpha(a);
        let tr = crate::i18n::tr;
        let ts = ui::sz(34);
        let ms = ui::sz(24);
        p.rect(Rect::new(0.0, 0.0, 1920.0, 230.0), 0.0, [0.0, 0.0, 0.0, 0.75], [0.0, 0.0, 0.0, 0.0], 0.0);
        ui::text_cut(p, m, &it.post.title, 80.0, 44.0, 1460.0, ts, ui::WHITE, ui::ALIGN_LEFT, true);
        let y = 44.0 + ts as f32 * 1.45;
        let meta = format!("r/{}  \u{b7}  u/{}", it.post.subreddit, it.post.author);
        let w = ui::text_raw(p, &meta, 80.0, y, ms, ui::DIM, ui::ALIGN_LEFT, false);
        common::score_line(p, m, it, 80.0 + w + ms as f32 * 1.2, y, ms, ui::DIM, cx.views.store.prefs.on("show_scores"));
        // Right: the place in a gallery, the video channel's sound.
        let mut ry = 44.0;
        if let Some((i, n)) = self.at.as_ref().and_then(|a| Some((a.slide, self.list[a.index(&self.list)?].slides.len()))).filter(|(_, n)| *n > 1) {
            ui::text_raw(p, &format!("{} / {n}", i.min(n - 1) + 1), 1840.0, ry, ts, ui::WHITE, ui::ALIGN_RIGHT, true);
            ry += ts as f32 * 1.45;
        }
        if self.mode == ShowMode::Videos {
            ui::text(p, if self.sound { "Sound on" } else { "Sound off" }, 1840.0, ry, ms, ui::DIM, ui::ALIGN_RIGHT, false);
        }
        p.rect(Rect::new(0.0, 880.0, 1920.0, 200.0), 0.0, [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.7], 0.0);
        let mut keys: Vec<&str> = Vec::new();
        if self.mode == ShowMode::Idle {
            keys.push(tr("Any button returns"));
        } else {
            keys.push(tr(if self.paused { "OK resumes" } else { "OK pauses" }));
            keys.push(tr("\u{2190}/\u{2192} previous / next"));
            if self.mode == ShowMode::Videos {
                keys.push(tr("\u{2191} sound"));
            }
            if cx.views.store.prefs.on("color_keys") {
                keys.push(tr("Blue: comments"));
            }
            keys.push(tr("BACK leaves"));
        }
        ui::text_raw(p, &keys.join("  \u{b7}  "), 1840.0, 1010.0, ui::sz(22), ui::DIM, ui::ALIGN_RIGHT, false);
        let bar = Rect::new(0.0, 1074.0, 1920.0, 6.0);
        ui::fill(p, bar, 0.0, [1.0, 1.0, 1.0, 0.2]);
        ui::fill(p, Rect::new(bar.x, bar.y, bar.w * self.progress(cx), bar.h), 0.0, ui::accent());
    }

    fn draw_paused(&self, p: Painter, m: &dyn plx_machine::machine::Measure) {
        let s = ui::sz(30);
        let label = crate::i18n::tr("Paused");
        let h = ui::ls(96.0);
        let w = ui::text_width(m, label, s, true) + h * 1.4;
        let r = Rect::new((1920.0 - w) / 2.0, (1080.0 - h) / 2.0, w, h);
        ui::fill(p, r, h / 2.0, [0.0, 0.0, 0.0, 0.65]);
        let ic = s as f32 * 1.2;
        ui::icon::draw(p, ui::icon::Kind::Pause, Rect::new(r.x + h * 0.4, r.y + (h - ic) / 2.0, ic, ic), ui::WHITE, true);
        ui::text_raw(p, label, r.x + h * 0.4 + ic + 14.0, r.y + (h - s as f32 * 1.3) / 2.0, s, ui::WHITE, ui::ALIGN_LEFT, true);
    }
}

impl Machine<RdHost> for SlideshowScreen {
    type Ev = ScreenEvent<RdHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, RdHost>, fx: &mut Effects<'_, RdHost>) -> Handled {
        let now = cx.views.now_ms;
        // The idle screen: any button (or the pointer) returns to where the user was, and does
        // nothing else.
        if self.mode == ShowMode::Idle {
            if let ScreenEvent::Input(InputEvent { kind, .. }) = ev {
                if matches!(kind, InputKind::Key { .. } | InputKind::Pointer { .. } | InputKind::Click { .. } | InputKind::Drag { .. } | InputKind::Wheel { .. } | InputKind::Text(_)) {
                    if !std::mem::replace(&mut self.leaving, true) {
                        fx.push(Fx::Nav(NavOp::Pop));
                    }
                    return Handled::Yes;
                }
            }
        }
        if let Some(c) = common::color_key(ev) {
            self.hud_at = now;
            // Signed out, the vote and save buttons offer to sign in, as on a feed.
            if let Some(it) = self.item(cx).filter(|_| cx.views.store.prefs.on("color_keys") && !common::sign_in_first(c, cx, fx)) {
                let name = it.post.name.clone();
                match c {
                    Color::Green => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(true) { 0 } else { 1 } })),
                    Color::Red => fx.push(Fx::App(RdFx::Vote { fullname: name, dir: if it.post.likes == Some(false) { 0 } else { -1 } })),
                    Color::Yellow => fx.push(Fx::App(RdFx::Save { fullname: name, on: !it.post.saved })),
                    Color::Blue => fx.push(Fx::Nav(NavOp::Push(RdArg::PostComments { id: it.post.id.clone() }))),
                }
            }
            fx.invalidate(Provenance::Input);
            return Handled::Yes;
        }
        match ev {
            ScreenEvent::Mount => {
                OPEN.fetch_add(1, Ordering::Relaxed);
                self.last_tick = now;
                self.hud_at = now;
                // The idle screen shows the subreddit's pictures of now, not those of its first
                // time, and an old listing is fetched again; the show starts on the new one.
                let stale = cx.views.store.feed(&self.key).is_some_and(|f| !f.ids.is_empty() && f.stale(rd_core::now_secs()));
                let reload = self.mode == ShowMode::Idle || stale;
                self.wait_fresh = reload;
                IDLE_TOP.store(self.mode == ShowMode::Idle, Ordering::Relaxed);
                fx.push(Fx::App(RdFx::LoadFeed { key: self.key.clone(), reload }));
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                OPEN.fetch_sub(1, Ordering::Relaxed);
                ADVANCING.store(false, Ordering::Relaxed);
                IDLE_TOP.store(false, Ordering::Relaxed);
                if self.playing.take().is_some() {
                    fx.push(Fx::App(RdFx::StopVideo));
                }
                Handled::Yes
            }
            ScreenEvent::Cover => {
                self.covered = true;
                // A page pushed over the idle screen replaced its Pop: the next key asks again.
                self.leaving = false;
                ADVANCING.store(false, Ordering::Relaxed);
                IDLE_TOP.store(false, Ordering::Relaxed);
                // A clip waits under the comments: paused when it plays, stopped while it still
                // loads (it would start, with its sound, under the page).
                if self.playing.is_some() {
                    match cx.views.store.play.phase {
                        Phase::Playing => {
                            fx.push(Fx::App(RdFx::PauseVideo));
                            self.asked_player_at = now;
                        }
                        Phase::Paused => {}
                        _ => {
                            self.playing = None;
                            fx.push(Fx::App(RdFx::StopVideo));
                        }
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                self.covered = false;
                self.leaving = false;
                IDLE_TOP.store(self.mode == ShowMode::Idle, Ordering::Relaxed);
                self.last_tick = now;
                self.hud_at = now;
                // A video opened from the comments took the player: the clip starts again.
                if cx.views.store.play.phase == Phase::Idle {
                    self.playing = None;
                }
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.tick(t.ms, cx, fx);
                Handled::Yes
            }
            // BACK leaves (the dispatcher pops).
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, .. }, .. }) => Handled::No,
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, sym, wcode, edge, .. }, .. }) => {
                if *edge != Edge::Down {
                    return Handled::Yes;
                }
                self.hud_at = now;
                match classify_input(*key, *sym, *wcode) {
                    K::Left { .. } => self.back(cx, fx),
                    K::Right { .. } => self.advance(cx, fx),
                    K::Ok | K::PlayPause => self.set_paused(!self.paused, cx, fx),
                    K::Play => self.set_paused(false, cx, fx),
                    K::Pause => self.set_paused(true, cx, fx),
                    K::Stop => fx.push(Fx::Nav(NavOp::Pop)),
                    K::Up if self.mode == ShowMode::Videos => self.switch_sound(cx, fx),
                    _ => {}
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Click { .. }, .. }) => {
                self.set_paused(!self.paused, cx, fx);
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Pointer { .. }, .. }) => {
                self.hud_at = now;
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<RdHost> for SlideshowScreen {
    fn groups(&self, _cx: &Cx<'_, RdHost>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec { id: G, kind: GroupKind::Column, seat: Seat::Remembered, reachable: AxisMask::BOTH, edge: [EdgeRule::Stop; 4], extent: FULL, len: 1, elem: ElemKind::Bare });
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, RdHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, RdHost>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, _key: &u32, _cx: &Cx<'_, RdHost>, _at: At) -> Option<Placed> {
        Some(Placed { rect: FULL, rest_rect: FULL, clip: FULL, index: Some(0) })
    }
    fn reconcile(&self, _want: FocusKey<u32>, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, RdHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
}

impl Screen<RdHost> for SlideshowScreen {
    fn name(&self) -> &'static str {
        "slideshow"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, RdHost>) -> Option<Cow<'_, str>> {
        None
    }
    /// The next picture (or the next clip's poster) is fetched while this one shows.
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, RdHost>) {
        let Some(at) = &self.at else { return };
        let url = match next(&self.list, at).as_ref().and_then(|s| slide_at(&self.list, s)) {
            Some(Slide::Pic(p)) | Some(Slide::Clip { poster: Some(p), .. }) => p.url.clone(),
            _ => return,
        };
        plx_ui::tex::warm_on(images::FULL, &url, 0, 0, false);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, RdHost>) {
        let (p, cx, m) = (f.painter, f.cx, f.measure);
        let now = cx.views.now_ms;
        if !self.video_up {
            ui::fill(p, FULL, 0.0, [0.0, 0.0, 0.0, 1.0]);
        }
        match self.slide().cloned() {
            Some(Slide::Pic(pic)) => self.draw_pic(p, m, &pic.url, now),
            Some(Slide::Clip { poster, .. }) if !self.video_up => {
                if let Some(pp) = &poster {
                    images::draw(p, images::FULL, &pp.url, FULL, 0.0, Fit::Contain, [0.75, 0.75, 0.75, 1.0]);
                }
                if self.playing.is_some() && matches!(cx.views.store.play.phase, Phase::Failed(_)) {
                    common::message(p, m, MESSAGE, "Couldn't play this video", "");
                } else if !self.paused {
                    ui::spinner(p, now, 960.0, 540.0);
                }
            }
            Some(Slide::Clip { .. }) => {}
            None => self.draw_empty(p, m, cx),
        }
        self.draw_overlay(p, m, cx);
        if self.paused {
            self.draw_paused(p, m);
        }
        common::toast(p, m, cx);
    }
    fn render(&self) -> RenderStrategy {
        if self.video_up {
            RenderStrategy::VideoPlane
        } else {
            RenderStrategy::Page
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rd_core::media::Video;

    fn pic(u: &str) -> Pic {
        Pic { url: u.into(), w: 1920, h: 1080 }
    }

    fn post(id: &str, media: Media) -> PostItem {
        let p = rd_core::model::Post::from_json(&serde_json::json!({ "id": id, "name": format!("t3_{id}") }));
        PostItem { post: p, media, preview: String::new(), new_comments: 0 }
    }

    fn video(gif: bool) -> Media {
        Media::Video(Video { hls: Some("h".into()), dash: None, mp4: Some("m".into()), w: 0, h: 0, duration: 30, gif, poster: Some(pic("p")) })
    }

    fn gallery() -> Media {
        let item = |u: &str, mp4: Option<&str>| Item { pic: pic(u), mp4: mp4.map(str::to_string), caption: None, link: None };
        Media::Gallery(vec![item("g1", None), item("g2", Some("g2.mp4")), item("g3", None)])
    }

    fn kinds(s: &[Slide]) -> Vec<&str> {
        s.iter().map(|s| if let Slide::Pic(p) = s { p.url.as_str() } else { "clip" }).collect()
    }

    #[test]
    fn each_mode_takes_its_media() {
        let img = post("a", Media::Image(pic("i")));
        let vid = post("b", video(false));
        let gal = post("c", gallery());
        assert_eq!(kinds(&slides(&img, "hls", ShowMode::Media)), ["i"]);
        assert_eq!(kinds(&slides(&img, "hls", ShowMode::Idle)), ["i"]);
        assert!(slides(&img, "hls", ShowMode::Videos).is_empty());
        assert_eq!(slides(&vid, "hls", ShowMode::Videos), [Slide::Clip { url: "h\nm".into(), poster: Some(pic("p")), gif: false, secs: 30 }]);
        assert!(slides(&vid, "hls", ShowMode::Idle).is_empty());
        assert_eq!(kinds(&slides(&gal, "hls", ShowMode::Media)), ["g1", "clip", "g3"]);
        assert_eq!(kinds(&slides(&gal, "hls", ShowMode::Videos)), ["clip"]);
        assert_eq!(kinds(&slides(&gal, "hls", ShowMode::Idle)), ["g1", "g3"]);
        for m in [Media::None, Media::YouTube { id: "x".into(), url: "u".into() }, Media::Link { url: "u".into(), preview: Some(pic("p")) }, Media::Imgur(rd_core::media::ImgurRef::Album("x".into()))] {
            assert!(slides(&post("d", m), "hls", ShowMode::Media).is_empty());
        }
    }

    #[test]
    fn nsfw_and_spoilers_only_when_shown_unblurred() {
        let mut nsfw = post("a", Media::Image(pic("i")));
        nsfw.post.over_18 = true;
        let mut spoiler = post("b", Media::Image(pic("j")));
        spoiler.post.spoiler = true;
        let plain = post("c", Media::Image(pic("k")));
        let text = post("d", Media::None);
        let mut prefs = Prefs::default();
        let ids = |p: &Prefs| entries(&[&nsfw, &spoiler, &plain, &text], p, ShowMode::Media).into_iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(&prefs), ["c"]);
        prefs.set("nsfw", "show");
        assert_eq!(ids(&prefs), ["a", "c"]);
        prefs.set("spoilers", "show");
        assert_eq!(ids(&prefs), ["a", "b", "c"]);
    }

    fn list(spec: &[(&str, usize)]) -> Vec<Entry> {
        spec.iter().map(|(id, n)| Entry { id: id.to_string(), slides: (0..*n).map(|i| Slide::Pic(pic(&format!("{id}{i}")))).collect() }).collect()
    }

    #[test]
    fn walks_slides_then_posts() {
        let l = list(&[("a", 1), ("b", 3), ("c", 1)]);
        let mut at = Spot::at(&l, 0, 0);
        let mut seen = vec![];
        while let Some(n) = next(&l, &at) {
            at = n;
            seen.push((at.id.clone(), at.slide));
        }
        let want: Vec<(String, usize)> = vec![("b".into(), 0), ("b".into(), 1), ("b".into(), 2), ("c".into(), 0)];
        assert_eq!(seen, want);
        let back = prev(&l, &at).unwrap();
        assert_eq!((back.id.as_str(), back.slide), ("b", 2));
        assert!(prev(&l, &Spot::at(&l, 0, 0)).is_none());
        assert_eq!(slide_at(&l, &back), Some(&Slide::Pic(pic("b2"))));
    }

    #[test]
    fn a_post_that_left_is_followed_by_the_one_in_its_place() {
        let l = list(&[("a", 1), ("b", 1), ("c", 1)]);
        let at = Spot::at(&l, 1, 0);
        let without_b = list(&[("a", 1), ("c", 1)]);
        assert_eq!(next(&without_b, &at).map(|s| s.id), Some("c".into()));
        assert_eq!(prev(&without_b, &at).map(|s| s.id), Some("a".into()));
        let only_a = list(&[("a", 1)]);
        assert!(next(&only_a, &at).is_none());
    }

    #[test]
    fn starts_at_the_focused_post_or_the_next_with_media() {
        let l = list(&[("a", 1), ("c", 1)]);
        let ids = ["a", "b", "c", "d"];
        assert_eq!(first(&l, &ids, None).map(|s| s.id), Some("a".into()));
        assert_eq!(first(&l, &ids, Some("c")).map(|s| s.id), Some("c".into()));
        // A text post focused: the next post with something to show.
        assert_eq!(first(&l, &ids, Some("b")).map(|s| s.id), Some("c".into()));
        // Nothing after it yet: wait for the next page.
        assert!(first(&l, &ids, Some("d")).is_none());
        assert!(first(&[], &ids, None).is_none());
    }

    #[test]
    fn how_long_each_slide_stays() {
        let p = Slide::Pic(pic("x"));
        let gif = Slide::Clip { url: "u".into(), poster: None, gif: true, secs: 0 };
        let vid = Slide::Clip { url: "u".into(), poster: None, gif: false, secs: 0 };
        assert_eq!(hold_ms(&p, 0, 8_000, 60_000), 8_000);
        // A short GIF loops for an interval; a long one plays through, up to the cap.
        assert_eq!(hold_ms(&gif, 3_000, 8_000, 60_000), 8_000);
        assert_eq!(hold_ms(&gif, 0, 8_000, 60_000), 8_000);
        assert_eq!(hold_ms(&gif, 20_000, 8_000, 60_000), 20_000);
        assert_eq!(hold_ms(&gif, 90_000, 8_000, 60_000), 60_000);
        assert_eq!(hold_ms(&gif, 90_000, 8_000, 0), 90_000);
        assert_eq!(hold_ms(&vid, 90_000, 8_000, 60_000), 60_000);
        assert_eq!(hold_ms(&vid, 90_000, 8_000, 0), u64::MAX);
        // Before the player reports a length, Reddit's is used.
        let known = Slide::Clip { url: "u".into(), poster: None, gif: true, secs: 12 };
        assert_eq!(length_ms(&known, 0), 12_000);
        assert_eq!(length_ms(&known, 11_500), 11_500);
        assert_eq!(hold_ms(&known, length_ms(&known, 0), 8_000, 60_000), 12_000);
        assert_eq!(length_ms(&p, 0), 0);
    }

    #[test]
    fn failed_requests_wait_longer_each_time() {
        assert_eq!(retry_wait(None, 0), 30_000);
        assert_eq!(retry_wait(None, 1), 60_000);
        assert_eq!(retry_wait(None, 4), 480_000);
        assert_eq!(retry_wait(None, 9), RETRY_MAX_MS);
        // A rate limit says how long.
        assert_eq!(retry_wait(Some(42), 3), 42_500);
    }

    #[test]
    fn idle_screen_shows_the_chosen_subreddit() {
        let mut p = Prefs::default();
        assert_eq!(idle_arg(&p), RdArg::Slideshow { key: FeedKey { feed: Feed::Sub("EarthPorn".into()), sort: Sort::Hot, time: Time::Day }, start: None, mode: ShowMode::Idle });
        p.idle_sub = Some("spaceporn".into());
        assert!(matches!(idle_arg(&p), RdArg::Slideshow { key: FeedKey { feed: Feed::Sub(s), .. }, .. } if s == "spaceporn"));
    }
}
