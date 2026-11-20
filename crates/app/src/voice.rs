//! LG voice search ("search for cats" on the Magic Remote while rdtnative is in front), after
//! ytnative's: registers with `com.webos.service.voiceconductor` as the foreground interactor,
//! offers the `SearchContent` intent when asked for the context, and queues the words of a
//! `performAction` for the frame loop, which types them into an open keyboard or searches Reddit
//! for them. Whether the TV hands a native app's bus name the voice at all is unverified, so every
//! reply is logged ("voice: ..."). The simulator has no voice service.

use std::sync::Mutex;

/// Words heard and not yet taken by the frame loop.
static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// What was said since the last call (main thread, once a frame).
pub fn take() -> Vec<String> {
    std::mem::take(&mut *HEARD.lock().unwrap())
}

#[cfg(not(any(windows, feature = "sim")))]
pub fn start() {
    use crate::ls2::Luna;
    plx_base::task::spawn("voice", || {
        // The media connection may only talk to the media services: the wide one may call any.
        let luna = match Luna::wide(plx_base::paths::app_id()) {
            Ok(l) => l,
            Err(e) => return crate::log(&format!("voice: no bus: {e}")),
        };
        let ok = luna.subscribe(
            "luna://com.webos.service.voiceconductor/interactor/register",
            r#"{"type":"foreground","subscribe":true}"#,
            // Replies are calls on the same bus: never block the bus thread that delivers this.
            move |msg| {
                let msg = msg.to_string();
                std::thread::spawn(move || handle(luna, &msg));
            },
        );
        crate::log(&format!("voice: interactor/register -> {ok}"));
    });
}

#[cfg(any(windows, feature = "sim"))]
pub fn start() {}

#[cfg(not(any(windows, feature = "sim")))]
fn handle(luna: &'static crate::ls2::Luna, msg: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else {
        return crate::log(&format!("voice: {}", msg.chars().take(200).collect::<String>()));
    };
    let ticket = v["voiceTicket"].clone();
    match v["command"].as_str() {
        Some("setContext") => {
            let p = serde_json::json!({
                "voiceTicket": ticket,
                "inAppIntents": [{ "intent": "SearchContent", "supportAsrOnly": true }],
            });
            let r = luna.call("luna://com.webos.service.voiceconductor/interactor/setContext", &p.to_string());
            crate::log(&format!("voice: setContext -> {r}"));
        }
        Some("performAction") => {
            let text = spoken(&v);
            crate::log(&format!("voice: performAction intent={} heard={:?}", v["action"]["intent"], text.is_some()));
            let done = match text {
                Some(t) => {
                    HEARD.lock().unwrap().push(t);
                    plx_machine::present::wake_from_worker();
                    true
                }
                None => {
                    crate::log(&format!("voice: no words in {}", msg.chars().take(400).collect::<String>()));
                    false
                }
            };
            let p = serde_json::json!({ "voiceTicket": ticket, "result": done });
            let r = luna.call("luna://com.webos.service.voiceconductor/interactor/reportActionResult", &p.to_string());
            crate::log(&format!("voice: reportActionResult -> {r}"));
        }
        _ => crate::log(&format!("voice: {}", msg.chars().take(200).collect::<String>())),
    }
}

/// The words of a `performAction`. Where they are differs between webOS versions and intents, so
/// every field a known client reads is tried: the action's own, its `nlpResult`, then the message's.
#[cfg_attr(any(windows, feature = "sim"), allow(dead_code))]
fn spoken(v: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 9] = ["query", "value", "displayString", "utterance", "text", "sttResult", "recognized", "keyword", "itemId"];
    let a = &v["action"];
    // A field may hold the words or a list of guesses, best first.
    let words = |f: &serde_json::Value| f.as_str().or_else(|| f.as_array()?.first()?.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    [a, &a["nlpResult"], v].into_iter().find_map(|o| KEYS.iter().find_map(|k| words(&o[*k])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_words_are_found_wherever_the_tv_puts_them() {
        let t = |v: serde_json::Value| spoken(&v);
        assert_eq!(t(json!({"command":"performAction","action":{"intent":"SearchContent","query":" cats "}})).as_deref(), Some("cats"));
        assert_eq!(t(json!({"action":{"intent":"SearchContent","value":"rust lang"}})).as_deref(), Some("rust lang"));
        assert_eq!(t(json!({"action":{"nlpResult":{"displayString":"funny dogs"}}})).as_deref(), Some("funny dogs"));
        assert_eq!(t(json!({"action":{"sttResult":["best guess","other"]}})).as_deref(), Some("best guess"));
        assert_eq!(t(json!({"action":{"intent":"SearchContent"},"utterance":"top level"})).as_deref(), Some("top level"));
        assert_eq!(t(json!({"action":{"query":"  ","text":"second field"}})).as_deref(), Some("second field"));
        assert_eq!(t(json!({"command":"performAction","action":{"intent":"SearchContent"}})), None);
    }
}
