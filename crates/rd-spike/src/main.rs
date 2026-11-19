//! Phase 0 spike: proves Reddit's OAuth flows with a user-supplied client ID and saves real
//! responses for rd-core's tests. Runs on a PC and, cross-built, on the TV.
//!
//! State (client ID, device ID, tokens) lives in `.rdt-spike.json` (git-ignored) or `$RDT_SPIKE_FILE`.
//! It holds a refresh token after `login`: treat it like a password.

use anyhow::{anyhow, bail, Context, Result};
use rd_core::auth::{self, Callback, ClientId, Grant, State};
use rd_core::comments::{self, Node};
use rd_core::config::{self, ApiConfig};
use rd_phone::{Field, Form, Kind, PhoneForm, Values};
use rd_core::model;
use rd_core::ratelimit::Priority;
use rd_core::session::{http_client, Identity, Saved, Session};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USAGE: &str = "rd-spike: Reddit API spike for rdtnative

USAGE:
  rd-spike setup <client-id> [--redirect-uri <uri>] [--user-agent <ua>]
                                    store your Reddit API settings (or set RDT_CLIENT_ID)
  rd-spike setup --phone            the same, entered on a phone: scan the QR code (same Wi-Fi)
  rd-spike token                    get an app-only token (browse without an account)
  rd-spike login [--phone]          sign in: open the printed URL, paste the URL you land on
                                    (--phone: paste it on your phone instead, via a QR code)
  rd-spike whoami                   show the stored identity
  rd-spike logout                   revoke the refresh token and forget it
  rd-spike get <path> [k=v ...]     GET oauth.reddit.com<path>, print JSON + rate limit
  rd-spike fixtures [dir] [sub]     save real responses (default dir spike-out/, sub 'pics')
  rd-spike soak <minutes> [secs]    fetch a listing every <secs> (default 10), log rate headers
";

fn state_path() -> PathBuf {
    std::env::var_os("RDT_SPIKE_FILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".rdt-spike.json"))
}

fn load() -> Result<Saved> {
    let p = state_path();
    let mut saved: Saved = match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("reading {}", p.display()))?,
        Err(_) => Saved::default(),
    };
    if let Ok(id) = std::env::var("RDT_CLIENT_ID") {
        if saved.client_id != id.trim() {
            saved = Saved { client_id: id.trim().to_string(), ..Saved::default() };
        }
    }
    if saved.client_id.is_empty() {
        bail!("no client ID: run `rd-spike setup <client-id>` or set RDT_CLIENT_ID");
    }
    if saved.device_id.is_empty() {
        saved.device_id = auth::new_device_id();
    }
    Ok(saved)
}

fn store(saved: &Saved) -> Result<()> {
    let p = state_path();
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(saved)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

fn session(saved: &Saved) -> Result<Session> {
    Session::from_saved(http_client(), saved, rd_core::ua::host_platform()).ok_or_else(|| anyhow!("stored client ID is malformed"))
}

fn rate_line(s: &Session) -> String {
    match s.last_rate() {
        Some(r) => format!("rate: used {} / remaining {} / reset in {}s", r.used, r.remaining, r.reset_secs),
        None => "rate: (no headers)".into(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "setup" => {
            let cfg = if args.get(1).map(String::as_str) == Some("--phone") {
                setup_on_phone()?
            } else {
                let mut client_id = None;
                let (mut redirect, mut ua) = (String::new(), String::new());
                let mut it = args[1..].iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "--redirect-uri" => redirect = it.next().cloned().ok_or_else(|| anyhow!("--redirect-uri needs a value"))?,
                        "--user-agent" => ua = it.next().cloned().ok_or_else(|| anyhow!("--user-agent needs a value"))?,
                        v if client_id.is_none() && !v.starts_with("--") => client_id = Some(v.to_string()),
                        v => bail!("unexpected argument {v:?}"),
                    }
                }
                let id = client_id.ok_or_else(|| anyhow!("usage: rd-spike setup <client-id> [--redirect-uri <uri>] [--user-agent <ua>] | --phone"))?;
                ApiConfig::from_fields(&id, &redirect, &ua).map_err(|e| {
                    anyhow!(e.iter().map(|e| format!("{}: {}", e.field, e.message)).collect::<Vec<_>>().join("; "))
                })?
            };
            let saved = Saved::from_config(&cfg);
            store(&saved)?;
            println!("stored in {}:", state_path().display());
            println!("  client ID     {}", cfg.client_id);
            println!("  redirect URI  {}", cfg.redirect_uri);
            println!("  user agent    {}", cfg.user_agent.as_deref().unwrap_or("(default)"));
            Ok(())
        }
        "token" => {
            let mut saved = load()?;
            saved.refresh_token = None;
            saved.username = None;
            saved.access_token = None;
            let s = session(&saved)?;
            let t0 = Instant::now();
            let t = s.renew()?;
            println!(
                "app-only token OK in {} ms: expires in {} s, scope '{}'",
                t0.elapsed().as_millis(),
                t.expires_at.saturating_sub(rd_core::now_secs()),
                t.scope
            );
            let r = s.get("/hot", &[("limit", "5")], Priority::Page)?;
            let l = model::parse_posts(&r.json)?;
            println!("/hot: {} posts, after={:?}", l.items.len(), l.after);
            for p in &l.items {
                println!("  r/{:<20} {:>6}  {}", p.subreddit, p.score, p.title);
            }
            println!("{}", rate_line(&s));
            store(&s.to_saved(&saved.device_id))
        }
        "login" => {
            let saved = load()?;
            let client = ClientId::parse(&saved.client_id).map_err(|e| anyhow!(e))?;
            let redirect = saved.redirect().to_string();
            let on_phone = args.get(1).map(String::as_str) == Some("--phone");
            let state = State { nonce: rd_core::random_token(16), tv: None };
            println!("1. Open this URL in a browser where you are logged in to Reddit:\n\n{}\n", auth::authorize_url(&client, &redirect, &state.encode()));
            let line = if on_phone {
                println!("2. Approve. Then scan this QR code and paste the URL you landed on (or the code):");
                let mut form = Form::new("rdtnative sign-in");
                form.intro.push("Paste the address of the page Reddit sent you to after you approved rdtnative, or just the code.".into());
                form.fields.push(Field::new("callback", "Address or code").kind(Kind::Url).max_len(2048));
                let v = run_phone_form(form, |v| match auth::parse_callback(&v["callback"]) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(vec![("callback".into(), format!("That does not look right: {e}"))]),
                })?;
                v["callback"].clone()
            } else {
                println!("2. Approve. Then paste the full URL you land on (or just the code) and press Enter:");
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                line
            };
            let (code, back) = match auth::parse_callback(&line).map_err(|e| anyhow!(e))? {
                Callback::Code { code, state } => (code, state),
                Callback::Denied { error, .. } => bail!("Reddit says: {error}"),
            };
            if !back.is_empty() && State::decode(&back).nonce != state.nonce {
                bail!("state mismatch: this callback belongs to another sign-in attempt");
            }
            let ua = rd_core::ua::user_agent(rd_core::ua::host_platform(), None);
            let t = auth::post_token(&http_client(), &ua, &client, &Grant::Code { code: &code, redirect_uri: &redirect })?;
            let refresh = t.refresh.clone().ok_or_else(|| anyhow!("no refresh token (duration=permanent ignored?)"))?;
            let mut user = Saved { refresh_token: Some(refresh), access_token: Some(t.access.clone()), expires_at: t.expires_at, ..saved };
            let s = session(&user)?;
            let name = s.fetch_username()?;
            user = s.to_saved(&user.device_id);
            store(&user)?;
            println!("signed in as u/{name}; scope '{}'. Refresh token saved in {}", t.scope, state_path().display());
            println!("{}", rate_line(&s));
            Ok(())
        }
        "whoami" => {
            let saved = load()?;
            let s = session(&saved)?;
            match s.identity() {
                Identity::AppOnly { device_id } => println!("app-only (device {device_id})"),
                Identity::User { .. } => println!("signed in as u/{}", s.fetch_username()?),
            }
            println!("user agent: {}", s.user_agent());
            store(&s.to_saved(&saved.device_id))
        }
        "logout" => {
            let saved = load()?;
            if let Some(rt) = &saved.refresh_token {
                let client = ClientId::parse(&saved.client_id).map_err(|e| anyhow!(e))?;
                let ua = rd_core::ua::user_agent(rd_core::ua::host_platform(), saved.username.as_deref());
                auth::revoke(&http_client(), &ua, &client, rt)?;
            }
            store(&Saved { client_id: saved.client_id, device_id: saved.device_id, ..Saved::default() })?;
            println!("signed out");
            Ok(())
        }
        "get" => {
            let path = args.get(1).ok_or_else(|| anyhow!("usage: rd-spike get <path> [k=v ...]"))?;
            let pairs: Vec<(String, String)> = args[2..]
                .iter()
                .map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or_else(|| anyhow!("expected k=v, got {kv}")))
                .collect::<Result<_>>()?;
            let q: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let saved = load()?;
            let s = session(&saved)?;
            let r = s.get(path, &q, Priority::Page)?;
            println!("{}", serde_json::to_string_pretty(&r.json)?);
            eprintln!("{}", rate_line(&s));
            store(&s.to_saved(&saved.device_id))
        }
        "fixtures" => {
            let dir = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("spike-out"));
            let sub = args.get(2).map(String::as_str).unwrap_or("pics");
            fixtures(&dir, sub)
        }
        "soak" => {
            let minutes: u64 = args.get(1).ok_or_else(|| anyhow!("usage: rd-spike soak <minutes> [secs]"))?.parse()?;
            let every: u64 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(10);
            soak(minutes, every)
        }
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

/// Fetches one of each response shape the app needs, saves them, and checks rd-core parses them.
fn fixtures(dir: &PathBuf, sub: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let saved = load()?;
    let s = session(&saved)?;
    let signed_in = s.is_signed_in();
    let save = |name: &str, v: &Value| -> Result<()> {
        let p = dir.join(name);
        std::fs::write(&p, serde_json::to_string_pretty(v)?)?;
        println!("  saved {} ({} bytes)", p.display(), std::fs::metadata(&p)?.len());
        Ok(())
    };
    let mut ok = 0;
    let mut failed = Vec::new();
    let mut step = |label: &str, r: Result<()>| match r {
        Ok(()) => ok += 1,
        Err(e) => {
            println!("  FAILED {label}: {e:#}");
            failed.push(label.to_string());
        }
    };

    let front = if signed_in { "/best" } else { "/hot" };
    let mut first_thread: Option<(String, String)> = None;
    step(front, (|| {
        let r = s.get(front, &[("limit", "25")], Priority::Page)?;
        let l = model::parse_posts(&r.json)?;
        println!("{front}: {} posts", l.items.len());
        save("front.json", &r.json)
    })());
    step("subreddit hot", (|| {
        let r = s.get(&format!("/r/{sub}/hot"), &[("limit", "25")], Priority::Page)?;
        let l = model::parse_posts(&r.json)?;
        println!("/r/{sub}/hot: {} posts; hints {:?}", l.items.len(), l.items.iter().filter_map(|p| p.post_hint.clone()).collect::<std::collections::BTreeSet<_>>());
        if let Some(p) = l.items.iter().filter(|p| !p.stickied).max_by_key(|p| p.num_comments) {
            first_thread = Some((p.id.clone(), p.name.clone()));
        }
        save("subreddit_hot.json", &r.json)
    })());
    step("r/all top/day", (|| {
        let r = s.get("/r/all/top", &[("t", "day"), ("limit", "25")], Priority::Page)?;
        let l = model::parse_posts(&r.json)?;
        println!("/r/all/top?t=day: {} posts, {} NSFW, {} video, {} gallery", l.items.len(),
            l.items.iter().filter(|p| p.over_18).count(), l.items.iter().filter(|p| p.is_video).count(), l.items.iter().filter(|p| p.is_gallery).count());
        save("all_top_day.json", &r.json)
    })());
    step("popular subreddits", (|| {
        let r = s.get("/subreddits/popular", &[("limit", "25")], Priority::Page)?;
        println!("/subreddits/popular: {} subreddits", model::parse_subreddits(&r.json)?.items.len());
        save("subreddits_popular.json", &r.json)
    })());
    step("search", (|| {
        let r = s.get("/search", &[("q", "webos"), ("limit", "10"), ("type", "link")], Priority::Page)?;
        println!("/search?q=webos: {} posts", model::parse_posts(&r.json)?.items.len());
        save("search.json", &r.json)
    })());
    if let Some((id, fullname)) = first_thread.clone() {
        step("thread + morechildren", (|| {
            let r = s.get(&format!("/comments/{id}"), &[("limit", "200"), ("sort", "confidence")], Priority::Page)?;
            let t = comments::parse_thread(&r.json)?;
            let rows = comments::flatten(&t.comments, &Default::default());
            println!("/comments/{id}: '{}' {} rows, {} comments incl. unfetched", t.post.title, rows.len(), comments::count(&t.comments));
            save("thread.json", &r.json)?;
            let more = find_more(&t.comments);
            match more {
                Some(m) => {
                    let (ids, rest) = comments::next_batch(&m);
                    let r = s.get("/api/morechildren", &comments::morechildren_query(&fullname, &ids, "confidence"), Priority::Page)?;
                    let things = comments::parse_morechildren(&r.json)?;
                    let mut tree = t.comments.clone();
                    comments::splice_more(&mut tree, &m.id, things.clone(), rest);
                    println!("/api/morechildren: {} things; tree now {} rows", things.len(), comments::flatten(&tree, &Default::default()).len());
                    save("morechildren.json", &r.json)
                }
                None => {
                    println!("(thread has no 'load more' row; morechildren skipped)");
                    Ok(())
                }
            }
        })());
    }
    if signed_in {
        step("subscriptions", (|| {
            let r = s.get("/subreddits/mine/subscriber", &[("limit", "100")], Priority::Page)?;
            println!("/subreddits/mine/subscriber: {} subreddits", model::parse_subreddits(&r.json)?.items.len());
            save("mine_subscriber.json", &r.json)
        })());
        step("me", (|| {
            let r = s.get("/api/v1/me", &[], Priority::Page)?;
            println!("/api/v1/me: u/{}", r.json["name"].as_str().unwrap_or("?"));
            Ok(())
        })());
    }
    println!("{}", rate_line(&s));
    store(&s.to_saved(&saved.device_id))?;
    println!("{ok} ok, {} failed {:?}", failed.len(), failed);
    if failed.is_empty() { Ok(()) } else { bail!("some fetches failed") }
}

fn find_more(nodes: &[Node]) -> Option<comments::More> {
    for n in nodes {
        match n {
            Node::More(m) if !m.is_continue_thread() => return Some(m.clone()),
            Node::Comment(c) => {
                if let Some(m) = find_more(&c.replies) {
                    return Some(m);
                }
            }
            _ => {}
        }
    }
    None
}

/// Polls a listing at a fixed interval and logs every rate-limit snapshot and error, to see how the
/// window behaves from this machine's IP (and the TV's).
fn soak(minutes: u64, every: u64) -> Result<()> {
    let saved = load()?;
    let s = session(&saved)?;
    let end = Instant::now() + Duration::from_secs(minutes * 60);
    let (mut ok, mut errs) = (0u32, 0u32);
    while Instant::now() < end {
        let t0 = Instant::now();
        match s.get("/r/all/new", &[("limit", "25")], Priority::Page) {
            Ok(r) => {
                ok += 1;
                let n = model::parse_posts(&r.json).map(|l| l.items.len()).unwrap_or(0);
                println!("{} ok {n} posts {} ms; {}", rd_core::now_secs(), t0.elapsed().as_millis(), rate_line(&s));
            }
            Err(e) => {
                errs += 1;
                println!("{} ERROR {e}", rd_core::now_secs());
            }
        }
        std::thread::sleep(Duration::from_secs(every).saturating_sub(t0.elapsed()));
    }
    store(&s.to_saved(&saved.device_id))?;
    println!("soak done: {ok} ok, {errs} errors");
    Ok(())
}

/// Runs a one-time phone form: prints the QR code and address, waits for one valid submission.
fn run_phone_form(form: Form, validate: impl Fn(&Values) -> Result<(), Vec<(String, String)>> + Send + Sync + 'static) -> Result<Values> {
    let minutes = form.lifetime.as_secs() / 60;
    let p = PhoneForm::start(form, Box::new(validate)).map_err(|e| anyhow!(e))?;
    if let Some(q) = p.qr() {
        println!("\n{}", q.to_terminal());
    }
    println!("Scan with a phone on the same network, or open {}", p.url);
    println!("(one-time link: works once and for {minutes} minutes)");
    let mut said_opened = false;
    loop {
        if let Some(v) = p.take() {
            println!("received from the phone");
            return Ok(v);
        }
        if !p.running() {
            bail!("the phone link expired");
        }
        if p.opened() && !said_opened {
            println!("opened on the phone…");
            said_opened = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The Reddit API settings form (client ID, redirect URI, user agent) on a phone.
fn setup_on_phone() -> Result<ApiConfig> {
    let current = load().ok().and_then(|s| {
        ApiConfig::from_fields(&s.client_id, s.redirect_uri.as_deref().unwrap_or(""), s.user_agent.as_deref().unwrap_or("")).ok()
    });
    let mut form = Form::new("rdtnative: Reddit API settings");
    form.intro = config::SETUP_STEPS.iter().map(|s| s.to_string()).collect();
    for f in config::api_fields(current.as_ref()) {
        form.fields.push(
            Field::new(f.key, f.label)
                .value(&f.value)
                .placeholder(&f.placeholder)
                .help(f.help)
                .kind(if f.url { Kind::Url } else { Kind::Text })
                .max_len(f.max_len),
        );
    }
    let validate = |v: &Values| {
        config::from_values(|k| v.get(k).cloned().unwrap_or_default())
            .map(|_| ())
            .map_err(|e| e.into_iter().map(|e| (e.field.to_string(), e.message)).collect())
    };
    let v = run_phone_form(form, validate)?;
    config::from_values(|k| v.get(k).cloned().unwrap_or_default()).map_err(|_| anyhow!("validated on the phone"))
}
