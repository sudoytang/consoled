pub struct StaticFile {
    pub content_type: &'static str,
    pub body: &'static [u8],
    pub cache: bool,
}

const INDEX: &[u8] = include_bytes!("../frontend/index.html");
const APP_JS: &[u8] = include_bytes!("../frontend/app.js");
const APP_CSS: &[u8] = include_bytes!("../frontend/app.css");
const XTERM_JS: &[u8] = include_bytes!("../frontend/vendor/xterm.js");
const XTERM_CSS: &[u8] = include_bytes!("../frontend/vendor/xterm.css");
const FIT_JS: &[u8] = include_bytes!("../frontend/vendor/xterm-addon-fit.js");

pub fn lookup(path: &str) -> Option<StaticFile> {
    match path {
        "/" | "/index.html" => Some(StaticFile {
            content_type: "text/html; charset=utf-8",
            body: INDEX,
            cache: false,
        }),
        "/app.js" => Some(StaticFile {
            content_type: "text/javascript; charset=utf-8",
            body: APP_JS,
            cache: true,
        }),
        "/app.css" => Some(StaticFile {
            content_type: "text/css; charset=utf-8",
            body: APP_CSS,
            cache: true,
        }),
        "/xterm.js" => Some(StaticFile {
            content_type: "text/javascript; charset=utf-8",
            body: XTERM_JS,
            cache: true,
        }),
        "/xterm.css" => Some(StaticFile {
            content_type: "text/css; charset=utf-8",
            body: XTERM_CSS,
            cache: true,
        }),
        "/xterm-addon-fit.js" => Some(StaticFile {
            content_type: "text/javascript; charset=utf-8",
            body: FIT_JS,
            cache: true,
        }),
        _ => None,
    }
}

pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'none'; font-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
