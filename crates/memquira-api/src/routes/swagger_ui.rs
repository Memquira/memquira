//! `GET /openapi.json` and the Swagger UI under `/docs`.
//!
//! Replaces `utoipa-swagger-ui` 9, whose build script pulled the vulnerable `zip` 3 crate. The UI
//! files are the frozen Swagger UI 5.32.14 `dist` (`assets/swagger-ui/`, provenance in its `README.md`), served with
//! the routes, status codes and content types the crate used: `/docs` redirects (303) to `/docs/`, `/docs/` is
//! `index.html`, `/docs/{file}` is one `dist` file and anything else is 404.

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

/// `(file, content type)` as `utoipa-swagger-ui` 9.0.2 resolved them through `mime_guess`.
const ASSETS: [(&str, &str, &[u8]); 19] = [
    (
        "favicon-16x16.png",
        "image/png",
        include_bytes!("../../assets/swagger-ui/favicon-16x16.png"),
    ),
    (
        "favicon-32x32.png",
        "image/png",
        include_bytes!("../../assets/swagger-ui/favicon-32x32.png"),
    ),
    (
        "index.css",
        "text/css",
        include_bytes!("../../assets/swagger-ui/index.css"),
    ),
    (
        "index.html",
        "text/html",
        include_bytes!("../../assets/swagger-ui/index.html"),
    ),
    (
        "oauth2-redirect.html",
        "text/html",
        include_bytes!("../../assets/swagger-ui/oauth2-redirect.html"),
    ),
    // Swagger UI 5.32.14: `oauth2-redirect.html` loads its script from this file instead of inline.
    (
        "oauth2-redirect.js",
        "text/javascript",
        include_bytes!("../../assets/swagger-ui/oauth2-redirect.js"),
    ),
    (
        "swagger-initializer.js",
        "text/javascript",
        include_bytes!("../../assets/swagger-ui/swagger-initializer.js"),
    ),
    (
        "swagger-ui-bundle.js",
        "text/javascript",
        include_bytes!("../../assets/swagger-ui/swagger-ui-bundle.js"),
    ),
    (
        "swagger-ui-bundle.js.map",
        "text/plain",
        include_bytes!("../../assets/swagger-ui/swagger-ui-bundle.js.map"),
    ),
    (
        "swagger-ui-es-bundle-core.js",
        "text/javascript",
        include_bytes!("../../assets/swagger-ui/swagger-ui-es-bundle-core.js"),
    ),
    (
        "swagger-ui-es-bundle-core.js.map",
        "text/plain",
        include_bytes!("../../assets/swagger-ui/swagger-ui-es-bundle-core.js.map"),
    ),
    (
        "swagger-ui-es-bundle.js",
        "text/javascript",
        include_bytes!("../../assets/swagger-ui/swagger-ui-es-bundle.js"),
    ),
    (
        "swagger-ui-es-bundle.js.map",
        "text/plain",
        include_bytes!("../../assets/swagger-ui/swagger-ui-es-bundle.js.map"),
    ),
    (
        "swagger-ui-standalone-preset.js",
        "text/javascript",
        include_bytes!("../../assets/swagger-ui/swagger-ui-standalone-preset.js"),
    ),
    (
        "swagger-ui-standalone-preset.js.map",
        "text/plain",
        include_bytes!("../../assets/swagger-ui/swagger-ui-standalone-preset.js.map"),
    ),
    (
        "swagger-ui.css",
        "text/css",
        include_bytes!("../../assets/swagger-ui/swagger-ui.css"),
    ),
    (
        "swagger-ui.css.map",
        "text/plain",
        include_bytes!("../../assets/swagger-ui/swagger-ui.css.map"),
    ),
    (
        "swagger-ui.js",
        "text/javascript",
        include_bytes!("../../assets/swagger-ui/swagger-ui.js"),
    ),
    (
        "swagger-ui.js.map",
        "text/plain",
        include_bytes!("../../assets/swagger-ui/swagger-ui.js.map"),
    ),
];

/// The `/openapi.json` and `/docs` routes. The document is serialised once, compact, exactly as `axum::Json` did.
pub fn router<S>(openapi: &utoipa::openapi::OpenApi) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let document =
        Bytes::from(serde_json::to_vec(openapi).expect("the OpenAPI document serialises to JSON"));
    Router::new()
        .route(
            "/openapi.json",
            get(move || {
                let document = document.clone();
                async move {
                    (
                        [(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        )],
                        document,
                    )
                }
            }),
        )
        .route("/docs", get(|| async { Redirect::to("/docs/") }))
        .route("/docs/", get(serve_swagger_ui))
        .route("/docs/{*rest}", get(serve_swagger_ui))
}

async fn serve_swagger_ui(path: Option<Path<String>>) -> Response {
    let tail = path.as_ref().map_or("", |tail| tail.as_str());
    let file = if tail.is_empty() || tail == "/" {
        "index.html"
    } else {
        tail
    };
    match ASSETS.iter().find(|(name, _, _)| *name == file) {
        Some((_, content_type, bytes)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, *content_type)],
            *bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::ASSETS;
    use sha2::{Digest, Sha256};

    /// Every embedded file matches `assets/swagger-ui/SHA256SUMS` (the frozen asset's provenance record).
    #[test]
    fn embedded_assets_match_the_recorded_hashes() {
        let sums = include_str!("../../assets/swagger-ui/SHA256SUMS");
        for (name, _, bytes) in ASSETS {
            let expected = sums
                .lines()
                .find_map(|line| line.strip_suffix(name).and_then(|h| h.strip_suffix("  ")))
                .unwrap_or_else(|| panic!("{name} missing from SHA256SUMS"));
            assert_eq!(hex::encode(Sha256::digest(bytes)), expected, "{name}");
        }
    }
}
