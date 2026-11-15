use axum::{
    http::{header, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// Le front Vue, compilé dans `web/dist` et embarqué dans le binaire.
#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (path, file) = match Assets::get(path).filter(|_| !path.is_empty()) {
        Some(f) => (path, f),
        // Application monopage : toute route inconnue sert index.html.
        None => match Assets::get("index.html") {
            Some(f) => ("index.html", f),
            None => return "front absent du binaire (web/dist)".into_response(),
        },
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.into())],
        file.data,
    )
        .into_response()
}
