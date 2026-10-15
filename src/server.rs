use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use maud::Markup;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::client::{self, ServerInfo};
use crate::git::Git;
use crate::target::{DiffSpec, Target};
use crate::{diff, markdown, view, watch};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const TOKEN_HEADER: &str = "x-abeview-token";
const MAX_DOCUMENTS: usize = 5_000;

const PAGE_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; \
    base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

struct View {
    id: String,
    target: Target,
    title: String,
    events: broadcast::Sender<()>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl View {
    /// Directory that relative paths in URLs (images, documents) resolve against.
    fn root(&self) -> &Path {
        match &self.target {
            Target::MarkdownFile { path } => path.parent().unwrap_or(Path::new("/")),
            Target::MarkdownDir { path } => path,
            Target::Diff { repo, .. } => repo,
        }
    }
}

struct AppState {
    token: String,
    port: u16,
    views: Mutex<HashMap<String, Arc<View>>>,
    clients: AtomicUsize,
    last_activity: Mutex<Instant>,
}

#[derive(Serialize, Deserialize)]
pub struct OpenResponse {
    pub url: String,
}

pub fn run() -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(serve())
}

async fn serve() -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let info = ServerInfo {
        port,
        token: random_hex(32),
        pid: std::process::id(),
    };
    client::write_state(&info)?;

    let state = Arc::new(AppState {
        token: info.token.clone(),
        port,
        views: Mutex::new(HashMap::new()),
        clients: AtomicUsize::new(0),
        last_activity: Mutex::new(Instant::now()),
    });
    tokio::spawn(shutdown_when_idle(state.clone()));

    axum::serve(listener, router(state)).await?;
    Ok(())
}

fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/views", post(create_view))
        .route("/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .nest("/api", api)
        .route("/v/{id}", get(page))
        .route("/v/{id}/content", get(content))
        .route("/v/{id}/events", get(events))
        .route("/v/{id}/preview", get(preview))
        .route("/v/{id}/raw/{*path}", get(raw))
        .route("/assets/{name}", get(asset))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Rejects requests whose Host is not our loopback address. Without this a
/// web page could rebind its own domain to 127.0.0.1 and read our pages.
async fn guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    let allowed = [
        format!("127.0.0.1:{}", state.port),
        format!("localhost:{}", state.port),
    ];
    if !host.is_some_and(|h| allowed.iter().any(|a| a == h)) {
        return (StatusCode::FORBIDDEN, "invalid host").into_response();
    }
    *state.last_activity.lock().unwrap() = Instant::now();

    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(PAGE_CSP));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    res
}

/// Browsers cannot attach a custom header cross-origin without a CORS
/// preflight, which we never answer, so web pages cannot call the API.
async fn require_token(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let given = req
        .headers()
        .get(TOKEN_HEADER)
        .map(|v| v.as_bytes())
        .unwrap_or_default();
    if !constant_time_eq(given, state.token.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    next.run(req).await
}

async fn create_view(
    State(state): State<Arc<AppState>>,
    Json(target): Json<Target>,
) -> Result<Json<OpenResponse>, AppError> {
    validate_target(&target)?;
    let existing = state
        .views
        .lock()
        .unwrap()
        .values()
        .find(|v| v.target == target)
        .map(|v| v.id.clone());
    let id = match existing {
        Some(id) => id,
        None => {
            let (events, _) = broadcast::channel(16);
            let watcher = watch::spawn(&target, events.clone())
                .map_err(|e| eprintln!("abeview: file watching disabled: {e:#}"))
                .ok();
            let view = View {
                id: random_hex(16),
                title: title_for(&target),
                target,
                events,
                _watcher: watcher,
            };
            let id = view.id.clone();
            state
                .views
                .lock()
                .unwrap()
                .insert(id.clone(), Arc::new(view));
            id
        }
    };
    Ok(Json(OpenResponse {
        url: format!("http://127.0.0.1:{}/v/{id}", state.port),
    }))
}

async fn shutdown(State(_): State<Arc<AppState>>) -> &'static str {
    client::remove_state_if_owned(std::process::id());
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        exit_server();
    });
    "bye"
}

#[derive(Deserialize)]
struct FileQuery {
    file: Option<String>,
}

async fn page(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<FileQuery>,
) -> Result<Html<String>, AppError> {
    let view = find_view(&state, &id)?;
    let kind = match view.target {
        Target::Diff { .. } => view::Kind::Diff,
        _ => view::Kind::Markdown,
    };
    let content = render_blocking(view.clone(), q.file).await;
    Ok(Html(
        view::page(&view.id, kind, &view.title, content).into_string(),
    ))
}

async fn content(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<FileQuery>,
) -> Result<Html<String>, AppError> {
    let view = find_view(&state, &id)?;
    Ok(Html(render_blocking(view, q.file).await.into_string()))
}

async fn render_blocking(view: Arc<View>, file: Option<String>) -> Markup {
    tokio::task::spawn_blocking(move || {
        render_content(&view, file.as_deref())
            .unwrap_or_else(|e| view::error_content(&format!("{e:#}")))
    })
    .await
    .unwrap_or_else(|e| view::error_content(&e.to_string()))
}

fn render_content(view: &View, file: Option<&str>) -> Result<Markup> {
    match &view.target {
        Target::Diff { repo, spec } => {
            let files = diff::parse(&Git::new(repo).diff(spec)?);
            Ok(view::diff_content(&files))
        }
        Target::MarkdownFile { path } => {
            let source = read_text(path)?;
            Ok(view::markdown_content(
                None,
                &render_markdown(view, &source, ""),
            ))
        }
        Target::MarkdownDir { path } => {
            let docs = list_documents(path);
            let current = match file {
                Some(f) => Some(f.to_string()),
                None => docs
                    .iter()
                    .find(|d| d.eq_ignore_ascii_case("README.md"))
                    .or(docs.first())
                    .cloned(),
            };
            let body = match &current {
                Some(rel) => {
                    if !docs.contains(rel) {
                        bail!("not a document in this directory: {rel}");
                    }
                    let source = read_text(&path.join(rel))?;
                    let dir = Path::new(rel).parent().unwrap_or(Path::new(""));
                    render_markdown(view, &source, &dir.to_string_lossy())
                }
                None => "<p>No Markdown files found.</p>".to_string(),
            };
            Ok(view::markdown_content(
                Some((&docs, current.as_deref())),
                &body,
            ))
        }
    }
}

fn render_markdown(view: &View, source: &str, doc_dir: &str) -> String {
    let prefix = format!("/v/{}/raw/", view.id);
    markdown::render(source, Path::new(doc_dir), move |p| {
        format!("{prefix}{}", view::percent_encode(p))
    })
}

fn list_documents(dir: &Path) -> Vec<String> {
    let mut docs: Vec<String> = ignore::WalkBuilder::new(dir)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            let rel = e.path().strip_prefix(dir).ok()?.to_str()?.to_string();
            view::is_markdown(&rel).then_some(rel)
        })
        .take(MAX_DOCUMENTS)
        .collect();
    docs.sort();
    docs
}

#[derive(Deserialize)]
struct PreviewQuery {
    path: String,
}

async fn preview(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<PreviewQuery>,
) -> Result<Html<String>, AppError> {
    let view = find_view(&state, &id)?;
    let Target::Diff { repo, spec } = view.target.clone() else {
        return Err(AppError::not_found());
    };
    if !view::is_markdown(&q.path) || !is_plain_relative(&q.path) {
        return Err(AppError::bad_request("not a Markdown path"));
    }
    // A symlink in the working tree could otherwise point outside the repository.
    if spec == DiffSpec::WorkingTree && safe_join(&repo, &q.path).is_none() {
        return Err(AppError::not_found());
    }
    let html = tokio::task::spawn_blocking(move || -> Result<String> {
        let bytes = Git::new(&repo).new_side_content(&spec, &q.path)?;
        let source = String::from_utf8_lossy(&bytes);
        let dir = Path::new(&q.path).parent().unwrap_or(Path::new(""));
        Ok(render_markdown(&view, &source, &dir.to_string_lossy()))
    })
    .await
    .map_err(|e| AppError::internal(e.into()))?
    .map_err(AppError::internal)?;
    Ok(Html(html))
}

async fn events(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let view = find_view(&state, &id)?;
    let guard = ClientGuard::new(state.clone());
    let stream = BroadcastStream::new(view.events.subscribe()).map(move |_| {
        let _keep_alive_while_streaming = &guard;
        Ok(Event::default().event("reload").data(""))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn raw(
    State(state): State<Arc<AppState>>,
    UrlPath((id, path)): UrlPath<(String, String)>,
) -> Result<Response, AppError> {
    let view = find_view(&state, &id)?;
    let ext = Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let content_type = image_content_type(&ext).ok_or_else(AppError::not_found)?;
    let full = safe_join(view.root(), &path).ok_or_else(AppError::not_found)?;
    let bytes = tokio::fs::read(&full)
        .await
        .map_err(|_| AppError::not_found())?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            // An SVG opened directly would otherwise run its scripts in our origin.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            ),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        bytes,
    )
        .into_response())
}

async fn asset(UrlPath(name): UrlPath<String>) -> Response {
    let (body, content_type): (&'static [u8], &str) = match name.as_str() {
        "app.css" => (
            include_bytes!("../assets/app.css"),
            "text/css; charset=utf-8",
        ),
        "app.js" => (
            include_bytes!("../assets/app.js"),
            "text/javascript; charset=utf-8",
        ),
        "github-markdown.css" => (
            include_bytes!("../assets/github-markdown.css"),
            "text/css; charset=utf-8",
        ),
        "mermaid.min.js" => (
            include_bytes!("../assets/mermaid.min.js"),
            "text/javascript; charset=utf-8",
        ),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

pub fn image_content_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

fn is_plain_relative(rel: &str) -> bool {
    let path = Path::new(rel);
    !rel.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Joins `rel` onto `root` only if the result, after resolving symlinks,
/// still lies inside `root`.
fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    if !is_plain_relative(rel) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let full = root.join(rel).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

fn validate_target(target: &Target) -> Result<(), AppError> {
    let ok = match target {
        Target::MarkdownFile { path } => path.is_absolute() && path.is_file(),
        Target::MarkdownDir { path } | Target::Diff { repo: path, .. } => {
            path.is_absolute() && path.is_dir()
        }
    };
    if ok {
        Ok(())
    } else {
        Err(AppError::bad_request("target does not exist"))
    }
}

fn title_for(target: &Target) -> String {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    match target {
        Target::MarkdownFile { path } | Target::MarkdownDir { path } => name(path),
        Target::Diff { repo, spec } => format!("{}: {}", name(repo), spec.title()),
    }
}

fn read_text(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn find_view(state: &AppState, id: &str) -> Result<Arc<View>, AppError> {
    state
        .views
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(AppError::not_found)
}

struct ClientGuard(Arc<AppState>);

impl ClientGuard {
    fn new(state: Arc<AppState>) -> Self {
        state.clients.fetch_add(1, Ordering::SeqCst);
        Self(state)
    }
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.clients.fetch_sub(1, Ordering::SeqCst);
        *self.0.last_activity.lock().unwrap() = Instant::now();
    }
}

async fn shutdown_when_idle(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let idle = state.last_activity.lock().unwrap().elapsed();
        if state.clients.load(Ordering::SeqCst) == 0 && idle > IDLE_TIMEOUT {
            exit_server();
        }
    }
}

fn exit_server() -> ! {
    client::remove_state_if_owned(std::process::id());
    std::process::exit(0);
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct AppError(StatusCode, String);

impl AppError {
    fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, "not found".into())
    }
    fn bad_request(msg: &str) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn internal(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_stays_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.png"), "x").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", dir.path().join("link.png")).unwrap();

        assert!(safe_join(dir.path(), "sub/a.png").is_some());
        assert!(safe_join(dir.path(), "../outside.png").is_none());
        assert!(safe_join(dir.path(), "/etc/hosts").is_none());
        assert!(safe_join(dir.path(), "sub/../sub/a.png").is_none());
        assert!(safe_join(dir.path(), "link.png").is_none());
    }

    #[test]
    fn compares_tokens() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"ab", b"abc"));
    }

    #[test]
    fn lists_documents_respecting_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(dir.path().join(".ignore"), "node_modules/\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        std::fs::write(dir.path().join("docs/a.markdown"), "").unwrap();
        std::fs::write(dir.path().join("docs/b.txt"), "").unwrap();
        std::fs::write(dir.path().join("node_modules/x/c.md"), "").unwrap();
        assert_eq!(
            list_documents(dir.path()),
            vec!["README.md", "docs/a.markdown"]
        );
    }
}
