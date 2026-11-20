//! Video playback: a stream URL (v.redd.it DASH or HLS, an MP4) handed to the TV's media service
//! (uMediaServer) with an SDL exported window as the video plane, as ytnative's engine A does. The
//! UI plane is cleared transparent over it while the player is on top.
//!
//! Threading: the exported window is created and destroyed on the main thread (SDL); every luna
//! call runs on a worker; media events update `Status`, which the player screen reads each frame.
//!
//! Speed, loop and a quality cap change while a video plays. The media service has no size or
//! bitrate limit, so a capped HLS video plays from a copy of its master playlist reduced to the
//! cap, written to a local file. When the TV takes the pipeline away after the video played
//! (another app or the screensaver wanted the decoder), the video is loaded again where it was on
//! the viewer's next play, seek or replay: started while the decoder is still taken, it would only
//! be taken again.
//!
//! The Windows simulator has no media pipeline: its player runs a clock so the screen and its
//! controls can be tried.

use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

/// Playback speed, 1.0 = normal (its own type so that a default `Status` plays at normal speed).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate(pub f32);

impl Default for Rate {
    fn default() -> Self {
        Rate(1.0)
    }
}

/// The speeds the player offers.
pub const SPEEDS: [f32; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub phase: Phase,
    pub url: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// Loops silently (GIFs).
    pub looping: bool,
    /// Set when the stream failed before playing and other addresses are left (one per line):
    /// the main thread starts them on its next tick.
    pub retry: Option<String>,
    pub rate: Rate,
    /// HLS: the heights the stream offers, highest first (the quality choice).
    pub heights: Vec<u32>,
    /// The highest quality this video plays at (an HLS height); 0 = Auto (the TV chooses).
    pub max_height: u32,
    /// The TV took the pipeline away after the video played: the position it is loaded again at
    /// on the viewer's next play, seek or replay.
    pub reload: Option<u64>,
    /// A message for the viewer (a speed the TV refused), shown once as a toast.
    pub notice: Option<String>,
    /// Sound off (the video channel's switch); every new video starts with sound.
    pub muted: bool,
}

/// What the viewer asks of the playing video, beside pause and seek.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    /// The remote's play key: resumes, or starts again once the video ended.
    Play,
    /// The remote's pause key.
    Pause,
    /// Playback speed, 1.0 = normal.
    Rate(f32),
    /// This video's highest quality (an HLS height; 0 = Auto): it loads again where it is.
    Quality(u32),
    Loop(bool),
    /// From the start.
    Replay,
}

#[cfg(not(any(windows, feature = "sim")))]
mod tv {
    use super::*;
    use crate::ls2::Luna;
    use std::ffi::{c_char, c_int, CStr, CString};

    /// The reduced master playlist a capped HLS video plays from.
    const LOCAL_M3U8: &str = "/tmp/rdtnative/play.m3u8";
    const RATE_REFUSED: &str = "The TV refused this speed: playing at normal speed";
    const TAKEN_AWAY: &str = "The TV stopped the video: OK continues where it was";

    #[repr(C)]
    struct SdlRect {
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
    }
    type CreateFn = unsafe extern "C" fn(c_int) -> *const c_char;
    type SetFn = unsafe extern "C" fn(*const c_char, *mut SdlRect, *mut SdlRect) -> c_int;
    type DestroyFn = unsafe extern "C" fn(*const c_char);

    fn sdl_sym<T>(name: &str) -> Option<T> {
        let c = CString::new(name).ok()?;
        let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
        (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
    }

    fn json_str(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
    }

    fn media_json(mid: &str) -> String {
        format!(r#"{{"mediaId":{}}}"#, json_str(mid))
    }

    /// setPlayRate's payload: the speed and the sound (`muted`: the video channel's switch).
    fn rate_json(mid: &str, rate: f32, muted: bool) -> String {
        format!(r#"{{"mediaId":{},"playRate":{rate},"audioOutput":{}}}"#, json_str(mid), !muted)
    }

    pub struct Player {
        pub status: Arc<Mutex<Status>>,
        window: Option<CString>,
        media_id: Arc<Mutex<Option<String>>>,
        luna: Option<&'static Luna>,
        gen: Arc<std::sync::atomic::AtomicU64>,
        /// The quality cap loads play at (an HLS height; 0 = Auto).
        cap: u32,
    }

    impl Player {
        pub fn new() -> Self {
            Player { status: Default::default(), window: None, media_id: Default::default(), luna: None, gen: Default::default(), cap: 0 }
        }

        pub fn active(&self) -> bool {
            self.window.is_some()
        }

        fn bus(&mut self) -> Option<&'static Luna> {
            if self.luna.is_none() {
                match Luna::shared(plx_base::paths::app_id()) {
                    Ok(l) => self.luna = Some(l),
                    Err(e) => crate::log(&format!("player: LS2 unavailable: {e}")),
                }
            }
            self.luna
        }

        fn fail(&mut self, why: &str) {
            crate::log(&format!("player: {why}"));
            self.status.lock().unwrap().phase = Phase::Failed(why.to_string());
        }

        /// The quality the next videos play at (the Video quality setting): an HLS height, 0 = Auto.
        pub fn set_quality_cap(&mut self, h: u32) {
            self.cap = h;
        }

        /// Main thread. Plays `url` full screen.
        pub fn start(&mut self, url: String, looping: bool) {
            self.load(url, looping, 0, 1.0);
        }

        /// Main thread. Plays `url` (addresses, one per line) full screen from `start_ms`, at speed
        /// `rate` and the current quality cap.
        fn load(&mut self, url: String, looping: bool, start_ms: u64, rate: f32) {
            // A reload of the same stream keeps its quality choice on screen while it loads.
            let heights = {
                let s = self.status.lock().unwrap();
                if s.url == url { s.heights.clone() } else { Vec::new() }
            };
            self.stop();
            let my_gen = self.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let gen = self.gen.clone();
            *self.status.lock().unwrap() =
                Status { phase: Phase::Loading, url: url.clone(), looping, position_ms: start_ms, rate: Rate(rate), heights, max_height: self.cap, ..Default::default() };
            let Some(create) = sdl_sym::<CreateFn>("SDL_webOSCreateExportedWindow") else {
                self.fail("this TV has no video window for native apps (webOS 4 is not supported yet)");
                return;
            };
            let wid = unsafe { create(0) };
            if wid.is_null() {
                self.fail("could not create the video window");
                return;
            }
            let wid = unsafe { CStr::from_ptr(wid) }.to_owned();
            if let Some(set) = sdl_sym::<SetFn>("SDL_webOSSetExportedWindow") {
                let mut src = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
                let mut dst = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
                unsafe { set(wid.as_ptr(), &mut src, &mut dst) };
            }
            let window_id = wid.to_string_lossy().into_owned();
            self.window = Some(wid);
            let Some(luna) = self.bus() else {
                self.fail("media bus unavailable");
                return;
            };
            let (status, media_id, cap) = (self.status.clone(), self.media_id.clone(), self.cap);
            let spawned = plx_base::task::spawn("play", move || {
                let live = || gen.load(std::sync::atomic::Ordering::SeqCst) == my_gen;
                let all = candidates(&url);
                let url = first_served(&all);
                // The probe can pass an address the TV's player is then refused (v.redd.it answers
                // 403 to it): the ones after it are tried if the stream fails before it plays.
                let rest: Vec<&str> = all.iter().skip_while(|u| **u != url).skip(1).copied().collect();
                let rest = Arc::new(Mutex::new((!rest.is_empty()).then(|| rest.join("\n"))));
                if !live() {
                    return;
                }
                status.lock().unwrap().url = url.clone();
                let uri = hls_source(&url, cap, &status, &live);
                if !live() {
                    return;
                }
                let app_id = plx_base::paths::app_id();
                let load = format!(
                    r#"{{"uri":{},"type":"media","payload":{{"option":{{"appId":{},"windowId":{}}}}}}}"#,
                    json_str(&uri),
                    json_str(app_id),
                    json_str(&window_id)
                );
                let r = luna.call("luna://com.webos.media/load", &load);
                crate::log(&format!("player: load {uri} -> {r}"));
                let v: serde_json::Value = serde_json::from_str(&r).unwrap_or_default();
                let Some(mid) = v["mediaId"].as_str().map(str::to_string) else {
                    if live() {
                        status.lock().unwrap().phase = Phase::Failed(format!("the TV's player refused the video: {r}"));
                    }
                    plx_machine::present::wake_from_worker();
                    return;
                };
                if !live() {
                    luna.call("luna://com.webos.media/unload", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
                    return;
                }
                *media_id.lock().unwrap() = Some(mid.clone());
                let (st, current, me) = (status.clone(), media_id.clone(), mid.clone());
                luna.subscribe("luna://com.webos.media/subscribe", &format!(r#"{{"mediaId":{},"subscribe":true}}"#, json_str(&mid)), move |msg| {
                    if current.lock().unwrap().as_deref() == Some(me.as_str()) {
                        on_event(&st, &current, msg, luna, &me, &rest);
                    }
                });
                luna.call("luna://com.webos.media/notifyForeground", &format!(r#"{{"connectionId":{}}}"#, json_str(&mid)));
                let r = luna.call("luna://com.webos.media/play", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
                crate::log(&format!("player: play -> {r}"));
                plx_machine::present::wake_from_worker();
                let rate = status.lock().unwrap().rate.0;
                if start_ms <= 2000 && rate == 1.0 {
                    return;
                }
                // A seek (or a speed) sent right after play is answered and ignored (ytnative saw a
                // resumed video start from 0:00): wait until the pipeline plays.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                while live() && status.lock().unwrap().phase == Phase::Loading && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if !live() || matches!(status.lock().unwrap().phase, Phase::Failed(_)) {
                    return;
                }
                if start_ms > 2000 {
                    let seek = |why: &str| {
                        let r = luna.call("luna://com.webos.media/seek", &format!(r#"{{"mediaId":{},"position":{start_ms}}}"#, json_str(&mid)));
                        crate::log(&format!("player: start at {start_ms} ms ({why}) -> {r}"));
                    };
                    seek("playing");
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if live() && status.lock().unwrap().position_ms + 5_000 < start_ms {
                        seek("again");
                    }
                }
                let rate = status.lock().unwrap().rate.0;
                if live() && rate != 1.0 {
                    apply_rate(luna, &mid, rate, &status, &media_id);
                }
            });
            if spawned.is_none() {
                self.fail("could not start the player thread");
            }
        }

        /// After the TV took the pipeline away: loads the video again at `at` (else where it was).
        /// False while the pipeline is there.
        fn reload_lost(&mut self, at: Option<u64>) -> bool {
            let (url, looping, rate, from) = {
                let s = self.status.lock().unwrap();
                let Some(from) = s.reload else { return false };
                (s.url.clone(), s.looping, s.rate.0, from)
            };
            let at = at.unwrap_or(from);
            crate::log(&format!("player: loading the video again at {at} ms"));
            self.load(url, looping, at, rate);
            true
        }

        pub fn toggle_pause(&mut self) {
            if self.reload_lost(None) {
                return;
            }
            let Some(luna) = self.luna else { return };
            let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
            let (phase, rate) = {
                let s = self.status.lock().unwrap();
                (s.phase.clone(), s.rate.0)
            };
            let (status, current) = (self.status.clone(), self.media_id.clone());
            let _ = plx_base::task::spawn("media", move || {
                if matches!(phase, Phase::Paused | Phase::Ended) {
                    resume(luna, &mid, rate, &status, &current);
                } else {
                    luna.call("luna://com.webos.media/pause", &media_json(&mid));
                }
            });
        }

        /// From the start: seek to 0, then play.
        pub fn replay(&mut self) {
            if self.reload_lost(Some(0)) {
                return;
            }
            let Some(luna) = self.luna else { return };
            let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
            let rate = {
                let mut s = self.status.lock().unwrap();
                s.position_ms = 0;
                s.rate.0
            };
            let (status, current) = (self.status.clone(), self.media_id.clone());
            let _ = plx_base::task::spawn("media", move || {
                luna.call("luna://com.webos.media/seek", &format!(r#"{{"mediaId":{},"position":0}}"#, json_str(&mid)));
                resume(luna, &mid, rate, &status, &current);
            });
        }

        /// Playback speed (0.25..4) through uMediaServer's setPlayRate. A reload of this video
        /// keeps it, and every resume asks for it again.
        pub fn set_rate(&mut self, rate: f32) {
            let rate = rate.clamp(0.25, 4.0);
            let lost = {
                let mut s = self.status.lock().unwrap();
                s.rate = Rate(rate);
                s.reload.is_some()
            };
            let Some(luna) = self.luna else { return };
            // Still loading, or taken away: the load asks for it once the video plays.
            let Some(mid) = self.media_id.lock().unwrap().clone().filter(|_| !lost) else { return };
            let (status, current) = (self.status.clone(), self.media_id.clone());
            let _ = plx_base::task::spawn("media", move || apply_rate(luna, &mid, rate, &status, &current));
        }

        /// This video's highest quality (an HLS height; 0 = Auto): it loads again where it is.
        pub fn set_quality(&mut self, h: u32) {
            self.cap = h;
            let (url, looping, at, rate, phase) = {
                let s = self.status.lock().unwrap();
                (s.url.clone(), s.looping, s.reload.unwrap_or(s.position_ms), s.rate.0, s.phase.clone())
            };
            if url.is_empty() || matches!(phase, Phase::Idle | Phase::Failed(_)) {
                return;
            }
            crate::log(&format!("player: quality {h}p, again at {at} ms"));
            self.load(url, looping, at, rate);
        }

        pub fn seek_by(&mut self, delta_ms: i64) {
            let lost = {
                let s = self.status.lock().unwrap();
                let dur = s.duration_ms as i64;
                s.reload.map(|at| {
                    let to = (at as i64 + delta_ms).max(0);
                    (if dur > 0 { to.min((dur - 500).max(0)) } else { to }) as u64
                })
            };
            if let Some(to) = lost {
                self.reload_lost(Some(to));
                return;
            }
            let Some(luna) = self.luna else { return };
            let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
            let to = {
                let mut s = self.status.lock().unwrap();
                let dur = s.duration_ms as i64;
                let to = (s.position_ms as i64 + delta_ms).max(0);
                let to = if dur > 0 { to.min((dur - 500).max(0)) } else { to };
                s.position_ms = to as u64;
                to
            };
            let payload = format!(r#"{{"mediaId":{},"position":{to}}}"#, json_str(&mid));
            let _ = plx_base::task::spawn("media", move || {
                luna.call("luna://com.webos.media/seek", &payload);
            });
        }

        /// Sound off or on for the playing video: the `audioOutput` flag of uMediaServer's
        /// `setPlayRate`, at the video's speed. Per pipeline: a new video starts with sound.
        pub fn set_muted(&mut self, on: bool) {
            let Some(luna) = self.luna else { return };
            let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
            let rate = {
                let mut s = self.status.lock().unwrap();
                s.muted = on;
                s.rate.0
            };
            let payload = rate_json(&mid, rate, on);
            let _ = plx_base::task::spawn("media", move || {
                let r = luna.call("luna://com.webos.media/setPlayRate", &payload);
                crate::log(&format!("player: setPlayRate audioOutput {} -> {r}", !on));
            });
        }

        /// Main thread. Unloads the pipeline and destroys the video window.
        pub fn stop(&mut self) {
            self.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mid = self.media_id.lock().unwrap().take();
            if let (Some(luna), Some(mid)) = (self.luna, mid) {
                // Synchronous: the window must outlive the pipeline that renders into it.
                let r = luna.call("luna://com.webos.media/unload", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
                crate::log(&format!("player: unload -> {r}"));
            }
            if let Some(w) = self.window.take() {
                if let Some(destroy) = sdl_sym::<DestroyFn>("SDL_webOSDestroyExportedWindow") {
                    unsafe { destroy(w.as_ptr()) };
                }
            }
            self.status.lock().unwrap().phase = Phase::Idle;
        }

        pub fn tick(&mut self) {
            let retry = {
                let mut s = self.status.lock().unwrap();
                s.retry.take().map(|r| (r, s.looping))
            };
            if let Some((urls, looping)) = retry {
                crate::log("player: stream refused, trying the next one");
                self.start(urls, looping);
            }
        }
    }

    /// HLS: the master's heights become the quality choice, and under a cap the TV plays a copy
    /// reduced to it from a local file. Anything else, or a master that cannot be read, plays
    /// from `url` itself.
    fn hls_source(url: &str, cap: u32, status: &Mutex<Status>, live: &dyn Fn() -> bool) -> String {
        if !rd_core::hls::is_hls(url) {
            return url.to_string();
        }
        let master = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(6))
            .build()
            .and_then(|http| http.get(url).send())
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text());
        let master = match master {
            Ok(m) => m,
            Err(e) => {
                crate::log(&format!("player: HLS master: {e}"));
                return url.to_string();
            }
        };
        let heights = rd_core::hls::heights(&master);
        crate::log(&format!("player: HLS heights {heights:?}"));
        if !live() {
            return url.to_string();
        }
        status.lock().unwrap().heights = heights;
        if cap == 0 {
            return url.to_string();
        }
        let path = std::path::Path::new(LOCAL_M3U8);
        let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(path, rd_core::hls::filter(&master, url, cap)));
        match written {
            Ok(()) => {
                crate::log(&format!("player: HLS up to {cap}p from {LOCAL_M3U8}"));
                format!("file://{LOCAL_M3U8}")
            }
            Err(e) => {
                crate::log(&format!("player: {LOCAL_M3U8}: {e}"));
                url.to_string()
            }
        }
    }

    /// Worker. Plays `mid` (again), then asks for the speed again when it is not normal: a
    /// pipeline may drop it on resume.
    fn resume(luna: &'static Luna, mid: &str, rate: f32, status: &Mutex<Status>, current: &Mutex<Option<String>>) {
        luna.call("luna://com.webos.media/play", &media_json(mid));
        if rate != 1.0 {
            apply_rate(luna, mid, rate, status, current);
        }
    }

    /// Worker. Asks pipeline `mid` for speed `rate`. When the TV refuses it, the video goes back
    /// to normal speed and the viewer is told.
    fn apply_rate(luna: &'static Luna, mid: &str, rate: f32, status: &Mutex<Status>, current: &Mutex<Option<String>>) {
        // The same call carries the sound: keep the video channel's switch.
        let muted = status.lock().unwrap().muted;
        let r = luna.call("luna://com.webos.media/setPlayRate", &rate_json(mid, rate, muted));
        crate::log(&format!("player: setPlayRate {rate} -> {r}"));
        if replied_ok(&r) || rate == 1.0 || current.lock().unwrap().as_deref() != Some(mid) {
            return;
        }
        let r = luna.call("luna://com.webos.media/setPlayRate", &rate_json(mid, 1.0, muted));
        crate::log(&format!("player: setPlayRate 1 -> {r}"));
        let mut s = status.lock().unwrap();
        s.rate = Rate(1.0);
        s.notice = Some(RATE_REFUSED.into());
        drop(s);
        plx_machine::present::wake_from_worker();
    }

    fn on_event(status: &Arc<Mutex<Status>>, current: &Arc<Mutex<Option<String>>>, msg: &str, luna: &'static Luna, mid: &str, rest: &Mutex<Option<String>>) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else { return };
        let mut s = status.lock().unwrap();
        if let Some(t) = v["currentTime"]["currentTime"].as_u64() {
            s.position_ms = t;
        } else if v.get("playing").is_some() {
            s.phase = Phase::Playing;
        } else if v.get("paused").is_some() {
            s.phase = Phase::Paused;
        } else if v.get("endOfStream").is_some() {
            if s.looping {
                // GIFs loop: back to the start.
                s.position_ms = 0;
                let p = format!(r#"{{"mediaId":{},"position":0}}"#, json_str(mid));
                let (rate, status, current, mid) = (s.rate.0, status.clone(), current.clone(), mid.to_string());
                let _ = plx_base::task::spawn("media", move || {
                    luna.call("luna://com.webos.media/seek", &p);
                    resume(luna, &mid, rate, &status, &current);
                });
            } else {
                s.phase = Phase::Ended;
            }
        } else if let Some(vi) = v.get("videoInfo") {
            s.width = vi["width"].as_u64().unwrap_or(0) as u32;
            s.height = vi["height"].as_u64().unwrap_or(0) as u32;
        } else if let Some(d) = v["sourceInfo"]["programInfo"][0]["duration"].as_u64() {
            s.duration_ms = d;
        } else if let Some(e) = v.get("error") {
            crate::log(&format!("player: media error {e}"));
            if s.phase == Phase::Loading && s.position_ms == 0 {
                if let Some(next) = rest.lock().unwrap().take() {
                    s.retry = Some(next);
                    drop(s);
                    plx_machine::present::wake_from_worker();
                    return;
                }
            }
            if s.retry.is_some() || s.reload.is_some() {
                return;
            }
            match after_loss(&s.phase) {
                Some(phase) => {
                    // Another app or the screensaver took the decoder: kept here, loaded again
                    // when the viewer goes on. A reload that fails before it plays fails for good.
                    crate::log(&format!("player: the TV took the video away at {} ms", s.position_ms));
                    s.phase = phase;
                    s.reload = Some(s.position_ms);
                    s.notice = Some(TAKEN_AWAY.into());
                }
                None => s.phase = Phase::Failed(e["errorText"].as_str().unwrap_or("playback error").to_string()),
            }
        } else {
            return;
        }
        drop(s);
        plx_machine::present::wake_from_worker();
    }
}

/// The simulator's player: no picture, a running clock.
#[cfg(any(windows, feature = "sim"))]
mod sim {
    use super::*;

    /// The heights the simulator pretends an HLS stream offers.
    const LADDER: [u32; 3] = [720, 480, 360];

    pub struct Player {
        pub status: Arc<Mutex<Status>>,
        started: Option<std::time::Instant>,
        base_ms: u64,
        active: bool,
        cap: u32,
    }

    impl Player {
        pub fn new() -> Self {
            Player { status: Default::default(), started: None, base_ms: 0, active: false, cap: 0 }
        }
        pub fn active(&self) -> bool {
            self.active
        }
        pub fn set_quality_cap(&mut self, h: u32) {
            self.cap = h;
        }
        pub fn start(&mut self, url: String, looping: bool) {
            let url = candidates(&url).first().map_or_else(String::new, |u| u.to_string());
            let heights = if rd_core::hls::is_hls(&url) { LADDER.to_vec() } else { Vec::new() };
            *self.status.lock().unwrap() = Status { phase: Phase::Playing, url, looping, duration_ms: 60_000, heights, ..Default::default() };
            self.set_quality(self.cap);
            self.started = Some(std::time::Instant::now());
            self.base_ms = 0;
            self.active = true;
        }
        /// The clock's position now.
        fn clock(&self, s: &Status) -> u64 {
            let ran = self.started.map_or(0, |t| (t.elapsed().as_millis() as f32 * s.rate.0) as u64);
            (self.base_ms + ran).min(s.duration_ms)
        }
        pub fn toggle_pause(&mut self) {
            let mut s = self.status.lock().unwrap();
            if s.phase == Phase::Playing {
                s.phase = Phase::Paused;
                self.base_ms = s.position_ms;
                self.started = None;
            } else {
                s.phase = Phase::Playing;
                self.started = Some(std::time::Instant::now());
            }
        }
        pub fn replay(&mut self) {
            let mut s = self.status.lock().unwrap();
            s.position_ms = 0;
            s.phase = Phase::Playing;
            self.base_ms = 0;
            self.started = Some(std::time::Instant::now());
        }
        pub fn set_rate(&mut self, rate: f32) {
            let mut s = self.status.lock().unwrap();
            self.base_ms = self.clock(&s);
            if self.started.is_some() {
                self.started = Some(std::time::Instant::now());
            }
            s.rate = Rate(rate.clamp(0.25, 4.0));
        }
        /// Plays the highest rung within the cap, as a filtered HLS master would.
        pub fn set_quality(&mut self, h: u32) {
            self.cap = h;
            let mut s = self.status.lock().unwrap();
            s.max_height = h;
            s.height = LADDER.into_iter().find(|l| h == 0 || *l <= h).unwrap_or(LADDER[0]);
            s.width = s.height * 16 / 9;
        }
        pub fn seek_by(&mut self, delta_ms: i64) {
            let mut s = self.status.lock().unwrap();
            let to = (s.position_ms as i64 + delta_ms).clamp(0, s.duration_ms as i64) as u64;
            s.position_ms = to;
            self.base_ms = to;
            if self.started.is_some() {
                self.started = Some(std::time::Instant::now());
            }
        }
        /// No sound to switch here: only the flag.
        pub fn set_muted(&mut self, on: bool) {
            self.status.lock().unwrap().muted = on;
        }
        pub fn stop(&mut self) {
            self.active = false;
            self.started = None;
            self.status.lock().unwrap().phase = Phase::Idle;
        }
        pub fn tick(&mut self) {
            if self.started.is_some() {
                let mut s = self.status.lock().unwrap();
                s.position_ms = self.clock(&s);
                if s.position_ms >= s.duration_ms {
                    if s.looping {
                        self.base_ms = 0;
                        self.started = Some(std::time::Instant::now());
                    } else {
                        s.phase = Phase::Ended;
                        self.started = None;
                    }
                }
            }
        }
    }
}

#[cfg(any(windows, feature = "sim"))]
pub use sim::Player;
#[cfg(not(any(windows, feature = "sim")))]
pub use tv::Player;

impl Player {
    /// What the viewer asked for (see [`Control`]).
    pub fn control(&mut self, c: Control) {
        match c {
            Control::Play => self.play(),
            Control::Pause => self.pause(),
            Control::Rate(r) => self.set_rate(r),
            Control::Quality(h) => self.set_quality(h),
            Control::Loop(on) => self.set_looping(on),
            Control::Replay => self.replay(),
        }
    }

    /// Resumes a paused video, starts an ended one again.
    pub fn play(&mut self) {
        let phase = self.status.lock().unwrap().phase.clone();
        match phase {
            Phase::Paused => self.toggle_pause(),
            Phase::Ended => self.replay(),
            _ => {}
        }
    }

    pub fn pause(&mut self) {
        let playing = self.status.lock().unwrap().phase == Phase::Playing;
        if playing {
            self.toggle_pause();
        }
    }

    /// Loops or plays once. Read at every end of the stream, so it changes while the video plays.
    pub fn set_looping(&mut self, on: bool) {
        self.status.lock().unwrap().looping = on;
    }
}

/// The quality cap a "Video quality" setting value stands for: its height, 0 for Auto.
pub fn quality_cap(pref: &str) -> u32 {
    pref.parse().unwrap_or(0)
}

/// What an error event leaves a video in `phase` showing: when it had played, the TV took the
/// pipeline away (another app or the screensaver wanted the decoder) and the video waits to be
/// loaded again; `None` when it never played (a real failure).
#[cfg(any(test, not(any(windows, feature = "sim"))))]
fn after_loss(phase: &Phase) -> Option<Phase> {
    match phase {
        Phase::Playing | Phase::Paused => Some(Phase::Paused),
        Phase::Ended => Some(Phase::Ended),
        _ => None,
    }
}

/// Whether a luna reply says the call went through.
#[cfg(any(test, not(any(windows, feature = "sim"))))]
fn replied_ok(reply: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(reply).is_ok_and(|v| v["returnValue"].as_bool() == Some(true))
}

/// The stream to play for a Reddit video, by the "Video stream" setting: HLS (video and sound; the
/// default, as Reddit's other clients play it), DASH, or the plain MP4 (no sound on v.redd.it).
/// The video's addresses in the preferred order, one per line: the player plays the first one
/// the server serves (Reddit refuses the HLS playlist of some videos and the DASH one of others).
pub fn pick_url(v: &rd_core::media::Video, pref: &str) -> Option<String> {
    let order: [&Option<String>; 3] = match pref {
        "dash" => [&v.dash, &v.hls, &v.mp4],
        "mp4" => [&v.mp4, &v.hls, &v.dash],
        _ => [&v.hls, &v.dash, &v.mp4],
    };
    let all: Vec<&str> = order.into_iter().flatten().map(String::as_str).collect();
    (!all.is_empty()).then(|| all.join("\n"))
}

/// The candidates `pick_url` packed (one address per line).
pub fn candidates(urls: &str) -> Vec<&str> {
    urls.lines().map(str::trim).filter(|u| !u.is_empty()).collect()
}

/// The first candidate the server serves (asked for one byte; a 403 or 404 moves on). Without an
/// answer from any (no network, all refused), the first one: the TV's player says why.
#[cfg(not(any(windows, feature = "sim")))]
fn first_served(urls: &[&str]) -> String {
    if urls.len() > 1 {
        if let Ok(http) = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(6)).build() {
            for u in urls {
                match http.get(*u).header("Range", "bytes=0-0").send() {
                    Ok(r) if r.status().is_success() => return u.to_string(),
                    Ok(r) => crate::log(&format!("player: {} for {}", r.status().as_u16(), u)),
                    Err(e) => crate::log(&format!("player: {e} for {}", u)),
                }
            }
        }
    }
    urls.first().map_or_else(String::new, |u| u.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_preference() {
        let v = rd_core::media::Video { hls: Some("h".into()), dash: Some("d".into()), mp4: Some("m".into()), w: 0, h: 0, duration: 0, gif: false, poster: None };
        let first = |pref: &str| super::pick_url(&v, pref).map(|u| super::candidates(&u).join(","));
        assert_eq!(first("dash").as_deref(), Some("d,h,m"));
        assert_eq!(first("hls").as_deref(), Some("h,d,m"));
        assert_eq!(first("").as_deref(), Some("h,d,m"));
        assert_eq!(first("mp4").as_deref(), Some("m,h,d"));
        let only = rd_core::media::Video { hls: None, dash: None, ..v };
        assert_eq!(super::pick_url(&only, "dash").as_deref(), Some("m"));
        assert!(super::pick_url(&rd_core::media::Video { mp4: None, ..only }, "").is_none());
    }

    #[test]
    fn a_default_status_plays_at_normal_speed() {
        assert_eq!(Status::default().rate, Rate(1.0));
        assert!(SPEEDS.contains(&1.0));
        assert_eq!(quality_cap("auto"), 0);
        assert_eq!(quality_cap("720"), 720);
    }

    #[test]
    fn errors_after_playing_wait_for_a_reload() {
        assert_eq!(after_loss(&Phase::Playing), Some(Phase::Paused));
        assert_eq!(after_loss(&Phase::Paused), Some(Phase::Paused));
        assert_eq!(after_loss(&Phase::Ended), Some(Phase::Ended));
        // A load (or a reload) that fails before it plays is a failure.
        assert_eq!(after_loss(&Phase::Loading), None);
        assert_eq!(after_loss(&Phase::Idle), None);
    }

    #[test]
    fn luna_replies() {
        assert!(replied_ok(r#"{"returnValue":true,"errorCode":0}"#));
        assert!(replied_ok("{\n  \"returnValue\": true\n}"));
        assert!(!replied_ok(r#"{"returnValue":false,"errorText":"not supported"}"#));
        assert!(!replied_ok("<timeout>"));
    }

    #[cfg(any(windows, feature = "sim"))]
    #[test]
    fn simulator_controls() {
        let mut p = Player::new();
        p.set_quality_cap(480);
        p.start("https://v.redd.it/x/HLSPlaylist.m3u8\nhttps://v.redd.it/x/DASH_720.mp4".into(), false);
        let status = p.status.clone();
        let st = || status.lock().unwrap().clone();
        assert_eq!((st().heights, st().max_height, st().height), (vec![720, 480, 360], 480, 480));
        p.control(Control::Quality(0));
        assert_eq!((st().max_height, st().height), (0, 720));
        p.control(Control::Rate(2.0));
        assert_eq!(st().rate, Rate(2.0));
        p.control(Control::Loop(true));
        assert!(st().looping);
        p.control(Control::Pause);
        assert_eq!(st().phase, Phase::Paused);
        p.control(Control::Play);
        assert_eq!(st().phase, Phase::Playing);
        p.seek_by(70_000);
        p.tick();
        assert_eq!(st().position_ms, 60_000);
        p.status.lock().unwrap().phase = Phase::Ended;
        p.control(Control::Play);
        assert_eq!((st().phase, st().position_ms), (Phase::Playing, 0));
    }
}

/// Opens YouTube video `id` in another app: ytnative when it is installed (it takes `v`), else
/// LG's YouTube app (`contentTarget`). Blocking (a worker calls it); `Err` when neither launched.
#[cfg(not(any(windows, feature = "sim")))]
pub fn launch_youtube(id: &str) -> Result<&'static str, String> {
    // The media connection may not call the application manager: the wide one may.
    let luna = crate::ls2::Luna::wide(plx_base::paths::app_id())?;
    let tries: [(&'static str, String); 2] = [
        ("ytnative", format!(r#"{{"id":"io.github.danzig666.ytnative","params":{{"v":"{id}"}}}}"#)),
        ("YouTube", format!(r#"{{"id":"youtube.leanback.v4","params":{{"contentTarget":"https://www.youtube.com/tv?v={id}"}}}}"#)),
    ];
    let mut last = String::new();
    for (name, payload) in tries {
        let r = luna.call("luna://com.webos.applicationManager/launch", &payload);
        crate::log(&format!("youtube: launch {name} -> {r}"));
        if r.contains("\"returnValue\":true") {
            return Ok(name);
        }
        last = r;
    }
    Err(last)
}

#[cfg(any(windows, feature = "sim"))]
pub fn launch_youtube(_id: &str) -> Result<&'static str, String> {
    Err("the simulator cannot open other apps".into())
}

/// Opens `url` in the TV's web browser. Blocking (a worker calls it).
#[cfg(not(any(windows, feature = "sim")))]
pub fn launch_browser(url: &str) -> Result<(), String> {
    let luna = crate::ls2::Luna::wide(plx_base::paths::app_id())?;
    let payload = serde_json::json!({"id": "com.webos.app.browser", "params": {"target": url}}).to_string();
    let r = luna.call("luna://com.webos.applicationManager/launch", &payload);
    crate::log(&format!("browser: launch -> {r}"));
    if r.contains("\"returnValue\":true") { Ok(()) } else { Err(r) }
}

#[cfg(any(windows, feature = "sim"))]
pub fn launch_browser(_url: &str) -> Result<(), String> {
    Err("the simulator cannot open other apps".into())
}

/// Closes the TV's browser (a sign-in opened in it is done) and brings this app back.
#[cfg(not(any(windows, feature = "sim")))]
pub fn close_browser_and_return() {
    let Ok(luna) = crate::ls2::Luna::wide(plx_base::paths::app_id()) else { return };
    let r = luna.call("luna://com.webos.applicationManager/closeByAppId", r#"{"id":"com.webos.app.browser"}"#);
    crate::log(&format!("browser: close -> {r}"));
    let payload = serde_json::json!({"id": plx_base::paths::app_id()}).to_string();
    let r = luna.call("luna://com.webos.applicationManager/launch", &payload);
    crate::log(&format!("browser: back to the app -> {r}"));
}

#[cfg(any(windows, feature = "sim"))]
pub fn close_browser_and_return() {}
