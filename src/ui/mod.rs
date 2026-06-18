//! Optional embedded web UI served under `/ui`.
//!
//! Gated behind the `ui` Cargo feature (on by default). Assets are embedded at
//! compile time via `rust-embed`; Vue itself loads from a CDN at runtime.

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::store::SharedState;

/// Embedded UI assets from `src/ui/assets/`.
#[derive(rust_embed::RustEmbed)]
#[folder = "src/ui/assets/"]
struct Asset;

/// Map a file path to a `Content-Type` based on its extension.
fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Serve an embedded asset by name, or 404 if it is missing.
fn serve(path: &str) -> Response {
    match Asset::get(path) {
        Some(file) => (
            [(header::CONTENT_TYPE, content_type_for(path))],
            Body::from(file.data.into_owned()),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// GET /ui — the SPA entry point.
async fn index() -> Response {
    serve("index.html")
}

/// GET /ui/assets/{file} — embedded static asset.
async fn asset(Path(file): Path<String>) -> Response {
    if file.contains('/') || file.contains("..") {
        return (StatusCode::BAD_REQUEST, "invalid asset path").into_response();
    }
    serve(&file)
}

/// Routes for the embedded web UI.
pub fn routes() -> Router<SharedState> {
    Router::new()
        .route("/ui", get(index))
        .route("/ui/", get(index))
        .route("/ui/assets/{file}", get(asset))
}

#[cfg(all(test, feature = "ui"))]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> Router {
        use crate::config::Config;
        use crate::store::{AppState, LogStore, MetricStore, TraceStore};
        use clap::Parser;
        use parking_lot::RwLock;
        use std::sync::Arc;
        use std::time::Instant;

        // AppState has no constructor — build the struct literal directly.
        // Config::parse_from(["aniani"]) yields the clap defaults. The UI
        // handlers never read state, so empty stores are fine.
        let state: SharedState = Arc::new(AppState {
            log_store: RwLock::new(LogStore::new()),
            metric_store: RwLock::new(MetricStore::new()),
            trace_store: RwLock::new(TraceStore::new()),
            config: Config::parse_from(["aniani"]),
            start_time: Instant::now(),
        });
        routes().with_state(state)
    }

    async fn get_resp(uri: &str) -> Response {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        app().oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn test_index_served_as_html() {
        let resp = get_resp("/ui").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("text/html"));
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn test_index_served_with_trailing_slash() {
        let resp = get_resp("/ui/").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("text/html"));
    }

    #[tokio::test]
    async fn test_app_js_served_as_javascript() {
        let resp = get_resp("/ui/assets/app.js").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("text/javascript"));
    }

    #[tokio::test]
    async fn test_style_css_served_as_css() {
        let resp = get_resp("/ui/assets/style.css").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("text/css"));
    }

    #[tokio::test]
    async fn test_unknown_asset_is_404() {
        let resp = get_resp("/ui/assets/nope.js").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_asset_path_traversal_rejected() {
        let resp = get_resp("/ui/assets/..%2f..%2fetc%2fpasswd").await;
        assert!(matches!(
            resp.status(),
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
        ));
    }
}
