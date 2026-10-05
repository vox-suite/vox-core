use axum::{
    http::{HeaderValue, header},
    response::{Html, IntoResponse, Response},
};

const TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<meta name="robots" content="noindex">
<meta name="color-scheme" content="dark">
<title>{{TITLE}} · Vox</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500&family=Space+Grotesk:wght@500&display=swap" rel="stylesheet">
<style>
:root{--bg:#040506;--raised:#111214;--line:#232427;--line2:#2f3031;--text:#fff;--muted:#9c9c9d;--dim:#6a6b6c;--coral:#ff6363;--mint:#59d499;--accent:{{ACCENT}}}
*{box-sizing:border-box}
html,body{height:100%;margin:0}
body{background:var(--bg);color:var(--text);font-family:Inter,ui-sans-serif,system-ui,-apple-system,sans-serif;line-height:1.55;letter-spacing:-.02em;-webkit-font-smoothing:antialiased;display:flex;flex-direction:column;align-items:center;justify-content:center;padding:24px;position:relative;isolation:isolate;overflow:hidden}
body::before{content:"";position:absolute;inset:0;z-index:-2;background:radial-gradient(ellipse 60% 50% at 50% 0%,rgb(255 99 99/.22),transparent 70%),radial-gradient(ellipse 45% 40% at 50% 100%,color-mix(in srgb,var(--accent) 14%,transparent),transparent 70%)}
body::after{content:"";position:absolute;inset:0;z-index:-1;pointer-events:none;opacity:.09;mix-blend-mode:screen;background-image:url("data:image/svg+xml,%3Csvg viewBox='0 0 200 200' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.9' numOctaves='4' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");background-size:140px 140px}
.brand{position:absolute;top:32px;left:50%;transform:translateX(-50%);font-family:"Space Grotesk",sans-serif;font-weight:500;font-size:34px;letter-spacing:-.08em;line-height:1}
.card{width:100%;max-width:440px;background:linear-gradient(180deg,rgb(255 255 255/.04),rgb(255 255 255/.015));border:1px solid var(--line);border-radius:20px;padding:44px 36px 36px;text-align:center;box-shadow:0 30px 80px rgb(0 0 0/.45)}
.badge{width:64px;height:64px;margin:0 auto 26px;border-radius:50%;display:grid;place-items:center;background:color-mix(in srgb,var(--status) 12%,transparent);border:1px solid color-mix(in srgb,var(--status) 40%,transparent);box-shadow:0 0 0 8px color-mix(in srgb,var(--status) 6%,transparent)}
.ok{--status:var(--mint)}.fail{--status:var(--coral)}
.badge svg{width:28px;height:28px;stroke:var(--status);fill:none;stroke-width:2.4;stroke-linecap:round;stroke-linejoin:round}
.chip{display:inline-flex;align-items:center;gap:8px;font-size:12px;color:var(--muted);border:1px solid var(--line2);border-radius:9999px;padding:5px 12px;margin-bottom:18px}
.chip i{width:6px;height:6px;border-radius:50%;background:var(--accent);box-shadow:0 0 0 4px color-mix(in srgb,var(--accent) 18%,transparent)}
h1{font-family:"Space Grotesk",sans-serif;font-weight:500;font-size:clamp(30px,6vw,38px);line-height:1.1;letter-spacing:-.05em;margin:0 0 14px}
p{margin:0;color:var(--muted);font-size:15px}
.hint{margin-top:26px;padding-top:22px;border-top:1px solid var(--line);color:var(--dim);font-size:13px}
.powered{margin-top:14px;font-size:12px;color:var(--dim)}
.powered b{color:var(--muted);font-weight:500}
@media (prefers-reduced-motion:no-preference){.card{animation:rise .6s cubic-bezier(.2,.7,.2,1) both}.badge{animation:pop .7s .15s cubic-bezier(.2,.9,.3,1.3) both}}
@keyframes rise{from{opacity:0;transform:translateY(14px)}}
@keyframes pop{from{opacity:0;transform:scale(.6)}}
</style>
</head>
<body>
<div class="brand">vox</div>
<main class="card {{STATUS_CLASS}}">
<div class="badge" aria-hidden="true">{{ICON}}</div>
<div class="chip"><i></i>{{NAME}}</div>
<h1>{{TITLE}}</h1>
<p>{{BODY}}</p>
<p class="hint">{{HINT}}</p>
{{POWERED}}
</main>
</body>
</html>"##;

const CHECK: &str = r#"<svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>"#;
const CROSS: &str = r#"<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>"#;

fn provider(id: &str) -> (&'static str, &'static str) {
    match id {
        "spotify" => ("Spotify", "#1db954"),
        "youtube" => ("YouTube", "#ff0033"),
        "swiggy" => ("Swiggy", "#fc8019"),
        "zomato" => ("Zomato", "#cb202d"),
        "google" | "google_calendar" => ("Google Calendar", "#3c90ff"),
        _ => ("Your account", "#ff6363"),
    }
}

pub fn page(connector: &str, ok: bool) -> Response {
    let (name, accent) = provider(connector);
    let (title, body, hint, icon) = if ok {
        (
            if name == "Your account" {
                "You're connected".to_string()
            } else {
                format!("{name} is connected")
            },
            "Vox can now use this account, within the permissions you approved. You can change or disconnect it any time in Connected Apps.",
            "You can close this tab and return to Vox.",
            CHECK,
        )
    } else {
        (
            "That didn't go through".to_string(),
            "The authorization was cancelled, expired, or couldn't be verified. Nothing was connected.",
            "Return to Vox and try connecting again.",
            CROSS,
        )
    };
    let powered = if connector == "swiggy" {
        r#"<p class="powered">Powered by <b>Swiggy</b></p>"#
    } else {
        ""
    };
    let html = TEMPLATE
        .replace("{{TITLE}}", &title)
        .replace("{{NAME}}", name)
        .replace("{{BODY}}", body)
        .replace("{{HINT}}", hint)
        .replace("{{ICON}}", icon)
        .replace("{{ACCENT}}", accent)
        .replace("{{STATUS_CLASS}}", if ok { "ok" } else { "fail" })
        .replace("{{POWERED}}", powered);
    let mut response = if ok {
        Html(html).into_response()
    } else {
        (axum::http::StatusCode::BAD_REQUEST, Html(html)).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
