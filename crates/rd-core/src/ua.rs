//! Reddit's required User-Agent: `<platform>:<app id>:<version> (by /u/<username>)`.

pub const APP_ID: &str = "io.github.danzig666.rdtnative";

/// `platform` is "webos" on the TV, "linux"/"windows" for the spike and simulator.
pub fn user_agent(platform: &str, username: Option<&str>) -> String {
    let v = env!("CARGO_PKG_VERSION");
    match username {
        Some(u) if !u.is_empty() => format!("{platform}:{APP_ID}:v{v} (by /u/{u})"),
        _ => format!("{platform}:{APP_ID}:v{v}"),
    }
}

/// The platform string for the machine this was compiled for.
pub fn host_platform() -> &'static str {
    if cfg!(target_arch = "arm") {
        "webos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_reddits_format() {
        let v = env!("CARGO_PKG_VERSION");
        assert_eq!(user_agent("webos", Some("alice")), format!("webos:{APP_ID}:v{v} (by /u/alice)"));
        assert_eq!(user_agent("webos", None), format!("webos:{APP_ID}:v{v}"));
        assert_eq!(user_agent("webos", Some("")), format!("webos:{APP_ID}:v{v}"));
    }
}
