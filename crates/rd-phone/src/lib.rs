//! "Enter on your phone": while a TV form is open, a one-time web page on the home network lets a
//! phone fill in its fields (typing or pasting, e.g. a Reddit client ID). The TV
//! shows the page's address as a QR code.
//!
//! The same approach as RustCouch's "Type on phone" (danzig666/webos-rustdesk-client), and as narrow:
//! it starts only when the person asks for it, binds the TV's LAN address on a random port, serves
//! one page at an unguessable 128-bit random path, and stops after one *valid* submission, when the
//! form closes (drop), or after its lifetime. An invalid submission re-shows the page with the
//! entered values and a message under each wrong field. Plain HTTP on the local network (a TV app
//! has no certificate a phone would trust), so it is meant for a home network you trust.
//!
//! std only (one thread, no async runtime), so the TV app, the simulator and the spike share it.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod page;
mod qr;
pub use qr::Qr;

/// Headers and the form's other bytes; each field adds room for its longest value (see
/// [`request_limit`]).
const MAX_REQUEST: usize = 16 * 1024;

/// The longest text a [`Kind::Multiline`] field takes unless `max_len` says otherwise (Reddit's
/// comment limit).
pub const MULTILINE_MAX_LEN: usize = 10_000;

/// Submitted values by field key.
pub type Values = BTreeMap<String, String>;

/// Checks a submission; `Err` lists `(field key, message)` to show under those fields.
pub type Validate = dyn Fn(&Values) -> Result<(), Vec<(String, String)>> + Send + Sync;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// Opens the phone's URL keyboard.
    Url,
    /// Hidden while typing, with a "Show" box.
    Secret,
    /// Several lines (a reply): a text box that keeps line breaks, with the phone's autocorrect
    /// and dictation on.
    Multiline,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub key: String,
    pub label: String,
    /// Pre-filled value (what the TV already has).
    pub value: String,
    /// Grey text shown when empty (e.g. the default that applies).
    pub placeholder: String,
    /// Small text under the field.
    pub help: String,
    pub kind: Kind,
    pub max_len: usize,
}

impl Field {
    pub fn new(key: &str, label: &str) -> Field {
        Field {
            key: key.into(),
            label: label.into(),
            value: String::new(),
            placeholder: String::new(),
            help: String::new(),
            kind: Kind::Text,
            max_len: 1024,
        }
    }
    pub fn value(mut self, v: &str) -> Field {
        self.value = v.into();
        self
    }
    pub fn placeholder(mut self, v: &str) -> Field {
        self.placeholder = v.into();
        self
    }
    pub fn help(mut self, v: &str) -> Field {
        self.help = v.into();
        self
    }
    /// A multi-line field also gets [`MULTILINE_MAX_LEN`]; `max_len` after this changes it.
    pub fn kind(mut self, k: Kind) -> Field {
        if k == Kind::Multiline {
            self.max_len = MULTILINE_MAX_LEN;
        }
        self.kind = k;
        self
    }
    pub fn max_len(mut self, n: usize) -> Field {
        self.max_len = n;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Form {
    /// Page heading, e.g. "Reddit API settings".
    pub title: String,
    /// Paragraphs above the fields.
    pub intro: Vec<String>,
    /// A quoted text under the intro (the comment a reply answers), line breaks kept.
    pub quote: String,
    /// Links shown under the intro (label, https URL).
    pub links: Vec<(String, String)>,
    pub fields: Vec<Field>,
    pub submit: String,
    pub lifetime: Duration,
    /// Numbered steps under the intro.
    pub steps: Vec<String>,
    /// A big button-styled link above the form (label, https URL), e.g. "Approve on Reddit".
    pub action: Option<(String, String)>,
    /// Fold-out notes under the form (summary, text), e.g. per-browser instructions.
    pub notes: Vec<(String, String)>,
    /// Pasting into a field sends the form at once.
    pub paste_submits: bool,
    /// A short address to type by hand: a fixed port when free ([`SHORT_PORT`]) and a short path.
    pub short: bool,
    /// Also take an OAuth redirect that lands on this server (any path with `state=` and `code=`
    /// or `error=`): its address becomes this field's value (`validate` must check `state`).
    pub catch_redirect: Option<String>,
    /// The page after a valid submission (heading, text).
    pub done: (String, String),
}

impl Form {
    pub fn new(title: &str) -> Form {
        Form {
            title: title.into(),
            intro: Vec::new(),
            quote: String::new(),
            links: Vec::new(),
            fields: Vec::new(),
            submit: "Send to TV".into(),
            lifetime: Duration::from_secs(600),
            steps: Vec::new(),
            action: None,
            notes: Vec::new(),
            paste_submits: false,
            short: false,
            catch_redirect: None,
            done: ("Sent to the TV.".into(), "You can close this page. The link no longer works.".into()),
        }
    }
}

/// A running one-time form.
pub struct PhoneForm {
    /// The page's address (encode it as the QR code).
    pub url: String,
    slot: Arc<Mutex<Option<Values>>>,
    stop: Arc<AtomicBool>,
    opened: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    submitted: Arc<AtomicBool>,
}

/// The TV's address on the home network: the interface its default route uses (no packet is sent).
pub fn lan_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).ok()?;
    s.connect(("192.0.2.1", 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// 128 random bits as 32 hex digits.
fn random_path() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| format!("no random source ({e})"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// The port a short address uses when it is free (else the next ones, else any).
pub const SHORT_PORT: u16 = 8091;

/// Letters easy to read off a TV and type: no 0/o, 1/l/i, and no x (the TV font draws an x
/// between digits as a multiplication sign).
const SHORT_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwyz23456789";

/// A short path to type by hand: 5 characters (~28 million). Wrong paths are answered slowly
/// (see `serve`), so guessing one in the page's lifetime is out of reach.
fn short_path() -> Result<String, String> {
    let mut b = [0u8; 5];
    getrandom::getrandom(&mut b).map_err(|e| format!("no random source ({e})"))?;
    Ok(b.iter().map(|x| SHORT_ALPHABET[*x as usize % SHORT_ALPHABET.len()] as char).collect())
}

fn bind(ip: IpAddr, short: bool) -> std::io::Result<TcpListener> {
    if short {
        for port in SHORT_PORT..SHORT_PORT + 5 {
            if let Ok(l) = TcpListener::bind(SocketAddr::from((ip, port))) {
                return Ok(l);
            }
        }
    }
    TcpListener::bind(SocketAddr::from((ip, 0)))
}

impl PhoneForm {
    /// Starts on the TV's LAN address.
    pub fn start(form: Form, validate: Box<Validate>) -> Result<PhoneForm, String> {
        let ip = lan_ip().ok_or("the TV has no home network address")?;
        PhoneForm::start_on(ip, form, validate)
    }

    /// Starts on a given address (tests use 127.0.0.1).
    pub fn start_on(ip: IpAddr, form: Form, validate: Box<Validate>) -> Result<PhoneForm, String> {
        PhoneForm::start_built(ip, form.short, move |_| form, validate)
    }

    /// Starts on the TV's LAN address with a form that needs the server's `host:port` (e.g. an
    /// OAuth state that tells a callback page where to hand the code back).
    pub fn start_with(short: bool, build: impl FnOnce(&str) -> Form, validate: Box<Validate>) -> Result<PhoneForm, String> {
        let ip = lan_ip().ok_or("the TV has no home network address")?;
        PhoneForm::start_built(ip, short, build, validate)
    }

    fn start_built(ip: IpAddr, short: bool, build: impl FnOnce(&str) -> Form, validate: Box<Validate>) -> Result<PhoneForm, String> {
        let listener = bind(ip, short).map_err(|e| format!("cannot open a port on the TV ({e})"))?;
        let host = listener.local_addr().map_err(|e| e.to_string())?.to_string();
        let mut form = build(&host);
        form.short = short;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let token = if form.short { short_path()? } else { random_path()? };
        let url = format!("http://{ip}:{port}/{token}");
        let me = PhoneForm {
            url,
            slot: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            opened: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(true)),
            submitted: Arc::new(AtomicBool::new(false)),
        };
        let (slot, stop, opened, running) = (me.slot.clone(), me.stop.clone(), me.opened.clone(), me.running.clone());
        let submitted = me.submitted.clone();
        std::thread::Builder::new()
            .name("phone-form".into())
            .spawn(move || {
                serve(listener, &token, &form, &*validate, &slot, &stop, &opened);
                if slot.lock().is_ok_and(|g| g.is_some()) {
                    submitted.store(true, Ordering::SeqCst);
                }
                running.store(false, Ordering::SeqCst);
            })
            .map_err(|e| e.to_string())?;
        Ok(me)
    }

    /// The values sent from the phone, once (after a valid submission).
    pub fn take(&self) -> Option<Values> {
        self.slot.lock().ok().and_then(|mut g| g.take())
    }

    /// The page has been opened on a phone (the TV can say "Opened on your phone…").
    pub fn opened(&self) -> bool {
        self.opened.load(Ordering::SeqCst)
    }

    /// False once the listener stopped (submitted, expired or dropped).
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Set once a valid submission arrived (also while the TV app is in the background); the
    /// count of other holders drops when the form is gone.
    pub fn submitted(&self) -> Arc<AtomicBool> {
        self.submitted.clone()
    }

    /// The QR code of `url`.
    pub fn qr(&self) -> Option<Qr> {
        Qr::new(&self.url)
    }
}

impl Drop for PhoneForm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn serve(
    listener: TcpListener,
    token: &str,
    form: &Form,
    validate: &Validate,
    slot: &Mutex<Option<Values>>,
    stop: &AtomicBool,
    opened: &AtomicBool,
) {
    let deadline = Instant::now() + form.lifetime;
    let limit = request_limit(form);
    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
        let mut sock = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(_) => continue,
        };
        let _ = sock.set_nonblocking(false);
        let _ = sock.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = sock.set_write_timeout(Some(Duration::from_secs(5)));
        let Some(req) = read_request(&mut sock, limit) else { continue };
        let (status, body) = match handle(&req, token, form, validate) {
            Reply::Page(html) => {
                opened.store(true, Ordering::SeqCst);
                ("200 OK", html)
            }
            Reply::Done(values) => {
                if let Ok(mut g) = slot.lock() {
                    *g = Some(values);
                }
                stop.store(true, Ordering::SeqCst);
                ("200 OK", page::done(form))
            }
            Reply::NotFound => {
                // Wrong paths are answered slowly: a short path cannot be found by trying.
                std::thread::sleep(Duration::from_millis(250));
                ("404 Not Found", "Not found".to_string())
            }
        };
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes());
        let _ = sock.shutdown(std::net::Shutdown::Both);
    }
}

/// The biggest request a form takes: every field at its longest, each character percent-encoded
/// (up to four UTF-8 bytes as `%XX`, 12 bytes), plus the headers.
fn request_limit(form: &Form) -> usize {
    MAX_REQUEST + form.fields.iter().map(|f| f.max_len * 12).sum::<usize>()
}

fn read_request(sock: &mut TcpStream, limit: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = sock.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > limit {
            return None;
        }
        if let Some(h) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..h]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if len > limit {
                return None;
            }
            if buf.len() >= h + 4 + len {
                break;
            }
        }
    }
    Some(buf)
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

#[derive(Debug, PartialEq)]
enum Reply {
    Page(String),
    Done(Values),
    NotFound,
}

fn handle(req: &[u8], token: &str, form: &Form, validate: &Validate) -> Reply {
    let Some(h) = find(req, b"\r\n\r\n") else { return Reply::NotFound };
    let head = String::from_utf8_lossy(&req[..h]);
    let mut parts = head.lines().next().unwrap_or("").split(' ');
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let prefill: Values = form.fields.iter().map(|f| (f.key.clone(), f.value.clone())).collect();
    if path != format!("/{token}") {
        // An OAuth redirect landing here (the redirect URI points at this server).
        if let (Some(key), "GET", Some((_, q))) = (&form.catch_redirect, method, path.split_once('?')) {
            let has = |k: &str| q.split('&').any(|kv| kv.starts_with(&format!("{k}=")));
            if has("state") && (has("code") || has("error")) {
                let mut values = prefill.clone();
                values.insert(key.clone(), format!("http://tv{path}"));
                return match validate(&values) {
                    Ok(()) => Reply::Done(values),
                    Err(errors) => Reply::Page(page::form(form, token, &prefill, &errors)),
                };
            }
        }
        return Reply::NotFound;
    }
    match method {
        "GET" => Reply::Page(page::form(form, token, &prefill, &[])),
        "POST" => {
            let body = String::from_utf8_lossy(&req[h + 4..]);
            let mut values = Values::new();
            let mut errors = Vec::new();
            for f in &form.fields {
                let v = body
                    .split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{}=", f.key)))
                    .map(|raw| form_decode(raw, f.kind == Kind::Multiline))
                    .unwrap_or_default();
                let v = v.trim().to_string();
                if v.chars().count() > f.max_len {
                    errors.push((f.key.clone(), format!("Too long (at most {} characters)", f.max_len)));
                }
                values.insert(f.key.clone(), v);
            }
            if errors.is_empty() {
                if let Err(e) = validate(&values) {
                    errors = e;
                }
            }
            if errors.is_empty() {
                Reply::Done(values)
            } else {
                Reply::Page(page::form(form, token, &values, &errors))
            }
        }
        _ => Reply::NotFound,
    }
}

/// `application/x-www-form-urlencoded` value: `+` is a space, `%XX` a byte, UTF-8; control
/// characters (line breaks from a sloppy paste) are dropped, except the line breaks of a
/// multi-line field (`lines`), which come as `\n` whatever the browser sent (`%0D%0A`).
fn form_decode(s: &str, lines: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    let s = String::from_utf8_lossy(&out);
    if lines {
        // A pasted tab parts words (a table, tab-separated values): it becomes a space rather
        // than nothing (the TV's text has no tabs).
        s.replace("\r\n", "\n").replace('\r', "\n").chars().map(|c| if c == '\t' { ' ' } else { c }).filter(|c| *c == '\n' || !c.is_control()).collect()
    } else {
        s.chars().filter(|c| !c.is_control()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        let mut f = Form::new("Reddit <API>");
        f.intro.push("Create an app.".into());
        f.links.push(("Help".into(), "https://example.com/help".into()));
        f.fields.push(Field::new("client_id", "Client ID").value("prefilled12345").help("Under the app's name"));
        f.fields.push(Field::new("redirect_uri", "Redirect URI").kind(Kind::Url).placeholder("https://x/cb").max_len(20));
        f
    }

    fn ok_if_long(v: &Values) -> Result<(), Vec<(String, String)>> {
        if v["client_id"].len() >= 14 {
            Ok(())
        } else {
            Err(vec![("client_id".into(), "Too short <!>".into())])
        }
    }

    fn req(method: &str, path: &str, body: &str) -> Vec<u8> {
        format!("{method} {path} HTTP/1.1\r\nHost: tv\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn only_the_token_path_is_served() {
        let f = form();
        assert!(matches!(handle(&req("GET", "/abc", ""), "abc", &f, &ok_if_long), Reply::Page(_)));
        assert_eq!(handle(&req("GET", "/", ""), "abc", &f, &ok_if_long), Reply::NotFound);
        assert_eq!(handle(&req("GET", "/abcd", ""), "abc", &f, &ok_if_long), Reply::NotFound);
        assert_eq!(handle(&req("PUT", "/abc", ""), "abc", &f, &ok_if_long), Reply::NotFound);
        assert_eq!(handle(b"garbage", "abc", &f, &ok_if_long), Reply::NotFound);
    }

    #[test]
    fn get_shows_prefilled_escaped_page() {
        let Reply::Page(html) = handle(&req("GET", "/t", ""), "t", &form(), &ok_if_long) else { panic!() };
        assert!(html.contains("Reddit &lt;API&gt;"));
        assert!(html.contains(r#"value="prefilled12345""#));
        assert!(html.contains(r#"placeholder="https://x/cb""#));
        assert!(html.contains(r#"inputmode="url""#));
        assert!(html.contains(r#"href="https://example.com/help""#));
    }

    #[test]
    fn valid_post_returns_trimmed_values() {
        let r = handle(&req("POST", "/t", "client_id=+abcDEF123_-xyz12%0A&redirect_uri=http%3A%2F%2Fa%2Fb&extra=1"), "t", &form(), &ok_if_long);
        let Reply::Done(v) = r else { panic!("{r:?}") };
        assert_eq!(v["client_id"], "abcDEF123_-xyz12");
        assert_eq!(v["redirect_uri"], "http://a/b");
        assert_eq!(v.len(), 2, "unknown fields are ignored");
    }

    #[test]
    fn invalid_post_reshows_values_and_errors() {
        let Reply::Page(html) = handle(&req("POST", "/t", "client_id=x%22y&redirect_uri="), "t", &form(), &ok_if_long) else { panic!() };
        assert!(html.contains(r#"value="x&quot;y""#), "entered value kept, escaped");
        assert!(html.contains("Too short &lt;!&gt;"));
        let Reply::Page(html) = handle(&req("POST", "/t", &format!("client_id=abcdefghijklmnop&redirect_uri={}", "a".repeat(21))), "t", &form(), &ok_if_long) else { panic!() };
        assert!(html.contains("Too long (at most 20 characters)"));
    }

    #[test]
    fn form_values_are_decoded() {
        assert_eq!(form_decode("a%2Bb+c%25", false), "a+b c%");
        assert_eq!(form_decode("bad%zz%4", false), "bad%zz%4");
        assert_eq!(form_decode("line%0Abreak", false), "linebreak");
        assert_eq!(form_decode("p%C3%A1ss", false), "páss");
        // A multi-line field keeps its line breaks, as \n whatever the browser sent.
        assert_eq!(form_decode("a%0D%0Ab%0Dc%0Ad", true), "a\nb\nc\nd");
        assert_eq!(form_decode("tab%09bell%07", true), "tab bell");
    }

    fn reply_form() -> Form {
        let mut f = Form::new("Reply");
        f.quote = "First line\n<b>second</b>".into();
        f.fields.push(Field::new("text", "Your reply").kind(Kind::Multiline).value("Draft </textarea> & more"));
        f.fields.push(Field::new("one", "One line"));
        f
    }

    #[test]
    fn multiline_fields_keep_line_breaks_and_render_as_a_text_box() {
        let f = reply_form();
        assert_eq!(f.fields[0].max_len, MULTILINE_MAX_LEN);
        assert_eq!(Field::new("x", "X").kind(Kind::Multiline).max_len(50).max_len, 50);
        let r = handle(&req("POST", "/t", "text=+Hi%0D%0A%0D%0Athere+%0D%0A&one=a%0D%0Ab"), "t", &f, &|_| Ok(()));
        let Reply::Done(v) = r else { panic!("{r:?}") };
        assert_eq!(v["text"], "Hi\n\nthere");
        assert_eq!(v["one"], "ab", "a one-line field still drops them");
        let Reply::Page(html) = handle(&req("GET", "/t", ""), "t", &f, &|_| Ok(())) else { panic!() };
        assert!(html.contains(r#"<textarea id="f0" name="text""#));
        assert!(html.contains(r#"maxlength="10000""#));
        assert!(html.contains(">Draft &lt;/textarea&gt; &amp; more</textarea>"), "the value is escaped text content");
        assert!(html.contains(r#"<input id="f1" name="one" type="text""#));
        assert!(html.contains("<blockquote>First line\n&lt;b&gt;second&lt;/b&gt;</blockquote>"));
    }

    #[test]
    fn a_whole_comment_fits_in_one_request() {
        let f = reply_form();
        // 10 000 four-byte characters, each sent as 12 bytes.
        let long = "%F0%9F%98%80".repeat(MULTILINE_MAX_LEN);
        assert!(long.len() + 1024 <= request_limit(&f));
        let p = PhoneForm::start_on(IpAddr::V4(Ipv4Addr::LOCALHOST), f, Box::new(|_: &Values| Ok(()))).unwrap();
        let (addr, path) = p.url.strip_prefix("http://").unwrap().split_once('/').unwrap();
        let post = |body: &str| http(addr, &format!("POST /{path} HTTP/1.1\r\nHost: tv\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(post(&format!("text={long}%F0%9F%98%80&one=")).contains("Too long (at most 10000 characters)"));
        assert!(post(&format!("text={long}&one=")).contains("Sent to the TV"));
        assert_eq!(p.take().unwrap()["text"].chars().count(), MULTILINE_MAX_LEN);
    }

    fn http(addr: &str, req: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn live_page_then_one_valid_submission() {
        let p = PhoneForm::start_on(IpAddr::V4(Ipv4Addr::LOCALHOST), form(), Box::new(ok_if_long)).unwrap();
        assert!(p.qr().is_some());
        let rest = p.url.strip_prefix("http://").unwrap();
        let (addr, path) = rest.split_once('/').unwrap();
        assert_eq!(path.len(), 32);
        assert!(!p.opened());
        let get = http(addr, &format!("GET /{path} HTTP/1.1\r\nHost: tv\r\n\r\n"));
        assert!(get.starts_with("HTTP/1.1 200") && get.contains("Send to TV"));
        assert!(p.opened());
        assert!(http(addr, "GET /nope HTTP/1.1\r\nHost: tv\r\n\r\n").starts_with("HTTP/1.1 404"));
        let post = |body: &str| {
            http(addr, &format!("POST /{path} HTTP/1.1\r\nHost: tv\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
        };
        // Invalid: the page comes back and the listener keeps running.
        assert!(post("client_id=short").contains("Too short"));
        assert!(p.take().is_none());
        assert!(post("client_id=abcdefghijklmnop&redirect_uri=").contains("Sent to the TV"));
        assert_eq!(p.take().unwrap()["client_id"], "abcdefghijklmnop");
        assert!(p.take().is_none(), "values are handed over once");
        std::thread::sleep(Duration::from_millis(400));
        assert!(!p.running());
        assert!(TcpStream::connect(addr).is_err(), "one submission only: the listener stopped");
    }

    #[test]
    fn short_address_and_caught_redirect() {
        let mut f = Form::new("Sign in");
        f.short = true;
        f.paste_submits = true;
        f.catch_redirect = Some("callback".into());
        f.action = Some(("Approve".into(), "https://www.reddit.com/x".into()));
        f.fields.push(Field::new("callback", "Address"));
        let check = |v: &Values| if v["callback"].contains("state=good") { Ok(()) } else { Err(vec![("callback".to_string(), "Wrong sign-in".to_string())]) };
        let p = PhoneForm::start_on(IpAddr::V4(Ipv4Addr::LOCALHOST), f.clone(), Box::new(check)).unwrap();
        let rest = p.url.strip_prefix("http://").unwrap();
        let (addr, path) = rest.split_once('/').unwrap();
        assert_eq!(path.len(), 5);
        assert!(path.bytes().all(|b| SHORT_ALPHABET.contains(&b)));
        let page = http(addr, &format!("GET /{path} HTTP/1.1\r\nHost: tv\r\n\r\n"));
        assert!(page.contains("Approve") && page.contains("paste") && page.contains(&format!(r#"action="/{path}""#)));
        // A redirect with someone else's state is refused (and shows the form), a matching one is taken.
        assert!(http(addr, "GET /cb?state=bad&code=x HTTP/1.1\r\nHost: tv\r\n\r\n").contains("Wrong sign-in"));
        assert!(http(addr, "GET /?code=x HTTP/1.1\r\nHost: tv\r\n\r\n").starts_with("HTTP/1.1 404"));
        assert!(http(addr, "GET /cb?state=good&code=abc HTTP/1.1\r\nHost: tv\r\n\r\n").contains("Sent to the TV"));
        assert_eq!(p.take().unwrap()["callback"], "http://tv/cb?state=good&code=abc");
        // Without catch_redirect such a request is a plain 404.
        f.catch_redirect = None;
        assert_eq!(handle(&req("GET", "/cb?state=good&code=abc", ""), "t", &f, &check), Reply::NotFound);
    }

    #[test]
    fn dropping_stops_the_listener() {
        let p = PhoneForm::start_on(IpAddr::V4(Ipv4Addr::LOCALHOST), form(), Box::new(ok_if_long)).unwrap();
        let addr = p.url.strip_prefix("http://").unwrap().split('/').next().unwrap().to_string();
        let running = p.running.clone();
        drop(p);
        std::thread::sleep(Duration::from_millis(400));
        assert!(!running.load(Ordering::SeqCst));
        assert!(TcpStream::connect(&addr).is_err());
    }
}
