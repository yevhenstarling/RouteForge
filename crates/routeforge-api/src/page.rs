//! The page: the one HTML file of `web/`, and the Leaflet it draws its map with,
//! served out of the binary (D52).
//!
//! The files are compiled in, as those of Swagger UI are (D43). So the API is one
//! file wherever it runs, and what it serves is what it was built with.
//!
//! The page is served with a content security policy (D69): the browser runs the
//! script that is in the page as it was built and Leaflet's, and no other.

use std::sync::LazyLock;

use actix_web::http::header;
use actix_web::{HttpResponse, web};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

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

/// Where the page's map tiles come from: the one other host it may ask (D8).
const TILE_HOST: &str = "https://tile.openstreetmap.org";

/// The policy the page is served with, made once from the page as it was built.
static POLICY: LazyLock<String> = LazyLock::new(|| {
    let page = FILES
        .iter()
        .find(|file| file.path == PAGE)
        .map_or(&[][..], |file| file.bytes);
    policy(&String::from_utf8_lossy(page))
});

/// The content security policy for a page (D69).
///
/// The page is one file, with its script and its style inside it (D52). A policy that
/// allowed "inline" script would allow any, an injected one as well, and one that
/// allowed none would stop the page. So the policy names the script and the style of
/// the page by their SHA-256. The browser runs what hashes to that, which is what was
/// built into this binary, and nothing that somebody got into the page later.
///
/// Everything else is from where the page is from: Leaflet's script, style and
/// images, and the API. The one other host is the tile server, for images alone.
/// `data:` is for the pictures Leaflet carries inside its own files.
fn policy(page: &str) -> String {
    let script = hash_of(inline(page, "<script>", "</script>"));
    let style = hash_of(inline(page, "<style>", "</style>"));
    [
        "default-src 'none'".to_owned(),
        format!("script-src 'self' '{script}'"),
        format!("style-src 'self' '{style}'"),
        format!("img-src 'self' data: {TILE_HOST}"),
        "connect-src 'self'".to_owned(),
        "base-uri 'none'".to_owned(),
        "form-action 'none'".to_owned(),
        "frame-ancestors 'none'".to_owned(),
    ]
    .join("; ")
}

/// What is between the first `open` and the `close` after it, to the byte: that is
/// what a browser hashes. Nothing, if the page has no such element.
fn inline<'a>(page: &'a str, open: &str, close: &str) -> &'a str {
    page.split_once(open)
        .and_then(|(_, rest)| rest.split_once(close))
        .map_or("", |(inside, _)| inside)
}

/// A source for a policy that allows exactly this text: `sha256-` and the hash.
fn hash_of(text: &str) -> String {
    format!(
        "sha256-{}",
        STANDARD.encode(Sha256::digest(text.as_bytes()))
    )
}

/// Adds the page at `/` and what it loads under `/vendor/`. Neither asks for a token:
/// the page is where one is got.
pub(crate) fn configure(config: &mut web::ServiceConfig) {
    config
        .route("/", web::get().to(page))
        .route("/vendor/{path:.*}", web::get().to(vendored));
}

async fn page() -> Result<HttpResponse, ApiError> {
    let mut page = serve(PAGE)?;
    let policy = header::HeaderValue::from_str(&POLICY).map_err(ApiError::internal)?;
    page.headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, policy);
    Ok(page)
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

    /// The example every description of the policy gives, and its hash as they give
    /// it: what is hashed is the text between the tags, to the byte.
    #[test]
    fn a_policy_names_the_hash_of_what_is_between_the_tags() {
        let page = "<!doctype html><style>b{}</style><script src=\"a.js\"></script>\
            <script>alert('Hello, world.');</script>";
        let policy = policy(page);
        assert!(
            policy.contains(
                "script-src 'self' 'sha256-qznLcsROx4GACP2dm0UCKCzCG+HiZ1guq6ZZDob/Tng='"
            ),
            "{policy}"
        );
        assert_eq!(inline(page, "<style>", "</style>"), "b{}");
        // One byte more is another script.
        let other = policy_of_script("alert('Hello, world.'); ");
        assert!(!other.contains("qznLcsROx4GACP2dm0UCKCzCG"), "{other}");
        // A page without either allows the script and the style that are nothing.
        let empty = "'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='";
        let bare = super::policy("<!doctype html><p>nothing</p>");
        assert!(
            bare.contains(&format!("script-src 'self' {empty}")),
            "{bare}"
        );
        assert!(
            bare.contains(&format!("style-src 'self' {empty}")),
            "{bare}"
        );
    }

    fn policy_of_script(script: &str) -> String {
        policy(&format!("<style></style><script>{script}</script>"))
    }

    /// What is between two tags, found here by another way than the one under test.
    fn between<'a>(page: &'a str, open: &str, close: &str) -> &'a str {
        let from = page.find(open).unwrap() + open.len();
        let to = from + page[from..].find(close).unwrap();
        &page[from..to]
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_page_comes_with_a_policy_that_allows_its_own_script_and_no_other(pool: PgPool) {
        let api = testing::api(&pool);
        let page = api.get(None, "/").await;
        let policy = page
            .headers
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("a policy")
            .to_str()
            .unwrap();

        // The script and the style of the page as it is on disk, which is the page
        // that was built in and is served.
        let on_disk = std::fs::read_to_string(web().join(PAGE)).unwrap();
        for (open, close, what) in [
            ("<script>", "</script>", "script-src"),
            ("<style>", "</style>", "style-src"),
        ] {
            let inside = between(&on_disk, open, close);
            assert!(inside.len() > 1_000, "{open} is not the page's own");
            let hash = STANDARD.encode(Sha256::digest(inside.as_bytes()));
            let allowed = format!("{what} 'self' 'sha256-{hash}'");
            assert!(policy.contains(&allowed), "{policy}");
        }

        // Nothing is allowed for being inline, for being made up as the page runs, or
        // for coming from anywhere at all.
        for loophole in ["unsafe-", "*", "http:", "blob:", "'none' 'self'"] {
            assert!(!policy.contains(loophole), "{loophole} in {policy}");
        }
        let directives: Vec<&str> = policy.split("; ").collect();
        assert_eq!(directives[0], "default-src 'none'");
        for locked in [
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(directives.contains(&locked), "{policy}");
        }
        assert!(directives.contains(&"connect-src 'self'"), "{policy}");
        // The one other host is the one the page's tiles come from, and it may send
        // images and nothing else.
        let tiles = "img-src 'self' data: https://tile.openstreetmap.org";
        assert!(directives.contains(&tiles), "{policy}");
        assert_eq!(policy.matches("https://").count(), 1, "{policy}");
        assert!(on_disk.contains("'https://tile.openstreetmap.org/{z}/{x}/{y}.png'"));

        // A policy is a document's. What the page loads comes without one.
        let script = api.get(None, "/vendor/leaflet/leaflet.js").await;
        assert!(
            script
                .headers
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none()
        );
    }

    /// The policy allows one script and one style by their hashes. So the page must
    /// have exactly those, and nothing else that runs or styles from inside the
    /// markup: an attribute is not something a hash can name.
    #[test]
    fn the_page_has_nothing_the_policy_would_need_an_exception_for() {
        let page = std::fs::read_to_string(web().join(PAGE)).unwrap();
        assert_eq!(page.matches("<script>").count(), 1);
        assert_eq!(
            page.matches("<script").count(),
            2,
            "Leaflet's, and the page's"
        );
        assert_eq!(page.matches("<style").count(), 1);

        // The markup is what comes before the page's script, less its style.
        let (markup, _) = page.split_once("<script>").unwrap();
        let markup = markup.replace(between(&page, "<style>", "</style>"), "");
        for inline in [" style=", "javascript:", "<base", "<object", "<embed"] {
            assert!(!markup.contains(inline), "{inline}");
        }
        // No handler in an attribute: ` onclick="`, ` onload="` and their kind.
        let handler = markup.split(" on").skip(1).find(|rest| {
            rest.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|letter| letter.is_ascii_lowercase())
            })
        });
        assert_eq!(handler, None);
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
