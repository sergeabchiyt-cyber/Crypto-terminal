use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{
        ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{
    connect_async,
    tungstenite::client::IntoClientRequest,
    tungstenite::Message as TungsteniteMessage,
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    nim_base: String,
    kucoin_base: String,
    phemex_ws: String,
    phemex_origin: String,
}

type AppError = (StatusCode, Json<Value>);

fn bad_gateway(err: impl std::fmt::Display) -> AppError {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "ok": false,
            "error": err.to_string(),
        })),
    )
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let nim_base = std::env::var("NIM_BASE")
        .unwrap_or_else(|_| "https://integrate.api.nvidia.com/v1".to_string());

    let kucoin_base = std::env::var("KUCOIN_BASE")
        .unwrap_or_else(|_| "https://api.kucoin.com".to_string());

    let phemex_ws = std::env::var("PHEMEX_WS")
        .unwrap_or_else(|_| "wss://vapi.phemex.com/ws".to_string());

    let phemex_origin = std::env::var("PHEMEX_ORIGIN")
        .unwrap_or_else(|_| "https://phemex.com".to_string());

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(5)
        .build()
        .expect("failed to build reqwest client");

    let state = AppState {
        client,
        nim_base,
        kucoin_base,
        phemex_ws,
        phemex_origin,
    };

    let cors = build_cors();

    let app = Router::new()
        .route("/", get(root))
        .route("/healthz", get(healthz))
        .route("/api/kucoin/bullet-public", post(kucoin_bullet))
        .route("/api/nim/chat/completions", post(nim_chat))
        .route("/api/ws/phemex", get(phemex_ws_handler))
        .layer(cors)
        .with_state(state);

    let addr = ("0.0.0.0", port);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind port");

    info!("Omni Stream Rust backend listening on {}", port);
    info!("Health: http://0.0.0.0:{}/healthz", port);

    axum::serve(listener, app)
        .await
        .expect("server failed");
}

fn build_cors() -> CorsLayer {
    let raw = std::env::var("ALLOWED_ORIGINS").unwrap_or_default();

    let origins: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if origins.is_empty() || origins.iter().any(|s| s == "*") {
        return CorsLayer::permissive();
    }

    let headers: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|s| HeaderValue::from_str(s).ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(headers))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

async fn root() -> Json<Value> {
    Json(json!({
        "ok": true,
        "service": "omni-stream-backend-rust",
        "health": "/healthz",
        "routes": {
            "kucoin": "/api/kucoin/bullet-public",
            "nim": "/api/nim/chat/completions",
            "phemex_ws": "/api/ws/phemex",
        }
    }))
}

async fn healthz() -> Json<Value> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Json(json!({
        "ok": true,
        "service": "omni-stream-backend-rust",
        "time": now,
        "routes": {
            "kucoin": "/api/kucoin/bullet-public",
            "nim": "/api/nim/chat/completions",
            "phemex_ws": "/api/ws/phemex",
        }
    }))
}

async fn kucoin_bullet(State(state): State<AppState>) -> Result<Response, AppError> {
    let url = format!("{}/api/v1/bullet-public", state.kucoin_base);

    let upstream = state
        .client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, "OmniStream/1.0")
        .body("{}")
        .send()
        .await
        .map_err(|e| bad_gateway(format!("KuCoin request failed: {e}")))?;

    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();

    let bytes = upstream
        .bytes()
        .await
        .map_err(|e| bad_gateway(format!("KuCoin body read failed: {e}")))?;

    let mut builder = Response::builder().status(status.as_u16());

    builder = match content_type {
        Some(ct) => builder.header(header::CONTENT_TYPE, ct),
        None => builder.header(header::CONTENT_TYPE, "application/json"),
    };

    builder = builder.header(header::CACHE_CONTROL, "no-store");

    let resp = builder
        .body(Body::from(bytes))
        .map_err(|e| bad_gateway(format!("response build failed: {e}")))?;

    Ok(resp)
}

async fn nim_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Response, AppError> {
    let url = format!("{}/chat/completions", state.nim_base);

    let mut req = state
        .client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "text/event-stream")
        .header(header::USER_AGENT, "OmniStream/1.0");

    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        req = req.header(header::AUTHORIZATION, auth);
    }

    let upstream = req
        .json(&payload)
        .send()
        .await
        .map_err(|e| bad_gateway(format!("NIM request failed: {e}")))?;

    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();

    let stream = upstream.bytes_stream();

    let mut builder = Response::builder().status(status.as_u16());

    builder = builder.header(
        header::CONTENT_TYPE,
        content_type.unwrap_or_else(|| HeaderValue::from_static("text/event-stream")),
    );

    builder = builder.header(header::CACHE_CONTROL, "no-cache, no-transform");
    builder = builder.header("X-Accel-Buffering", "no");

    let resp = builder
        .body(Body::from_stream(stream))
        .map_err(|e| bad_gateway(format!("response build failed: {e}")))?;

    Ok(resp)
}

async fn phemex_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_phemex(socket, state))
}

async fn handle_phemex(socket: WebSocket, state: AppState) {
    let mut request = match state.phemex_ws.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Phemex client request build failed: {e}");
            return;
        }
    };

    let origin = HeaderValue::from_str(&state.phemex_origin)
        .unwrap_or_else(|_| HeaderValue::from_static("https://phemex.com"));

    request.headers_mut().insert(header::ORIGIN, origin);
    request
        .headers_mut()
        .insert(header::USER_AGENT, HeaderValue::from_static("OmniStream/1.0"));

    let upstream = match connect_async(request).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            tracing::warn!("Phemex upstream WebSocket failed: {e}");
            return;
        }
    };

    let (mut client_sink, mut client_stream) = socket.split();
    let (mut upstream_sink, mut upstream_stream) = upstream.split();

    let client_to_upstream = async move {
        while let Some(Ok(msg)) = client_stream.next().await {
            let out = axum_to_tungstenite(msg);
            if upstream_sink.send(out).await.is_err() {
                break;
            }
        }
    };

    let upstream_to_client = async move {
        while let Some(Ok(msg)) = upstream_stream.next().await {
            let out = tungstenite_to_axum(msg);
            if client_sink.send(out).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = client_to_upstream => {},
        _ = upstream_to_client => {},
    }
}

fn axum_to_tungstenite(msg: AxumMessage) -> TungsteniteMessage {
    match msg {
        AxumMessage::Text(s) => TungsteniteMessage::Text(s),
        AxumMessage::Binary(b) => TungsteniteMessage::Binary(b),
        AxumMessage::Ping(p) => TungsteniteMessage::Ping(p),
        AxumMessage::Pong(p) => TungsteniteMessage::Pong(p),
        AxumMessage::Close(_) => TungsteniteMessage::Close(None),
    }
}

fn tungstenite_to_axum(msg: TungsteniteMessage) -> AxumMessage {
    match msg {
        TungsteniteMessage::Text(s) => AxumMessage::Text(s),
        TungsteniteMessage::Binary(b) => AxumMessage::Binary(b),
        TungsteniteMessage::Ping(p) => AxumMessage::Ping(p),
        TungsteniteMessage::Pong(p) => AxumMessage::Pong(p),
        TungsteniteMessage::Close(_) => AxumMessage::Close(None),
    }
}
