//! The SPA and its static files, embedded in the binary from `assets/`.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

use super::error::ApiError;

#[derive(Embed)]
#[folder = "assets/"]
struct Assets;

/// `GET /`: the single page.
pub async fn index() -> Response {
    serve("index.html")
}

/// `GET /static/{path}`.
pub async fn static_file(Path(path): Path<String>) -> Response {
    serve(&path)
}

fn serve(path: &str) -> Response {
    match Assets::get(path) {
        Some(file) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type(path))],
            file.data.into_owned(),
        )
            .into_response(),
        None => ApiError::NotFound("Not found".into()).into_response(),
    }
}

/// The media type for a file name, with `charset=utf-8` on text types (as Starlette's
/// `StaticFiles` sends them).
pub fn content_type(path: &str) -> String {
    let mime = if path.ends_with(".js") || path.ends_with(".mjs") {
        "text/javascript".to_string()
    } else {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_string()
    };
    let text = mime.starts_with("text/")
        || mime == "application/json"
        || mime == "application/javascript"
        || mime == "image/svg+xml";
    if text {
        format!("{mime}; charset=utf-8")
    } else {
        mime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(content_type("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("app.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("x.png"), "image/png");
        assert_eq!(content_type("x.unknownext"), "application/octet-stream");
    }

    #[test]
    fn the_spa_is_embedded() {
        for name in ["index.html", "app.js", "app.css", "syntax.js"] {
            assert!(Assets::get(name).is_some(), "{name} missing from assets/");
        }
    }
}
