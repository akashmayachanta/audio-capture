//! The Svelte app (`frontend/dist`), embedded at build time by build.rs.

use http::{header, Response, StatusCode};
use include_dir::{include_dir, Dir};

static DIST: Dir<'static> = include_dir!("$OUT_DIR/frontend");

pub fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// `path` from the request URI (`/` is `index.html`).
pub fn serve(path: &str) -> Response<Vec<u8>> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    match DIST.get_file(rel) {
        Some(f) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type(rel))
            .header(header::CACHE_CONTROL, if rel == "index.html" { "no-store" } else { "max-age=31536000, immutable" })
            .body(f.contents().to_vec())
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(b"not found".to_vec())
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(content_type("assets/a.js"), "text/javascript");
        assert_eq!(content_type("a.css"), "text/css");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("x.svg"), "image/svg+xml");
        assert_eq!(content_type("x.bin"), "application/octet-stream");
    }

    #[test]
    fn root_serves_index_html() {
        let r = serve("/");
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "text/html; charset=utf-8");
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert!(String::from_utf8_lossy(r.body()).contains("<title>"));
    }

    #[test]
    fn unknown_asset_is_404() {
        assert_eq!(serve("/nope/missing.js").status(), 404);
    }
}
