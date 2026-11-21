//! Pictures: our `plx_ui::tex::Source`. A draw probes a URL; a miss interns it as a PosterKey and
//! queues a fetch; worker threads download + decode; the main thread hands decoded pixels to the
//! render cache (`tex::accept`) and uploads them under the frame budget (`tex::prepare`).
//!
//! Three classes, picked by plx's `srv` number: [`THUMB`] (feed tiles, scaled down to a tile's
//! size), [`FULL`] (the viewer and the post page, up to the screen's size) and [`BLUR`] (NSFW and
//! spoiler covers: shrunk to a few dozen pixels, so the GPU's stretch blurs them).

use plx_machine::machine::{PosterKey, PresentHandle};
use plx_machine::present::Present;
use plx_ui::frame::Budget;
use plx_ui::tex::{self, Decoded, PosterError, PosterReady, Tex, Uploader, Warm};
use plx_ui::{Painter, Rect};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};

const WORKERS: usize = 4;

pub const THUMB: u16 = 0;
pub const FULL: u16 = 1;
pub const BLUR: u16 = 2;

fn max_size(srv: u16) -> (u32, u32) {
    match srv {
        FULL => (1920, 2160),
        BLUR => (40, 40),
        _ => (960, 960),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Queued,
    Ready,
    Failed,
}

#[derive(Default)]
struct State {
    by_url: HashMap<(u16, String), (PosterKey, Status)>,
    queue: VecDeque<(PosterKey, u16, String)>,
    done: Vec<PosterReady<PosterKey>>,
    next: u32,
    started: bool,
}

pub struct Thumbs {
    st: Mutex<State>,
    cv: Condvar,
}

pub static THUMBS: std::sync::LazyLock<Thumbs> = std::sync::LazyLock::new(|| Thumbs { st: Mutex::new(State::default()), cv: Condvar::new() });

impl Thumbs {
    fn request(&self, srv: u16, url: &str) -> (PosterKey, Status, bool) {
        let mut st = self.st.lock().unwrap();
        if let Some(&(k, s)) = st.by_url.get(&(srv, url.to_string())) {
            return (k, s, false);
        }
        st.next += 1;
        let k = PosterKey(st.next);
        st.by_url.insert((srv, url.to_string()), (k, Status::Queued));
        // Full pictures are what the user is looking at: ahead of the feed's tiles.
        if srv == FULL {
            st.queue.push_front((k, srv, url.to_string()));
        } else {
            st.queue.push_back((k, srv, url.to_string()));
        }
        if !st.started {
            st.started = true;
            for i in 0..WORKERS {
                let _ = plx_base::task::spawn(if i == 0 { "thumb0" } else { "thumb" }, || (*THUMBS).work());
            }
        }
        drop(st);
        self.cv.notify_one();
        (k, Status::Queued, true)
    }

    fn work(&'static self) {
        let http = rd_core::session::http_client();
        loop {
            let (key, srv, url) = {
                let mut st = self.st.lock().unwrap();
                loop {
                    if let Some(j) = st.queue.pop_front() {
                        break j;
                    }
                    st = self.cv.wait(st).unwrap();
                }
            };
            let result = fetch(&http, &url, max_size(srv));
            let mut st = self.st.lock().unwrap();
            let ok = result.is_ok();
            if let Some(e) = st.by_url.get_mut(&(srv, url)) {
                e.1 = if ok { Status::Ready } else { Status::Failed };
            }
            st.done.push(PosterReady { key, result });
            drop(st);
            plx_machine::present::wake_from_worker();
        }
    }

    /// Main thread, every frame: hand finished decodes to the render cache.
    pub fn deliver(&self) {
        let done = std::mem::take(&mut self.st.lock().unwrap().done);
        for r in done {
            tex::accept(r);
        }
    }

    /// Pictures still queued or being fetched.
    pub fn pending(&self) -> usize {
        self.st.lock().unwrap().queue.len()
    }
}

fn fetch(http: &reqwest::blocking::Client, url: &str, max: (u32, u32)) -> Result<Decoded, PosterError> {
    let bytes = http
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (rdtnative)")
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|_| PosterError::Fetch)?;
    let (w, h, rgba) = plx_gfx::img::img_decode_owned(&bytes).ok_or(PosterError::Decode)?;
    let (w, h, rgba) = downscale(w, h, rgba, max);
    Ok(Decoded { w: w as u16, h: h as u16, rgba: rgba.into_boxed_slice() })
}

/// Box-filter downscale by an integer factor so the picture fits `max`.
fn downscale(w: u32, h: u32, rgba: Vec<u8>, max: (u32, u32)) -> (u32, u32, Vec<u8>) {
    let f = w.div_ceil(max.0).max(h.div_ceil(max.1)).max(1);
    if f == 1 || w < f || h < f {
        return (w, h, rgba);
    }
    let (nw, nh) = (w / f, h / f);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            for dy in 0..f {
                let row = ((y * f + dy) * w) as usize;
                for dx in 0..f {
                    let i = (row + (x * f + dx) as usize) * 4;
                    for c in 0..4 {
                        acc[c] += rgba[i + c] as u32;
                    }
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / (f * f)) as u8;
            }
        }
    }
    (nw, nh, out)
}

impl tex::Source for Thumbs {
    fn probe(&self, srv: u16, path: &str, _w: i32, _h: i32, _png: bool) -> Option<PosterKey> {
        if path.is_empty() {
            return None;
        }
        let (k, s, _) = self.request(srv, path);
        (s == Status::Ready).then_some(k)
    }
    fn warm(&self, srv: u16, path: &str, _w: i32, _h: i32, _png: bool) -> Warm {
        if path.is_empty() {
            return Warm::Known;
        }
        if self.request(srv, path).2 { Warm::Claimed } else { Warm::Known }
    }
    fn logo(&self, _srv: u16, _logo: &str) -> Option<PosterKey> {
        None
    }
    fn logo_warm(&self, _srv: u16, _logo: &str) -> Warm {
        Warm::Known
    }
    fn failed(&self, srv: u16, path: &str, _w: i32, _h: i32, _png: bool) -> bool {
        self.st.lock().unwrap().by_url.get(&(srv, path.to_string())).is_some_and(|e| e.1 == Status::Failed)
    }
    fn unresident(&self, key: PosterKey, _refused: bool) {
        // Evicted: forget it so a later draw fetches again. A failed fetch stays known as failed
        // (forgetting it would download it again on every draw, forever).
        self.st.lock().unwrap().by_url.retain(|_, v| v.0 != key || v.1 == Status::Failed);
    }
    fn idle(&self) -> bool {
        let st = self.st.lock().unwrap();
        st.queue.is_empty() && st.done.is_empty()
    }
}

/// The GL half of the cache (plx-native's `GfxUploader`).
pub struct GfxUploader;

impl Uploader for GfxUploader {
    fn upload(&mut self, d: &Decoded) -> Tex {
        let id = plx_gfx::img::img_upload_rgba(d.rgba.as_ptr(), d.w as i32, d.h as i32);
        Tex { id, w: d.w, h: d.h }
    }
    fn warm(&mut self, t: Tex) {
        if t.id != 0 {
            plx_gfx::gfx::warm_tex(t.id);
        }
    }
    fn free(&mut self, t: Tex) {
        if t.id != 0 {
            plx_gfx::gfx::delete_tex(t.id);
        }
    }
}

/// Called from the Rig's per-frame `prepare`.
pub fn prepare(b: &mut Budget, present: &mut Present) {
    tex::prepare(b, &mut GfxUploader, &mut PresentHandle(present), crate::now_us);
}

/// How a picture fills its box.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Fills the box, cropping what sticks out (tiles).
    Cover,
    /// Whole picture inside the box, centred (viewer, post page).
    Contain,
}

/// Where the picture landed, or why not.
pub enum Drawn {
    /// Drawn in this rect (inside the box for `Contain`).
    At(Rect),
    Loading,
    Failed,
}

/// Draws picture `url` of class `srv` in `r`. While it loads nothing is drawn; the caller draws a
/// placeholder underneath.
pub fn draw(p: Painter, srv: u16, url: &str, r: Rect, rad: f32, fit: Fit, tint: [f32; 4]) -> Drawn {
    if url.is_empty() {
        return Drawn::Failed;
    }
    let (id, w, h) = plx_ui::tex::resolve_wh_on(srv, url, r.w as i32, r.h as i32, false);
    if id == 0 {
        return if plx_ui::tex::failed_on(srv, url, r.w as i32, r.h as i32, false) {
            Drawn::Failed
        } else {
            crate::ui::animating();
            Drawn::Loading
        };
    }
    let (w, h) = (w.max(1.0), h.max(1.0));
    match fit {
        Fit::Cover => {
            let (ra, ia) = (r.w / r.h, w / h);
            // plx's UV window is (offset u, offset v, width, height).
            let uv = if ia > ra {
                let k = ra / ia;
                [(1.0 - k) / 2.0, 0.0, k, 1.0]
            } else {
                let k = ia / ra;
                // Keep the top of tall pictures (faces, headlines), not the middle.
                let top = ((1.0 - k) * 0.25).max(0.0);
                [0.0, top, 1.0, k]
            };
            p.tex_uv(id, uv, r, rad, tint);
            Drawn::At(r)
        }
        Fit::Contain => {
            let k = (r.w / w).min(r.h / h);
            let (dw, dh) = (w * k, h * k);
            let d = Rect::new(r.x + (r.w - dw) / 2.0, r.y + (r.h - dh) / 2.0, dw, dh);
            p.tex(id, d, rad, tint);
            Drawn::At(d)
        }
    }
}

/// Starts loading picture `url` of class `srv` for a `w` x `h` box without drawing it (the next
/// page of a gallery, so it is there when the user moves on).
pub fn warm(srv: u16, url: &str, w: f32, h: f32) {
    if !url.is_empty() {
        let _ = plx_ui::tex::warm_on(srv, url, w as i32, h as i32, false);
    }
}

/// The size of a loaded picture, if it is loaded.
pub fn size_of(srv: u16, url: &str) -> Option<(f32, f32)> {
    let (id, w, h) = plx_ui::tex::resolve_wh_on(srv, url, 0, 0, false);
    (id != 0).then_some((w, h))
}
