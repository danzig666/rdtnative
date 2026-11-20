//! rdtnative: native Reddit client for LG webOS.
//!
//! Boot and frame loop follow ytnative's `main.rs`, itself after plx-native's `app/boot.rs` +
//! `app/run.rs` (GPL-3.0-or-later): SDL + GLES2 window, the plx_ui dispatcher, Magic Remote
//! pointer, wheel and edge glide.

mod awake;
mod crashtrace; // fatal-signal tracer -> /tmp/rdtnative/crash.log (see `cargo xtask crash`)
mod edge;
mod flow;
mod host;
mod i18n;
mod images;
mod input;
#[cfg(not(any(windows, feature = "sim")))]
mod ls2;
mod player;
mod prefs;
// The desktop simulator (Windows, or Linux with `--features sim`) swaps the TV-only modules (Wayland surface, FIFO remote) for
// stand-ins under `sim/`; see `sim/mod.rs`.
#[cfg_attr(any(windows, feature = "sim"), path = "sim/remote.rs")]
mod remote;
mod richtext;
mod screens;
mod sdl;
#[cfg(any(windows, feature = "sim"))]
mod sim;
mod store;
mod ui;
mod update;
#[cfg_attr(any(windows, feature = "sim"), path = "sim/wl.rs")]
mod wl;

use host::{AppRig, RdArg, RdHost};
use plx_machine::machine::{MachineId, NavOp, Tick};
use plx_ui::containers::transition::PageDip;
use plx_ui::dispatch::{Dispatcher, NoTap};
use std::ffi::CString;
use std::time::Instant;

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Rows per second at the very edge for the pointer edge glide (0 = off), from the settings.
pub static EDGE_SPEED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(6);
/// Items one wheel notch moves.
pub static WHEEL_ROWS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

pub fn now_us() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

pub fn log(s: &str) {
    plx_base::eventlog::log(s);
    eprintln!("{s}");
}

/// The app's data (sign-in, settings) lives in /tmp, which a restart clears (Settings says so).
pub static DATA_TEMPORARY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The app is on screen (not behind the TV's home screen or another app): slideshows pause
/// meanwhile.
pub static FOREGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Persistent, writable, survives reinstall in the Developer Mode jail (ytnative's finding);
/// falls back to the app's tmp dir.
#[cfg(not(any(windows, feature = "sim")))]
fn data_dir() -> std::path::PathBuf {
    let id = plx_base::paths::app_id();
    // Rooted TVs: next to the apps. Developer Mode only: the app runs as its own jail user, which
    // cannot write there; the package ships a writable `data` folder in the app's own folder
    // instead. /tmp is the last resort: it is cleared at every restart.
    let own = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("data")));
    let candidates = [Some(std::path::PathBuf::from(format!("/media/developer/{id}-data"))), own, Some(std::path::PathBuf::from(format!("/media/internal/.{id}")))];
    for p in candidates.into_iter().flatten() {
        if std::fs::create_dir_all(&p).is_ok() && std::fs::write(p.join(".probe"), b"ok").is_ok() {
            log(&format!("data: {}", p.display()));
            return p;
        }
    }
    DATA_TEMPORARY.store(true, std::sync::atomic::Ordering::Relaxed);
    let p = std::path::PathBuf::from("/tmp/rdtnative/data");
    let p = if std::fs::create_dir_all(&p).is_ok() { p } else { std::path::PathBuf::from("/tmp") };
    log(&format!("data: {} (temporary: settings are lost at restart)", p.display()));
    p
}

/// Simulator: `%LOCALAPPDATA%\rdtnative` on Windows, `~/.local/share/rdtnative` on Linux.
#[cfg(any(windows, feature = "sim"))]
fn data_dir() -> std::path::PathBuf {
    let d = sim::base_dir();
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Windows simulator: a 1920x1080 window (`RDTNATIVE_SIM_WINDOW=WxH` to change; the UI is scaled
/// to fit) with a desktop GL context. Tries a 4.1 core profile first (what plx-native's macOS
/// simulator uses: the vendored renderer then compiles its GLES2 shaders behind a `#version 410
/// core` preamble and binds a VAO), then an OpenGL ES 2.0 context (shaders as on the TV), then
/// whatever the driver gives by default. Resolves the GL trampolines once a context exists.
#[cfg(any(windows, feature = "sim"))]
unsafe fn sim_window() -> *mut std::os::raw::c_void {
    let (w, h) = std::env::var("RDTNATIVE_SIM_WINDOW")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?))))
        .unwrap_or((1920, 1080));
    let mut flags = sdl::SDL_WINDOW_OPENGL;
    if std::env::var_os("RDTNATIVE_SIM_HIDDEN").is_some() {
        flags |= sdl::SDL_WINDOW_HIDDEN;
    }
    let title = CString::new("rdtnative (simulator)").unwrap();
    let attempts: [(&str, Option<(i32, i32, i32)>); 3] = [
        ("GL 4.1 core", Some((sdl::SDL_GL_CONTEXT_PROFILE_CORE, 4, 1))),
        ("GLES 2.0", Some((sdl::SDL_GL_CONTEXT_PROFILE_ES, 2, 0))),
        ("driver default", None),
    ];
    for (name, profile) in attempts {
        sdl::SDL_GL_ResetAttributes();
        if let Some((mask, major, minor)) = profile {
            sdl::SDL_GL_SetAttribute(sdl::SDL_GL_CONTEXT_PROFILE_MASK, mask);
            sdl::SDL_GL_SetAttribute(sdl::SDL_GL_CONTEXT_MAJOR_VERSION, major);
            sdl::SDL_GL_SetAttribute(sdl::SDL_GL_CONTEXT_MINOR_VERSION, minor);
        }
        for (a, v) in [(sdl::SDL_GL_RED_SIZE, 8), (sdl::SDL_GL_GREEN_SIZE, 8), (sdl::SDL_GL_BLUE_SIZE, 8), (sdl::SDL_GL_ALPHA_SIZE, 8)] {
            sdl::SDL_GL_SetAttribute(a, v);
        }
        let win = sdl::SDL_CreateWindow(title.as_ptr(), sdl::SDL_WINDOWPOS_CENTERED, sdl::SDL_WINDOWPOS_CENTERED, w, h, flags);
        if win.is_null() {
            log(&format!("sim: SDL_CreateWindow failed: {}", sdl::error()));
            std::process::exit(1);
        }
        if !sdl::SDL_GL_CreateContext(win).is_null() {
            log(&format!("sim: {w}x{h} window, {name} context"));
            #[cfg(windows)]
            sim::gl::load();
            return win;
        }
        log(&format!("sim: {name} context refused: {}", sdl::error()));
        sdl::SDL_DestroyWindow(win);
    }
    log("sim: no usable OpenGL context");
    std::process::exit(1);
}


fn main() {
    crashtrace::install(); // first: every later crash gets a record
    START.get_or_init(Instant::now);
    #[cfg(any(windows, feature = "sim"))]
    if std::env::var_os("PLXNATIVE_RUNTIME_DIR").is_none() {
        let run = sim::base_dir().join("run");
        let _ = std::fs::create_dir_all(&run);
        std::env::set_var("PLXNATIVE_RUNTIME_DIR", run);
    }
    std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true");
    std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_EXIT", "true");
    let _ = plx_platform::tv::install(&wl::PORT);
    store::set_data_dir(data_dir());
    log(&format!("rdtnative {}: boot", env!("CARGO_PKG_VERSION")));

    unsafe {
        sdl::SDL_SetMainReady();
        let hint = |k: &str, v: &str| {
            let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
            sdl::SDL_SetHint(k.as_ptr(), v.as_ptr());
        };
        hint("SDL_VIDEO_ALLOW_SCREENSAVER", "1");
        if sdl::SDL_Init(sdl::SDL_INIT_VIDEO) != 0 {
            log(&format!("SDL_Init failed: {}", sdl::error()));
            std::process::exit(1);
        }
        #[cfg(any(windows, feature = "sim"))]
        let win = sim_window();
        #[cfg(not(any(windows, feature = "sim")))]
        let win = {
            for (a, v) in [
                (sdl::SDL_GL_CONTEXT_PROFILE_MASK, sdl::SDL_GL_CONTEXT_PROFILE_ES),
                (sdl::SDL_GL_CONTEXT_MAJOR_VERSION, 2),
                (sdl::SDL_GL_CONTEXT_MINOR_VERSION, 0),
                (sdl::SDL_GL_RED_SIZE, 8),
                (sdl::SDL_GL_GREEN_SIZE, 8),
                (sdl::SDL_GL_BLUE_SIZE, 8),
                (sdl::SDL_GL_ALPHA_SIZE, 8),
                (sdl::SDL_GL_BUFFER_SIZE, 32),
                (sdl::SDL_GL_DEPTH_SIZE, 0),
                (sdl::SDL_GL_STENCIL_SIZE, 0),
            ] {
                sdl::SDL_GL_SetAttribute(a, v);
            }
            let title = CString::new("rdtnative").unwrap();
            let win = sdl::SDL_CreateWindow(title.as_ptr(), 0, 0, 1920, 1080, sdl::SDL_WINDOW_OPENGL | sdl::SDL_WINDOW_FULLSCREEN);
            if win.is_null() {
                log(&format!("SDL_CreateWindow failed: {}", sdl::error()));
                std::process::exit(1);
            }
            if sdl::SDL_GL_CreateContext(win).is_null() {
                log(&format!("SDL_GL_CreateContext failed: {}", sdl::error()));
                std::process::exit(1);
            }
            win
        };
        plx_base::surface::probe(win);
        sdl::SDL_GL_SetSwapInterval(1);
        plx_platform::tv::window::bind_window(win);
        plx_platform::tv::window::grab(win);

        plx_gfx::gfx::init_gl();
        plx_gfx::text::init_text();
        plx_gfx::gfx::init_image();
        plx_gfx::gfx::init_blur();
        plx_platform::i18n::initialize(plx_platform::i18n::Preference::System, false);
        plx_ui::tex::install(&*images::THUMBS);
        log("rdtnative: graphics ready");

        let mut rig = AppRig::new();
        // Once in a while at start: is there a newer release? (asked about on the feed)
        update::start_auto_check();
        let mut d = Dispatcher::<RdHost>::with_transition(Box::new(PageDip::new()));
        d.request(MachineId::Nav, NavOp::Root(RdArg::Home));
        let (mut mouse_down, mut last_wheel) = (false, 0u32);
        let mut input_echo = 0u8;
        let mut perf = Perf::default();
        let mut pointer_at: Option<(f32, f32)> = None;
        let mut edge_v = edge::Glide::default();
        let (mut cursor_motion_at, mut cursor_hidden) = (0u32, true);
        let mut remote = remote::Remote::open();
        let mut ev = [0u8; 128];
        let mut prev = 0u32;
        let mut running = true;
        let mut foreground = true;
        let mut watch = awake::Watch::new(sdl::SDL_GetTicks());
        while running {
            let now = sdl::SDL_GetTicks();
            let dt = if prev == 0 { 0.016 } else { ((now - prev) as f32 / 1000.0).min(0.05) };
            prev = now;
            let tick = Tick { ms: now, dt_us: (dt * 1e6) as u32 };

            let mut inputs = Vec::new();
            // A key, the pointer or the wheel (the idle screen's clock; not the app's own events).
            let mut real_input = false;
            while sdl::SDL_PollEvent(ev.as_mut_ptr().cast()) != 0 {
                let kind = input::rd_u32(&ev, 0);
                real_input |= matches!(kind, sdl::SDL_MOUSEMOTION | sdl::SDL_MOUSEBUTTONDOWN | sdl::SDL_MOUSEBUTTONUP | sdl::SDL_MOUSEWHEEL | sdl::SDL_KEYDOWN | sdl::SDL_KEYUP);
                match kind {
                    sdl::SDL_QUIT => running = false,
                    sdl::SDL_MOUSEMOTION => {
                        cursor_motion_at = now;
                        cursor_hidden = false;
                        let (x, y) = input::pointer_xy(&ev);
                        pointer_at = Some((x, y));
                        inputs.push(if mouse_down { input::drag(x, y, tick) } else { input::pointer(x, y, tick) });
                    }
                    sdl::SDL_MOUSEBUTTONDOWN if ev[16] == 1 => {
                        let (x, y) = input::pointer_xy(&ev);
                        mouse_down = true;
                        inputs.push(input::click(x, y, tick));
                    }
                    sdl::SDL_MOUSEBUTTONUP if ev[16] == 1 => {
                        mouse_down = false;
                        inputs.push(input::release(tick));
                    }
                    sdl::SDL_MOUSEWHEEL => {
                        // One notch = one row (lists and the keyboard step by focus).
                        let dy = input::rd_i32(&ev, 20);
                        if dy != 0 && now.wrapping_sub(last_wheel) > 110 {
                            last_wheel = now;
                            for _ in 0..WHEEL_ROWS.load(std::sync::atomic::Ordering::Relaxed).max(1) {
                                inputs.extend(input::wheel(dy, tick));
                            }
                        }
                    }
                    sdl::SDL_KEYDOWN | sdl::SDL_KEYUP => {
                        let (state, wcode, sym) = input::decode_key(&ev);
                        // An arrow key puts the Magic Remote pointer away (as plx-native does).
                        if state & 0xff != 0 && !cursor_hidden && sym & (1 << 30) != 0 && (79..=82).contains(&(sym & 0xff)) {
                            hide_cursor();
                            cursor_hidden = true;
                            pointer_at = None;
                        }
                        if wcode == 505 {
                            running = false; // EXIT key
                        }
                        let k = input::key_input(sym, wcode, state, tick);
                        if matches!(k.kind, plx_machine::machine::InputKind::PointerHidden | plx_machine::machine::InputKind::Key { .. }) {
                            pointer_at = None;
                        }
                        inputs.push(k);
                    }
                    sdl::SDL_APP_WILLENTERBACKGROUND | sdl::SDL_APP_DIDENTERBACKGROUND => {
                        if foreground {
                            log("lifecycle: background");
                        }
                        foreground = false;
                        FOREGROUND.store(false, std::sync::atomic::Ordering::Relaxed);
                        wl::release();
                    }
                    sdl::SDL_APP_DIDENTERFOREGROUND => {
                        log("lifecycle: foreground");
                        foreground = true;
                        FOREGROUND.store(true, std::sync::atomic::Ordering::Relaxed);
                        wl::grab(win);
                    }
                    _ => {}
                }
            }
            if let Some(r) = remote.as_mut() {
                let mut quit = false;
                let keys = r.poll(tick, &mut quit);
                real_input |= !keys.is_empty();
                inputs.extend(keys);
                if quit {
                    running = false;
                }
            }

            plx_machine::idle::frame_begin(dt);
            images::THUMBS.deliver();
            let results = rig.drain(now);
            for e in &inputs {
                use plx_machine::machine::InputKind as K;
                match e.kind {
                    K::Pointer { x, y, .. } | K::Drag { x, y, .. } => pointer_at = Some((x, y)),
                    K::PointerHidden | K::Key { .. } => pointer_at = None,
                    _ => {}
                }
            }
            // Pointer resting at the top / bottom edge steps the list under it, after a short
            // rest, faster nearer the very edge (plx-native's edge glide, via ytnative).
            {
                const V_BAND: f32 = 48.0;
                let top = d.top_screen().map(|s| s.name());
                // Full-screen media (the video, the picture viewer, the slideshow) takes no edge
                // scrolling: there Up/Down mean seek, caption and sound.
                let full_screen = matches!(top, Some("player" | "viewer" | "slideshow"));
                let v_side = pointer_at.filter(|_| !mouse_down && !full_screen).and_then(|(_, y)| {
                    if y < V_BAND {
                        Some((1i8, y, V_BAND))
                    } else if y >= 1080.0 - V_BAND {
                        Some((-1i8, 1080.0 - y, V_BAND))
                    } else {
                        None
                    }
                });
                let speed = EDGE_SPEED.load(std::sync::atomic::Ordering::Relaxed) as f32;
                let v_side = v_side.filter(|_| speed > 0.0);
                if let Some((x, y)) = pointer_at {
                    ui::set_pointer(x, y);
                }
                if ui::glides(top) {
                    // Scrolling pages glide by pixels (eased in, faster nearer the edge), as
                    // ytnative's pages do; the screen keeps focus on what stays in view.
                    if let Some((dir, share, dt)) = edge_v.frame(v_side, now) {
                        inputs.push(input::wheel_event(dir as f32 * speed * 0.6 * share * dt, tick));
                    }
                } else if let Some(dir) = edge_v.step(v_side, now, speed.max(1.0)) {
                    inputs.extend(input::wheel(dir as i32, tick));
                }
            }
            // Idle pointer: hide it after a few seconds without motion (sooner over a video).
            let idle_ms = if matches!(d.top_screen().map(|s| s.name()), Some("player" | "slideshow")) { 3000 } else { 5000 };
            if !cursor_hidden && !mouse_down && now.wrapping_sub(cursor_motion_at) > idle_ms {
                hide_cursor();
                cursor_hidden = true;
                pointer_at = None;
                inputs.push(plx_machine::machine::InputEvent {
                    at: tick,
                    source: plx_machine::machine::Source::Sdl,
                    kind: plx_machine::machine::InputKind::PointerHidden,
                });
            }
            // Lean-back: the idle screen after a while without a button (never over a video, a
            // slideshow or a menu), and the TV kept awake while pictures or a video move.
            {
                let video = matches!(rig.store.play.phase, player::Phase::Playing | player::Phase::Loading);
                let busy = video || screens::slideshow::open() || !foreground || ui::menu_open() || !rig.store.configured();
                watch.note(now, real_input, busy);
                if watch.idle_due(now, rig.store.prefs.num("idle_after").max(0) as u32) && rig.store.nav_request.is_none() {
                    log("idle: opening the idle screen");
                    rig.store.nav_request = Some(screens::slideshow::idle_arg(&rig.store.prefs));
                }
                let moving = foreground && (screens::slideshow::advancing() || rig.store.play.phase == player::Phase::Playing);
                if let Some(on) = watch.awake(now, moving) {
                    log(if on { "screensaver: held off while pictures or a video move" } else { "screensaver: allowed" });
                    if on {
                        sdl::SDL_DisableScreenSaver();
                    } else {
                        sdl::SDL_EnableScreenSaver();
                    }
                }
            }
            // The automatic update check found a newer version: ask once, on the feed (never over
            // a video or a page being read).
            if let Some(arg) = rig.store.nav_request.take() {
                d.request(MachineId::Nav, NavOp::Push(arg));
            }
            if d.top_screen().map(|s| s.name()) == Some("feed") && update::take_prompt() {
                d.request(MachineId::Nav, NavOp::Present(RdArg::Menu(host::MenuKind::Update)));
            }
            let had_input = !inputs.is_empty();
            if had_input {
                input_echo = 3;
            } else if input_echo > 0 {
                input_echo -= 1;
            }
            let had_results = !results.is_empty();
            let step_t0 = Instant::now();
            let r = d.frame_with(&mut rig, tick, inputs, results, &mut NoTap, false);
            perf.step(step_t0.elapsed().as_micros() as u64);
            d.prune(&r.unmounted);
            plx_ui::tex::note_queued(&mut d.budget);

            let present = foreground
                && (r.presented || had_input || had_results || input_echo > 0 || ui::take_animating() || plx_machine::idle::should_present(now) || plx_ui::tex::has_pending());
            #[cfg(any(windows, feature = "sim"))]
            let present = present || remote.as_ref().is_some_and(|r| r.shot_pending());
            if present {
                let draw_t0 = Instant::now();
                plx_gfx::text::begin_frame();
                plx_ui::card_motion::begin_frame(now);
                plx_gfx::gfx::straighten::frame_begin();
                let (x, y, w, h) = plx_base::surface::viewport();
                sdl::glViewport(x, y, w, h);
                // The player keeps the video plane visible under a transparent page.
                if d.top_screen().map(|s| s.render()) == Some(plx_ui::screen::RenderStrategy::VideoPlane) {
                    plx_gfx::gfx::frame_clear_through();
                } else {
                    let c = ui::bg_rgb();
                    plx_gfx::gfx::frame_clear(c.0, c.1, c.2);
                }
                plx_ui::guard(|| {
                    d.draw(&mut rig, true);
                });
                #[cfg(any(windows, feature = "sim"))]
                if let Some(r) = remote.as_mut() {
                    let (mut dw, mut dh) = (0, 0);
                    sdl::SDL_GL_GetDrawableSize(win, &mut dw, &mut dh);
                    r.take_shots(dw, dh);
                }
                perf.draw(draw_t0.elapsed().as_micros() as u64);
                sdl::SDL_GL_SwapWindow(win);
                plx_gfx::gfx::blur_frame_end();
                plx_gfx::gfx::region_frame_end();
                plx_gfx::gfx::snapshot_frame_end();
                plx_gfx::gfx::field_frame_end();
                plx_gfx::gfx::ground_probes_frame_end();
                plx_machine::idle::note_present(now);
            } else {
                plx_gfx::text::warm_fonts_idle(2000, now_us);
                sdl::SDL_Delay(plx_machine::idle::IDLE_POLL_MS);
            }
        }
        log("rdtnative: exit");
        sdl::SDL_Quit();
    }
}

/// Hides LG's Magic Remote pointer (`SDL_webOSCursorVisibility(0)`); the compositor shows it again
/// by itself on the next motion. Native SDL apps get no idle hide from the system, so the app does
/// it: after a few seconds without motion, and at once on an arrow key.
#[cfg(not(any(windows, feature = "sim")))]
fn hide_cursor() {
    type F = unsafe extern "C" fn(std::ffi::c_int) -> std::ffi::c_int;
    static FN: std::sync::OnceLock<Option<F>> = std::sync::OnceLock::new();
    let f = FN.get_or_init(|| {
        let name = CString::new("SDL_webOSCursorVisibility").unwrap();
        let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if p.is_null() {
            log("cursor: no SDL_webOSCursorVisibility");
            None
        } else {
            Some(unsafe { std::mem::transmute::<*mut libc::c_void, F>(p) })
        }
    });
    if let Some(f) = f {
        unsafe { f(0) };
    }
}
#[cfg(any(windows, feature = "sim"))]
fn hide_cursor() {}

/// Frame-time statistics, logged once a minute while frames are drawn.
#[derive(Default)]
struct Perf {
    since: Option<Instant>,
    steps: u64,
    step_sum: u64,
    step_max: u64,
    draws: u64,
    draw_sum: u64,
    draw_max: u64,
}

impl Perf {
    fn step(&mut self, us: u64) {
        let since = *self.since.get_or_insert_with(Instant::now);
        self.steps += 1;
        self.step_sum += us;
        self.step_max = self.step_max.max(us);
        if since.elapsed().as_secs() >= 60 {
            if self.draws > 0 {
                log(&format!(
                    "perf: {} draws, draw avg {} max {} us; step avg {} max {} us",
                    self.draws,
                    self.draw_sum / self.draws,
                    self.draw_max,
                    self.step_sum / self.steps.max(1),
                    self.step_max
                ));
            }
            *self = Perf::default();
        }
    }
    fn draw(&mut self, us: u64) {
        self.draws += 1;
        self.draw_sum += us;
        self.draw_max = self.draw_max.max(us);
    }
}
