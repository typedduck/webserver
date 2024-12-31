#![allow(clippy::multiple_crate_versions)]

use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::LazyLock,
};

use axum::Router;
use tokio::net::TcpListener;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

static ENV_PREFIX: LazyLock<String> = LazyLock::new(|| env!("CARGO_CRATE_NAME").to_uppercase());
static SERVER_LOG: LazyLock<String> = LazyLock::new(|| format!("{}_LOG", &*ENV_PREFIX));
static SERVER_ADDR: LazyLock<String> = LazyLock::new(|| format!("{}_ADDR", &*ENV_PREFIX));
static SERVER_PORT: LazyLock<String> = LazyLock::new(|| format!("{}_PORT", &*ENV_PREFIX));
static SERVER_DIR: LazyLock<String> = LazyLock::new(|| format!("{}_DIR", &*ENV_PREFIX));

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

    if let Err(e) = serve(serve_dir()).await {
        tracing::error!("{}", e);
    }
}

fn serve_dir() -> Router {
    let dir = std::env::var(&*SERVER_DIR).unwrap_or_else(|_| "public".into());
    tracing::info!("serving '{}'", dir);
    let service = ServeDir::new(&dir).append_index_html_on_directories(true);
    Router::new().nest_service("/", service)
}

async fn serve(app: Router) -> Result<(), Error> {
    let addr =
        IpAddr::from_str(&std::env::var(&*SERVER_ADDR).unwrap_or_else(|_| "0.0.0.0".into()))?;
    let port = std::env::var(&*SERVER_PORT)
        .unwrap_or_else(|_| "8080".into())
        .parse::<u16>()?;
    let addr = SocketAddr::from((addr, port));
    let listener = TcpListener::bind(addr).await?;

    tracing::info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app.layer(TraceLayer::new_for_http()))
        .await
        .unwrap();
    Ok(())
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
