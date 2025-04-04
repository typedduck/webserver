#![allow(clippy::multiple_crate_versions)]

use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    str::FromStr,
    sync::LazyLock,
    time::Duration,
};

use axum::Router;
use tokio::{net::TcpListener, signal};
use tower_http::{
    services::{ServeDir, ServeFile},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

#[cfg(feature = "metrics")]
use axum::{extract::Request, middleware::Next, response::IntoResponse};

static ENV_PREFIX: LazyLock<String> = LazyLock::new(|| env!("CARGO_CRATE_NAME").to_uppercase());
static SERVER_LOG: LazyLock<String> = LazyLock::new(|| format!("{}_LOG", &*ENV_PREFIX));
static SERVER_ADDR: LazyLock<String> = LazyLock::new(|| format!("{}_ADDR", &*ENV_PREFIX));
static SERVER_PORT: LazyLock<String> = LazyLock::new(|| format!("{}_PORT", &*ENV_PREFIX));
static SERVER_DIR: LazyLock<String> = LazyLock::new(|| format!("{}_DIR", &*ENV_PREFIX));
static SERVER_404: LazyLock<String> = LazyLock::new(|| format!("{}_404", &*ENV_PREFIX));
static SERVER_TIMEOUT: LazyLock<String> = LazyLock::new(|| format!("{}_TIMEOUT", &*ENV_PREFIX));

const DEFAULT_DIR: &str = "public";
const DEFAULT_404: &str = "404.html";
const DEFAULT_TIMEOUT: &str = "0"; // no timeout

#[cfg(feature = "metrics")]
static METRICS_ADDR: LazyLock<String> = LazyLock::new(|| "METRICS_ADDR".to_string());
#[cfg(feature = "metrics")]
static METRICS_PORT: LazyLock<String> = LazyLock::new(|| "METRICS_PORT".to_string());

#[tokio::main]
async fn main() {
    let level = tracing::Level::from_str(
        std::env::var(&*SERVER_LOG)
            .unwrap_or_else(|_| "warn".into())
            .as_str(),
    )
    .unwrap_or(tracing::Level::WARN);

    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .init();

    #[cfg(not(feature = "metrics"))]
    {
        start_site_server().await;
    }

    #[cfg(feature = "metrics")]
    {
        let (_site, _metrics) = tokio::join!(start_site_server(), start_metrics_server());
    }
}

#[allow(clippy::cognitive_complexity)]
fn site_app() -> Result<Router, Error> {
    let timeout = std::env::var(&*SERVER_TIMEOUT)
        .unwrap_or_else(|_| DEFAULT_TIMEOUT.into())
        .parse::<u64>()
        .map_err(Error::Timeout)?;
    let timeout = Duration::from_millis(timeout);
    let dir = std::env::var(&*SERVER_DIR).unwrap_or_else(|_| DEFAULT_DIR.into());
    let service = ServeDir::new(&dir).append_index_html_on_directories(true);
    let file_404 = std::env::var(&*SERVER_404).unwrap_or_else(|_| DEFAULT_404.into());
    let file_404 = Path::new(&dir).join(file_404);
    let file_index = Path::new(&dir).join("index.html");
    let service = service.not_found_service(ServeFile::new(&file_404));

    tracing::info!("serving '{}'", dir);
    tracing::info!("serving 404 from '{}'", file_404.display());
    tracing::info!("serving index from '{}'", file_index.display());

    let app = Router::new().route_service("/", ServeFile::new(&file_index));
    #[cfg(feature = "metrics")]
    let app = app.route_layer(axum::middleware::from_fn(track_metrics));
    let app = app.fallback_service(service);
    let app = if timeout > Duration::default() {
        tracing::info!("timeout: {} ms", timeout.as_millis());
        app.layer(TimeoutLayer::new(timeout))
    } else {
        app
    };

    Ok(app)
}

async fn start_site_server() {
    if let Err(e) = serve_site().await {
        tracing::error!("{}", e);
    }
}

async fn serve_site() -> Result<(), Error> {
    let addr =
        IpAddr::from_str(&std::env::var(&*SERVER_ADDR).unwrap_or_else(|_| "0.0.0.0".into()))?;
    let port = std::env::var(&*SERVER_PORT)
        .unwrap_or_else(|_| "8080".into())
        .parse::<u16>()
        .map_err(Error::Port)?;
    let addr = SocketAddr::from((addr, port));
    let listener = TcpListener::bind(addr).await?;
    let app = site_app()?.layer(TraceLayer::new_for_http());

    tracing::info!("site listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
    Ok(())
}

#[cfg(feature = "metrics")]
fn metrics_app() -> Router {
    use std::future::ready;

    use axum::routing::get;
    use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

    const EXPONENTIAL_SECONDS: &[f64] = &[
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ];

    let recorder_handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("http_requests_duration_seconds".to_string()),
            EXPONENTIAL_SECONDS,
        )
        .unwrap()
        .install_recorder()
        .unwrap();

    Router::new().route("/metrics", get(move || ready(recorder_handle.render())))
}

#[cfg(feature = "metrics")]
async fn start_metrics_server() {
    if let Err(e) = serve_metrics().await {
        tracing::error!("{}", e);
    }
}

#[cfg(feature = "metrics")]
async fn serve_metrics() -> Result<(), Error> {
    let addr =
        IpAddr::from_str(&std::env::var(&*METRICS_ADDR).unwrap_or_else(|_| "0.0.0.0".into()))?;
    let port = std::env::var(&*METRICS_PORT)
        .unwrap_or_else(|_| "8081".into())
        .parse::<u16>()
        .map_err(Error::Port)?;
    let addr = SocketAddr::from((addr, port));
    let listener = TcpListener::bind(addr).await?;

    tracing::info!("metrics listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, metrics_app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
    Ok(())
}

#[cfg(feature = "metrics")]
async fn track_metrics(req: Request, next: Next) -> impl IntoResponse {
    use std::time::Instant;

    use axum::extract::MatchedPath;

    let start = Instant::now();
    let path = req.extensions().get::<MatchedPath>().map_or_else(
        || req.uri().path().to_owned(),
        |matched_path| matched_path.as_str().to_owned(),
    );
    let method = req.method().clone();

    let response = next.run(req).await;

    let latency = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    let labels = [
        ("method", method.to_string()),
        ("path", path),
        ("status", status),
    ];

    metrics::counter!("http_requests_total", &labels).increment(1);
    metrics::histogram!("http_requests_duration_seconds", &labels).record(latency);

    response
}

#[allow(clippy::redundant_pub_crate)]
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[derive(Debug)]
enum Error {
    Io(std::io::Error),
    IpAddr(std::net::AddrParseError),
    Port(std::num::ParseIntError),
    Timeout(std::num::ParseIntError),
}

impl From<std::net::AddrParseError> for Error {
    fn from(e: std::net::AddrParseError) -> Self {
        Self::IpAddr(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::IpAddr(e) => e.fmt(f),
            Self::Port(_) => write!(f, "port must be a positive integer (u16)"),
            Self::Timeout(_) => write!(f, "timeout must be a positive integer (u64)"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::IpAddr(e) => Some(e),
            Self::Port(e) | Self::Timeout(e) => Some(e),
        }
    }
}
