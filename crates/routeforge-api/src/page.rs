//! The page: the one HTML file of `web/`, and the Leaflet it draws its map with,
//! served out of the binary (D52).
//!
//! The files are compiled in, as those of Swagger UI are (D43). So the API is one
//! file wherever it runs, and what it serves is what it was built with.

use actix_web::http::header;
use actix_web::{HttpResponse, web};

use crate::error::{self, ApiError};

/// A file of `web/`, as it is served.
struct File {
    /// Where it is under `web/`, which is also where it is served.
    path: &'static str,
    content_type: &'static str,
    bytes: &'static [u8],
}

macro_rules! file {
    ($path:literal, $content_type:literal) => {
        File {
            path: $path,
            content_type: $content_type,
            bytes: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/", $path)),
        }
    };
}

/// The page itself, which is served at `/`.
const PAGE: &str = "index.html";

/// Every file under `web/`. One that is added there has to be added here: a test walks
/// the directory and fails for a file that is not served.
static FILES: &[File] = &[
    file!("index.html", "text/html; charset=utf-8"),
    file!(
        "vendor/leaflet/leaflet.js",
        "text/javascript; charset=utf-8"
    ),
    file!("vendor/leaflet/leaflet.css", "text/css; charset=utf-8"),
    // Leaflet's licence asks that it goes wherever the code goes.
    file!("vendor/leaflet/LICENSE", "text/plain; charset=utf-8"),
    file!("vendor/leaflet/images/layers.png", "image/png"),
    file!("vendor/leaflet/images/layers-2x.png", "image/png"),
    file!("vendor/leaflet/images/marker-icon.png", "image/png"),
    file!("vendor/leaflet/images/marker-icon-2x.png", "image/png"),
    file!("vendor/leaflet/images/marker-shadow.png", "image/png"),
];

/// Adds the page at `/` and what it loads under `/vendor/`. Neither asks for a token:
/// the page is where one is got.
pub(crate) fn configure(config: &mut web::ServiceConfig) {
    config
        .route("/", web::get().to(page))
        .route("/vendor/{path:.*}", web::get().to(vendored));
}

async fn page() -> Result<HttpResponse, ApiError> {
    serve(PAGE)
}

async fn vendored(path: web::Path<String>) -> Result<HttpResponse, ApiError> {
    serve(&format!("vendor/{path}"))
}

fn serve(path: &str) -> Result<HttpResponse, ApiError> {
    let file = FILES
        .iter()
        .find(|file| file.path == path)
        .ok_or(error::NOTHING_HERE)?;
    Ok(HttpResponse::Ok()
        .content_type(file.content_type)
        // Asked for again each time. The files come from this machine or the next, so
        // that costs nothing, and a page that was changed is the page that is shown.
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .body(file.bytes))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use actix_web::http::StatusCode;
    use sqlx::PgPool;

    use super::*;
    use crate::error::PROBLEM_JSON;
    use crate::testing;

    /// Where `web/` is on the machine the tests run on.
    fn web() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web")
    }

    /// Every file under `directory`, as its path from there with forward slashes.
    fn files_under(directory: &Path, prefix: &str, found: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().unwrap().is_dir() {
                files_under(&entry.path(), &format!("{name}/"), found);
            } else {
                found.insert(name);
            }
        }
    }

    #[test]
    fn every_file_under_web_is_in_the_table_and_no_other() {
        let mut on_disk = BTreeSet::new();
        files_under(&web(), "", &mut on_disk);
        let served: BTreeSet<String> = FILES.iter().map(|file| file.path.to_owned()).collect();
        assert_eq!(served.len(), FILES.len(), "a file is there twice");
        assert_eq!(served, on_disk);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_page_and_what_it_loads_are_served_as_they_are_on_disk(pool: PgPool) {
        let api = testing::api(&pool);

        // Without a token: the page is where a token is got.
        let page = api.get(None, "/").await;
        assert_eq!(page.status, StatusCode::OK);
        assert_eq!(
            page.content_type.as_deref(),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(page.headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");
        assert!(
            page.text.starts_with("<!doctype html>"),
            "{}",
            &page.text[..40]
        );

        for file in FILES {
            let url = if file.path == PAGE {
                "/".to_owned()
            } else {
                format!("/{}", file.path)
            };
            let served = api.get(None, &url).await;
            assert_eq!(served.status, StatusCode::OK, "{url}");
            assert_eq!(served.content_type.as_deref(), Some(file.content_type));
            let on_disk = std::fs::read(web().join(file.path)).unwrap();
            assert!(!on_disk.is_empty(), "{url}");
            assert!(served.bytes == on_disk, "{url} is not the file on disk");
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn what_is_not_of_the_page_is_not_served(pool: PgPool) {
        let api = testing::api(&pool);
        for path in [
            "/index.html",
            // The page is at `/` and nowhere else: `/vendor/` is for what it loads.
            "/vendor/index.html",
            "/vendor/",
            "/vendor/leaflet/leaflet.js.map",
            "/vendor/leaflet/../../Cargo.toml",
            "/vendor/leaflet/images",
        ] {
            let reply = api.get(None, path).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(reply.content_type.as_deref(), Some(PROBLEM_JSON), "{path}");
        }
        // The page is read, and nothing is sent to it.
        let posted = api.post(None, "/", serde_json::Value::Null).await;
        assert_eq!(posted.status, StatusCode::NOT_FOUND);
    }

    /// The demonstration works without the internet (D8). The page may ask another
    /// host for one thing, the tiles under the map, and draws without them.
    #[test]
    fn the_page_asks_no_other_host_for_anything_but_map_tiles() {
        let page = std::fs::read_to_string(web().join(PAGE)).unwrap();

        let mut elsewhere = BTreeSet::new();
        for scheme in ["http://", "https://"] {
            for (at, _) in page.match_indices(scheme) {
                let url: String = page[at..]
                    .chars()
                    .take_while(|letter| !matches!(letter, '"' | '\'' | '`' | '<' | ' ' | '\n'))
                    .collect();
                elsewhere.insert(url);
            }
        }
        let expected: BTreeSet<String> = [
            // The tiles, which a switch on the page turns off.
            "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            // A link in the map's corner, which is followed only by whoever clicks it.
            "https://www.openstreetmap.org/copyright",
        ]
        .map(str::to_owned)
        .into();
        assert_eq!(elsewhere, expected);

        // What it runs and what it is styled by come with it.
        assert!(page.contains(r#"<script src="vendor/leaflet/leaflet.js"></script>"#));
        assert!(page.contains(r#"<link rel="stylesheet" href="vendor/leaflet/leaflet.css">"#));
        assert_eq!(page.matches("<script src=").count(), 1);
        assert_eq!(page.matches("<link rel=\"stylesheet\"").count(), 1);
        // No font, image or frame from anywhere either.
        for loaded_from_elsewhere in ["@import", "<iframe", "<img", "url(http"] {
            assert!(
                !page.contains(loaded_from_elsewhere),
                "{loaded_from_elsewhere}"
            );
        }
    }
}
