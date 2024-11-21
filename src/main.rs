mod cache;
mod config;
mod proxy;
mod request_context;

use std::future::Future;

use axum::{
    body::{to_bytes, Body},
    extract::{State, WebSocketUpgrade},
    http::{HeaderMap, Request},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use cache::CACHE;
use config::{Config, ConfigRoute};
use env_logger::Env;
use http::StatusCode;
use log::{debug, info};
use request_context::RequestContext;
use tokio::{fs::File, io::AsyncReadExt};
use tower_http::compression::CompressionLayer;

const CACHE_LIMIT: usize = 256 * 1024;

#[derive(Clone)]
struct AppState {
    config: Config,
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}

#[tokio::main]
async fn main() {
    let mut file = File::open("tinyweb.yaml")
        .await
        .expect("open tinyweb.yaml failed");
    let mut str = String::new();
    file.read_to_string(&mut str)
        .await
        .expect("read tinyweb.yaml failed");
    drop(file);

    let config: Config = serde_yaml::from_str(&str).expect("read tinyweb.yaml failed");

    let bind = config.bind.clone();
    info!("config: {config:#?}");

    env_logger::Builder::from_env(Env::default().default_filter_or(&config.log)).init();

    let state = AppState { config };

    let comression_layer: CompressionLayer = CompressionLayer::new()
        .br(true)
        .deflate(true)
        .gzip(true)
        .zstd(true);

    let app = Router::new()
        .route("/*path", any(handler))
        .with_state(state)
        .layer(comression_layer)
        .fallback(not_found);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .expect("server bind failed");

    info!("tinyweb start: {bind}");
    axum::serve(listener, app).await.expect("server run failed");
}

async fn fetch_from_cache<F, R>(key: String, cache: u64, f: F) -> Response<Body>
where
    F: FnOnce() -> R,
    R: Future<Output = anyhow::Result<Response<Body>>>,
{
    assert!(cache > 0);

    match CACHE.get(&key).await {
        Some(v) => {
            info!("cache hit: {}", key);
            (v.1, v.2, v.3).into_response()
        }
        None => {
            info!("cache not hit: {}", key);
            let result = f().await;
            return match result {
                Ok(r) => {
                    let status = r.status();
                    let headers = r.headers().clone();
                    let bytes = to_bytes(r.into_body(), usize::MAX).await.unwrap();
                    if bytes.len() <= CACHE_LIMIT {
                        CACHE
                            .insert(key, (cache, status, headers.clone(), bytes.clone()))
                            .await;
                    }
                    (status, headers, bytes).into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    HeaderMap::new(),
                    e.to_string(),
                )
                    .into_response(),
            };
        }
    }
}

// 新的处理函数，返回具体类型而不是 impl Future
async fn handler(
    State(state): State<AppState>,
    ws: Option<WebSocketUpgrade>,
    req: Request<Body>,
) -> Response<Body> {
    debug!("new req: {}", req.uri());

    let mut ctx = RequestContext { req, ws, cache: 0 };

    for route in &state.config.routes {
        if route.is_match(ctx.req.uri().path()) {
            debug!("directive: {} {:?}", ctx.req.uri().path(), route.directive);
            for directive in &route.directive {
                let directive = ConfigRoute::parse_directive(directive);
                if let Some(resp) = ctx.exec(directive).await {
                    return resp;
                }
            }
        }
    }
    (StatusCode::NOT_FOUND, HeaderMap::new(), "not found").into_response()
}
