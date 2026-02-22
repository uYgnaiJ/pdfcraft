use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Request, StatusCode, Uri},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};
use percent_encoding::percent_decode_str;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use tauri::async_runtime::JoinHandle;
use tokio::sync::oneshot;
use url::Url;

#[derive(Clone)]
struct ServerConfig {
    out_dir: Arc<PathBuf>,
}

struct ServerState {
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
}

struct RunningServer {
    url: String,
    shutdown_tx: oneshot::Sender<()>,
    join_handle: JoinHandle<()>,
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let running = tauri::async_runtime::block_on(start_local_server(app.handle().clone()))?;
            let url = Url::parse(&running.url).map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

            app.manage(ServerState {
                shutdown_tx: Mutex::new(Some(running.shutdown_tx)),
                join_handle: Mutex::new(Some(running.join_handle)),
            });

            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("PDFCraft")
                .inner_size(1280.0, 860.0)
                .min_inner_size(1000.0, 700.0)
                .build()
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            handle_run_events(app, &event);
        });
}

async fn start_local_server(app: AppHandle) -> Result<RunningServer, Box<dyn std::error::Error>> {
    let out_dir = resolve_out_dir(&app)?;

    if !out_dir.exists() {
        return Err(format!("Static export not found at {}. Run `npm run build` first.", out_dir.display()).into());
    }

    let state = ServerConfig {
        out_dir: Arc::new(out_dir),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://127.0.0.1:{}/en/", addr.port());
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let router = Router::new()
        .route("/*path", get(serve_file))
        .route("/", get(serve_file))
        .with_state(state)
        .layer(middleware::from_fn(add_response_headers));

    let join_handle = tauri::async_runtime::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });

        if let Err(error) = server.await {
            eprintln!("[pdfcraft-desktop] local server failed: {error}");
        }
    });

    Ok(RunningServer {
        url,
        shutdown_tx,
        join_handle,
    })
}

async fn serve_file(
    State(state): State<ServerConfig>,
    uri: Uri,
) -> Result<Response<Body>, (StatusCode, String)> {
    let request_path = decode_request_path(uri.path())?;
    let file_path = if request_path.is_empty() {
        state.out_dir.join("index.html")
    } else {
        state.out_dir.join(&request_path)
    };

    if is_file(&file_path) {
        return load_path_response(file_path).await;
    }

    let nested_index = state.out_dir.join(&request_path).join("index.html");
    if is_file(&nested_index) {
        return load_path_response(nested_index).await;
    }

    if looks_like_asset_request(&request_path) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Asset not found: {request_path}"),
        ));
    }

    load_path_response(state.out_dir.join("index.html")).await
}

fn decode_request_path(path: &str) -> Result<String, (StatusCode, String)> {
    let decoded = percent_decode_str(path)
        .decode_utf8()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid percent-encoded path".to_string()))?
        .trim_start_matches('/')
        .to_string();

    if decoded
        .split('/')
        .any(|segment| segment == ".." || segment.contains('\\'))
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid path".to_string()));
    }

    Ok(decoded)
}

fn looks_like_asset_request(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|name| name.contains('.'))
}

async fn load_path_response(path: PathBuf) -> Result<Response<Body>, (StatusCode, String)> {
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type_for_path(&path));
            Ok(response)
        }
        Err(error) => Err((
            StatusCode::NOT_FOUND,
            format!("Could not read {}: {error}", path.display()),
        )),
    }
}

async fn add_response_headers(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    headers.insert(
        "Cross-Origin-Opener-Policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        "Cross-Origin-Embedder-Policy",
        HeaderValue::from_static("require-corp"),
    );
    headers.insert(
        "Cross-Origin-Resource-Policy",
        HeaderValue::from_static("cross-origin"),
    );

    if path.ends_with(".wasm.gz") {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/wasm"));
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    } else if path.ends_with(".data.gz") {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    } else if path.ends_with(".wasm") {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/wasm"));
    } else if path.ends_with(".mjs") {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript"),
        );
    }

    response
}

fn content_type_for_path(path: &Path) -> HeaderValue {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or_default() {
        "html" => HeaderValue::from_static("text/html; charset=utf-8"),
        "js" => HeaderValue::from_static("application/javascript; charset=utf-8"),
        "mjs" => HeaderValue::from_static("application/javascript; charset=utf-8"),
        "css" => HeaderValue::from_static("text/css; charset=utf-8"),
        "json" => HeaderValue::from_static("application/json; charset=utf-8"),
        "svg" => HeaderValue::from_static("image/svg+xml"),
        "wasm" => HeaderValue::from_static("application/wasm"),
        "woff" => HeaderValue::from_static("font/woff"),
        "woff2" => HeaderValue::from_static("font/woff2"),
        "ttf" => HeaderValue::from_static("font/ttf"),
        "otf" => HeaderValue::from_static("font/otf"),
        "png" => HeaderValue::from_static("image/png"),
        "jpg" | "jpeg" => HeaderValue::from_static("image/jpeg"),
        "webp" => HeaderValue::from_static("image/webp"),
        "ico" => HeaderValue::from_static("image/x-icon"),
        _ => HeaderValue::from_static("application/octet-stream"),
    }
}

fn is_file(path: &Path) -> bool {
    path.metadata().map(|meta| meta.is_file()).unwrap_or(false)
}

fn resolve_out_dir(app: &AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(resource_dir) = app.path().resource_dir() {
        let bundled = resource_dir.join("out");
        if bundled.exists() {
            return Ok(bundled);
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_out = manifest_dir.join("..").join("out");
    if project_out.exists() {
        return Ok(project_out);
    }

    Err("Could not locate exported frontend assets (`out/`).".into())
}

fn shutdown_server(app: &AppHandle) {
    if let Some(state) = app.try_state::<ServerState>() {
        if let Some(tx) = state.shutdown_tx.lock().ok().and_then(|mut lock| lock.take()) {
            let _ = tx.send(());
        }

        if let Some(handle) = state.join_handle.lock().ok().and_then(|mut lock| lock.take()) {
            tauri::async_runtime::block_on(async {
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    let _ = handle.await;
                })
                .await;
            });
        }
    }
}

#[allow(clippy::single_match)]
fn handle_run_events(app: &AppHandle, event: &RunEvent) {
    match event {
        RunEvent::WindowEvent { event, .. } => {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                app.exit(0);
            }
        }
        RunEvent::ExitRequested { .. } => shutdown_server(app),
        _ => {}
    }
}
