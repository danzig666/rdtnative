//! Desktop simulator support (Windows, or Linux with `--features sim`; the TV build never compiles this).
//!
//! The same app runs in a 1920x1080 SDL window on desktop OpenGL: `gl` forwards the GLES2 entry
//! points, `remote` serves the key
//! channel on localhost TCP, `shot` writes screenshots. `ls2`/`wl` (the stand-ins for the TV's
//! luna-service2 and Wayland modules) are mounted in their place from `main.rs`.

#[cfg(windows)]
pub mod gl;
pub mod shot;

/// The data dir (settings, tokens) and runtime root: `%LOCALAPPDATA%\rdtnative` on Windows,
/// `$XDG_DATA_HOME/rdtnative` (default `~/.local/share/rdtnative`) on Linux, or `RDTNATIVE_SIM_DATA`.
pub fn base_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("RDTNATIVE_SIM_DATA") {
        return d.into();
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("rdtnative")
}

/// Local time offset from UTC in seconds (the MSVC CRT has no `tm_gmtoff`).
#[cfg(windows)]
pub fn local_offset_secs() -> i64 {
    extern "C" {
        fn _localtime64_s(out: *mut libc::tm, t: *const i64) -> i32;
        fn _mkgmtime64(t: *mut libc::tm) -> i64;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if _localtime64_s(&mut tm, &now) != 0 {
            return 0;
        }
        let as_utc = _mkgmtime64(&mut tm);
        if as_utc == -1 {
            0
        } else {
            as_utc - now
        }
    }
}
