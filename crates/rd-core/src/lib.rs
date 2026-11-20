//! Reddit API core for rdtnative. Pure Rust, no TV dependencies (the counterpart of ytnative's
//! `yt-core`). Every user supplies their own Reddit "installed app" client ID; this crate never
//! contains one.

pub mod auth;
pub mod comments;
pub mod config;
pub mod error;
pub mod hls;
pub mod listing;
pub mod markdown;
pub mod media;
pub mod model;
pub mod ratelimit;
pub mod session;
pub mod ua;

pub use error::RdError;

/// Host for token requests and the authorize page.
pub const WWW: &str = "https://www.reddit.com";
/// Host for every authenticated API call.
pub const OAUTH: &str = "https://oauth.reddit.com";
/// The redirect URI users register on their Reddit app (a static page in `site/callback`).
pub const DEFAULT_REDIRECT_URI: &str = "https://danzig666.github.io/rdtnative/callback";

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `n` random characters from `[a-z0-9]` (state nonces, device IDs).
pub fn random_token(n: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).expect("OS random source");
    buf.iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect()
}

/// Percent-encodes a query component (RFC 3986 unreserved characters pass through).
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Builds `a=b&c=d` with each value percent-encoded.
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_reserved_characters() {
        assert_eq!(enc("a b/c:d"), "a%20b%2Fc%3Ad");
        assert_eq!(enc("Az09-_.~"), "Az09-_.~");
        assert_eq!(form(&[("x", "1 2"), ("y", "&")]), "x=1%202&y=%26");
    }

    #[test]
    fn random_tokens_have_the_requested_shape() {
        let t = random_token(30);
        assert_eq!(t.len(), 30);
        assert!(t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        assert_ne!(t, random_token(30));
    }
}
