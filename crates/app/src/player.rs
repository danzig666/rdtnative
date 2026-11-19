//! Video playback: a stream URL (v.redd.it DASH or HLS, an MP4) handed to the TV's media service
//! (uMediaServer) with an SDL exported window as the video plane, as ytnative's engine A does. The
//! UI plane is cleared transparent over it while the player is on top.
//!
//! Threading: the exported window is created and destroyed on the main thread (SDL); every luna
//! call runs on a worker; media events update `Status`, which the player screen reads each frame.
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
}

#[cfg(not(any(windows, feature = "sim")))]
mod tv {
    use super::*;
    use crate::ls2::Luna;
    use std::ffi::{c_char, c_int, CStr, CString};

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

    pub struct Player {
        pub status: Arc<Mutex<Status>>,
        window: Option<CString>,
        media_id: Arc<Mutex<Option<String>>>,
        luna: Option<&'static Luna>,
        gen: Arc<std::sync::atomic::AtomicU64>,
    }

    impl Player {
        pub fn new() -> Self {
            Player { status: Default::default(), window: None, media_id: Default::default(), luna: None, gen: Default::default() }
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

        /// Main thread. Plays `url` full screen.
        pub fn start(&mut self, url: String, looping: bool) {
            self.stop();
            let my_gen = self.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let gen = self.gen.clone();
            *self.status.lock().unwrap() = Status { phase: Phase::Loading, url: url.clone(), looping, ..Default::default() };
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
            let (status, media_id) = (self.status.clone(), self.media_id.clone());
            let spawned = plx_base::task::spawn("play", move || {
                let live = || gen.load(std::sync::atomic::Ordering::SeqCst) == my_gen;
                let app_id = plx_base::paths::app_id();
                let load = format!(
                    r#"{{"uri":{},"type":"media","payload":{{"option":{{"appId":{},"windowId":{}}}}}}}"#,
                    json_str(&url),
                    json_str(app_id),
                    json_str(&window_id)
                );
                let r = luna.call("luna://com.webos.media/load", &load);
                crate::log(&format!("player: load {url} -> {r}"));
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
                        on_event(&st, msg, luna, &me);
                    }
                });
                luna.call("luna://com.webos.media/notifyForeground", &format!(r#"{{"connectionId":{}}}"#, json_str(&mid)));
                let r = luna.call("luna://com.webos.media/play", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
                crate::log(&format!("player: play -> {r}"));
                plx_machine::present::wake_from_worker();
            });
            if spawned.is_none() {
                self.fail("could not start the player thread");
            }
        }

        pub fn toggle_pause(&mut self) {
            let Some(luna) = self.luna else { return };
            let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
            let phase = self.status.lock().unwrap().phase.clone();
            let method = if matches!(phase, Phase::Paused | Phase::Ended) { "play" } else { "pause" };
            let payload = format!(r#"{{"mediaId":{}}}"#, json_str(&mid));
            let _ = plx_base::task::spawn("media", move || {
                luna.call(&format!("luna://com.webos.media/{method}"), &payload);
            });
        }

        pub fn seek_by(&mut self, delta_ms: i64) {
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

        pub fn tick(&mut self) {}
    }

    fn on_event(status: &Mutex<Status>, msg: &str, luna: &'static Luna, mid: &str) {
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
                let q = format!(r#"{{"mediaId":{}}}"#, json_str(mid));
                let _ = plx_base::task::spawn("media", move || {
                    luna.call("luna://com.webos.media/seek", &p);
                    luna.call("luna://com.webos.media/play", &q);
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
            s.phase = Phase::Failed(e["errorText"].as_str().unwrap_or("playback error").to_string());
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

    pub struct Player {
        pub status: Arc<Mutex<Status>>,
        started: Option<std::time::Instant>,
        base_ms: u64,
        active: bool,
    }

    impl Player {
        pub fn new() -> Self {
            Player { status: Default::default(), started: None, base_ms: 0, active: false }
        }
        pub fn active(&self) -> bool {
            self.active
        }
        pub fn start(&mut self, url: String, looping: bool) {
            *self.status.lock().unwrap() = Status { phase: Phase::Playing, url, looping, duration_ms: 60_000, width: 1280, height: 720, ..Default::default() };
            self.started = Some(std::time::Instant::now());
            self.base_ms = 0;
            self.active = true;
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
        pub fn seek_by(&mut self, delta_ms: i64) {
            let mut s = self.status.lock().unwrap();
            let to = (s.position_ms as i64 + delta_ms).clamp(0, s.duration_ms as i64) as u64;
            s.position_ms = to;
            self.base_ms = to;
            if self.started.is_some() {
                self.started = Some(std::time::Instant::now());
            }
        }
        pub fn stop(&mut self) {
            self.active = false;
            self.started = None;
            self.status.lock().unwrap().phase = Phase::Idle;
        }
        pub fn tick(&mut self) {
            if let Some(t) = self.started {
                let mut s = self.status.lock().unwrap();
                s.position_ms = (self.base_ms + t.elapsed().as_millis() as u64).min(s.duration_ms);
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

/// The stream to play for a Reddit video, by the "Video stream" setting: HLS (video and sound; the
/// default, as Reddit's other clients play it), DASH, or the plain MP4 (no sound on v.redd.it).
pub fn pick_url(v: &rd_core::media::Video, pref: &str) -> Option<String> {
    let order: [&Option<String>; 3] = match pref {
        "dash" => [&v.dash, &v.hls, &v.mp4],
        "mp4" => [&v.mp4, &v.hls, &v.dash],
        _ => [&v.hls, &v.dash, &v.mp4],
    };
    order.into_iter().flatten().next().cloned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn stream_preference() {
        let v = rd_core::media::Video { hls: Some("h".into()), dash: Some("d".into()), mp4: Some("m".into()), w: 0, h: 0, duration: 0, gif: false, poster: None };
        assert_eq!(super::pick_url(&v, "dash").as_deref(), Some("d"));
        assert_eq!(super::pick_url(&v, "hls").as_deref(), Some("h"));
        assert_eq!(super::pick_url(&v, "").as_deref(), Some("h"));
        assert_eq!(super::pick_url(&v, "hls").as_deref(), Some("h"));
        assert_eq!(super::pick_url(&v, "mp4").as_deref(), Some("m"));
        let only = rd_core::media::Video { hls: None, dash: None, ..v };
        assert_eq!(super::pick_url(&only, "dash").as_deref(), Some("m"));
    }
}

/// Opens YouTube video `id` in another app: ytnative when it is installed (it takes `v`), else
/// LG's YouTube app (`contentTarget`). Blocking (a worker calls it); `Err` when neither launched.
#[cfg(not(any(windows, feature = "sim")))]
pub fn launch_youtube(id: &str) -> Result<&'static str, String> {
    let luna = crate::ls2::Luna::shared(plx_base::paths::app_id())?;
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
