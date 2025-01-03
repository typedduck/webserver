#![allow(clippy::multiple_crate_versions)]

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    sync::LazyLock,
};

use axum::Router;
use tokio::{net::TcpListener, signal};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[cfg(feature = "metrics")]
use axum::{extract::Request, middleware::Next, response::IntoResponse};

static ENV_PREFIX: LazyLock<String> = LazyLock::new(|| env!("CARGO_CRATE_NAME").to_uppercase());
static SERVER_LOG: LazyLock<String> = LazyLock::new(|| format!("{}_LOG", &*ENV_PREFIX));
static SERVER_ADDR: LazyLock<String> = LazyLock::new(|| format!("{}_ADDR", &*ENV_PREFIX));
static SERVER_PORT: LazyLock<String> = LazyLock::new(|| format!("{}_PORT", &*ENV_PREFIX));
static SERVER_DIR: LazyLock<String> = LazyLock::new(|| format!("{}_DIR", &*ENV_PREFIX));
static SERVER_404: LazyLock<String> = LazyLock::new(|| format!("{}_404", &*ENV_PREFIX));

const DEFAULT_DIR: &str = "public";
const DEFAULT_404: &str = "404.html";

#[cfg(feature = "metrics")]
static SERVER_METRICS: LazyLock<String> = LazyLock::new(|| format!("{}_METRICS", &*ENV_PREFIX));
#[cfg(feature = "metrics")]
static METRICS_ADDR: LazyLock<String> = LazyLock::new(|| "METRICS_ADDR".to_string());
#[cfg(feature = "metrics")]
static METRICS_PORT: LazyLock<String> = LazyLock::new(|| "METRICS_PORT".to_string());

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_env(&*SERVER_LOG).unwrap_or_else(|_| {
                format!("{}=warn,tower_http=warn", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    #[cfg(not(feature = "metrics"))]
    start_site_server().await;

    #[cfg(feature = "metrics")]
    {
        let collect_metrics = std::env::var(&*SERVER_METRICS).unwrap_or_else(|_| "true".into());
        match collect_metrics.to_lowercase().as_str() {
            "true" | "1" => {
                let (_site, _metrics) = tokio::join!(start_site_server(), start_metrics_server());
            }
            _ => {
                start_site_server().await;
            }
        }
    }
}

fn site_app() -> Router {
    let dir = std::env::var(&*SERVER_DIR).unwrap_or_else(|_| DEFAULT_DIR.into());
    tracing::info!("serving '{}'", dir);
    let service = ServeDir::new(&dir).append_index_html_on_directories(true);
    let mut file_404 = PathBuf::from(dir);

    file_404.push(std::env::var(&*SERVER_404).unwrap_or_else(|_| DEFAULT_404.into()));
    tracing::info!("serving 404 from '{}'", file_404.display());
    let service = service.fallback(ServeFile::new(&file_404));

    #[cfg(feature = "metrics")]
    let app = Router::new()
        .nest_service("/", service)
        .route_layer(axum::middleware::from_fn(track_metrics));
    #[cfg(not(feature = "metrics"))]
    let app = Router::new().nest_service("/", service);

    app
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
        .parse::<u16>()?;
    let addr = SocketAddr::from((addr, port));
    let listener = TcpListener::bind(addr).await?;

    tracing::info!("site listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, site_app().layer(TraceLayer::new_for_http()))
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
        .parse::<u16>()?;
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
    IpAddr(std::net::AddrParseError),
    Port(std::num::ParseIntError),
    Io(std::io::Error),
}

impl From<std::net::AddrParseError> for Error {
    fn from(e: std::net::AddrParseError) -> Self {
        Self::IpAddr(e)
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Self::Port(e)
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
            Self::IpAddr(e) => e.fmt(f),
            Self::Port(_) => write!(f, "port must be an integer between 1 and 65535"),
            Self::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IpAddr(e) => Some(e),
            Self::Port(e) => Some(e),
            Self::Io(e) => Some(e),
        }
    }
}
