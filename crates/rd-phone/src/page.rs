//! The phone page: small, self-contained HTML (scripts only for the "Show" toggle and sending on
//! paste), readable on a phone or a computer in light and dark mode.

use crate::{Form, Kind, Values};

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

const STYLE: &str = "\
:root{--bg:#f6f7f8;--fg:#1a1a1b;--muted:#5f6368;--card:#fff;--field:#fff;--border:#c9ccd1;--accent:#ff4500;--err:#c62828}\
@media (prefers-color-scheme:dark){:root{--bg:#0b0b0c;--fg:#e8eaed;--muted:#9aa0a6;--card:#1a1a1b;--field:#232325;--border:#3c4043;--err:#ff8a80}}\
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:17px/1.45 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif}\
main{max-width:34rem;margin:0 auto;padding:20px 16px 40px}h1{font-size:22px;margin:0 0 8px}p{margin:8px 0;color:var(--muted)}\
a{color:var(--accent);font-weight:600}.links{margin:10px 0 4px}.links a{display:inline-block;margin-right:16px}\
form{background:var(--card);border:1px solid var(--border);border-radius:14px;padding:16px;margin-top:14px}\
label{display:block;font-weight:600;margin:14px 0 6px}label:first-child{margin-top:0}\
input[type=text],input[type=url],input[type=password]{width:100%;font:16px ui-monospace,SFMono-Regular,Menlo,monospace;padding:12px;border-radius:10px;border:1px solid var(--border);background:var(--field);color:var(--fg)}\
input.bad{border-color:var(--err)}.help{font-size:14px;color:var(--muted);margin-top:4px}.err{font-size:14px;color:var(--err);margin-top:4px;font-weight:600}\
.show{font-weight:400;font-size:14px;color:var(--muted)}\
button{margin-top:18px;width:100%;font:600 18px system-ui,sans-serif;padding:14px;border:0;border-radius:999px;background:var(--accent);color:#fff}ol{margin:10px 0;padding-left:1.4em;color:var(--fg)}ol li{margin:6px 0}a.action{display:block;text-align:center;margin:16px 0 4px;padding:14px;border-radius:999px;background:var(--accent);color:#fff;text-decoration:none;font-size:18px}details{margin-top:12px;background:var(--card);border:1px solid var(--border);border-radius:12px;padding:10px 14px}summary{cursor:pointer;font-weight:600}details p{white-space:pre-line}\
.copy{display:flex;gap:8px;align-items:center;margin:6px 0 4px}.copy input{flex:1;min-width:0;font:13px ui-monospace,monospace;padding:9px;border-radius:10px;border:1px solid var(--border);background:var(--field);color:var(--muted)}\
.copy button{width:auto;margin:0;padding:9px 16px;font-size:15px;background:var(--card);color:var(--accent);border:1px solid var(--accent)}";

fn head(title: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="referrer" content="no-referrer"><title>{t}</title><style>{STYLE}</style></head><body><main>"#,
        t = escape(title)
    )
}

pub fn form(form: &Form, token: &str, values: &Values, errors: &[(String, String)]) -> String {
    let mut h = head(&form.title);
    h += &format!("<h1>{}</h1>", escape(&form.title));
    for p in &form.intro {
        h += &format!("<p>{}</p>", escape(p));
    }
    if !form.steps.is_empty() {
        h += "<ol>";
        for st in &form.steps {
            h += &format!("<li>{}</li>", escape(st));
        }
        h += "</ol>";
    }
    if let Some((label, url)) = form.action.as_ref().filter(|(_, u)| u.starts_with("https://")) {
        h += &format!(r#"<a class="action" id="act" href="{}" target="_blank" rel="noopener noreferrer">{}</a>"#, escape(url), escape(label));
        // The link as text with a copy button: an address pasted into the address bar stays in the
        // browser, where a tapped link may be taken by an installed app (the Reddit app on Android).
        h += &format!(
            r#"<div class="copy"><input id="acturl" type="text" readonly value="{}"><button type="button" id="copybtn">Copy link</button></div>"#,
            escape(url)
        );
        // On Android, open the link in Chrome explicitly: an installed app that claims the site
        // cannot take an intent addressed to Chrome. Elsewhere the plain link is used.
        h += r#"<script>(function(){var a=document.getElementById('act'),u=a.href;if(/Android/i.test(navigator.userAgent)){a.href='intent://'+u.replace(/^https:\/\//,'')+'#Intent;scheme=https;package=com.android.chrome;S.browser_fallback_url='+encodeURIComponent(u)+';end';a.removeAttribute('target')}var b=document.getElementById('copybtn'),t=document.getElementById('acturl');b.onclick=function(){t.focus();t.select();t.setSelectionRange(0,t.value.length);var ok=false;try{ok=document.execCommand('copy')}catch(e){}if(!ok&&navigator.clipboard){navigator.clipboard.writeText(t.value).then(function(){b.textContent='Copied'});return}b.textContent=ok?'Copied: paste it into the address bar':'Select the text above and copy it'}})()</script>"#;
    }
    if !form.links.is_empty() {
        h += r#"<div class="links">"#;
        for (label, url) in &form.links {
            // Only absolute https links (they are ours, but keep the page safe by construction).
            if url.starts_with("https://") {
                h += &format!(r#"<a href="{}" target="_blank" rel="noopener noreferrer">{}</a>"#, escape(url), escape(label));
            }
        }
        h += "</div>";
    }
    // The TV checks every field itself; `novalidate` stops the phone's browser from rejecting
    // custom-scheme redirect URIs in a `type="url"` input before they reach it.
    h += &format!(r#"<form id="f" method="post" action="/{}" autocomplete="off" novalidate>"#, escape(token));
    for (i, f) in form.fields.iter().enumerate() {
        let id = format!("f{i}");
        let value = values.get(&f.key).map(String::as_str).unwrap_or("");
        let err: Vec<&str> = errors.iter().filter(|(k, _)| *k == f.key).map(|(_, m)| m.as_str()).collect();
        let (ty, extra) = match f.kind {
            Kind::Text => ("text", ""),
            Kind::Url => ("url", r#" inputmode="url""#),
            Kind::Secret => ("password", ""),
        };
        h += &format!(r#"<label for="{id}">{}</label>"#, escape(&f.label));
        h += &format!(
            r#"<input id="{id}" name="{name}" type="{ty}"{extra} value="{value}" placeholder="{ph}" maxlength="{max}" autocapitalize="off" autocorrect="off" spellcheck="false"{bad}{focus}>"#,
            name = escape(&f.key),
            value = escape(value),
            ph = escape(&f.placeholder),
            max = f.max_len,
            bad = if err.is_empty() { "" } else { r#" class="bad""# },
            focus = if (errors.is_empty() && i == 0) || (!err.is_empty() && errors.first().map(|e| &e.0) == Some(&f.key)) { " autofocus" } else { "" },
        );
        if f.kind == Kind::Secret {
            h += &format!(
                r#"<label class="show"><input type="checkbox" onchange="document.getElementById('{id}').type=this.checked?'text':'password'"> Show</label>"#
            );
        }
        for m in err {
            h += &format!(r#"<div class="err">{}</div>"#, escape(m));
        }
        if !f.help.is_empty() {
            h += &format!(r#"<div class="help">{}</div>"#, escape(&f.help));
        }
    }
    h += &format!(r#"<button type="submit">{}</button></form>"#, escape(&form.submit));
    for (summary, text) in &form.notes {
        h += &format!("<details><summary>{}</summary><p>{}</p></details>", escape(summary), escape(text));
    }
    if form.paste_submits {
        // Pasting sends the form at once (the pasted text lands in the field first).
        h += r#"<script>document.querySelectorAll('#f input[type=text],#f input[type=url]').forEach(function(i){i.addEventListener('paste',function(){setTimeout(function(){if(i.value.trim())document.getElementById('f').submit()},50)})})</script>"#;
    }
    h += "</main></body></html>";
    h
}

pub fn done(form: &Form) -> String {
    let mut h = head(&form.title);
    h += &format!("<h1>{}</h1><p>{}</p></main></body></html>", escape(&form.done.0), escape(&form.done.1));
    h
}
