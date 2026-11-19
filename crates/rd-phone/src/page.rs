//! The phone page: small, self-contained HTML (no scripts except the "Show" toggle), readable on a
//! phone in light and dark mode.

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
button{margin-top:18px;width:100%;font:600 18px system-ui,sans-serif;padding:14px;border:0;border-radius:999px;background:var(--accent);color:#fff}";

fn head(title: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="referrer" content="no-referrer"><title>{t}</title><style>{STYLE}</style></head><body><main>"#,
        t = escape(title)
    )
}

pub fn form(form: &Form, values: &Values, errors: &[(String, String)]) -> String {
    let mut h = head(&form.title);
    h += &format!("<h1>{}</h1>", escape(&form.title));
    for p in &form.intro {
        h += &format!("<p>{}</p>", escape(p));
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
    h += r#"<form method="post" autocomplete="off">"#;
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
    h += &format!(r#"<button type="submit">{}</button></form></main></body></html>"#, escape(&form.submit));
    h
}

pub fn done(form: &Form) -> String {
    let mut h = head(&form.title);
    h += "<h1>Sent to the TV.</h1><p>You can close this page. The link no longer works.</p></main></body></html>";
    h
}
