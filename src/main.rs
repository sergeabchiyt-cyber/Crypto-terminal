use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{
        ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::time::{Duration, MissedTickBehavior};
use tokio_stream::wrappers::ReceiverStream;
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
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, "OmniStream/1.0")
        .body("{}")
        .send()
        .await
        .map_err(|e| bad_gateway(format!("KuCoin request failed: {e}")))?;

    let status = upstream.status();

    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok());

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
    info!("NIM proxy: opening early SSE stream");

    let auth = headers
        .get(header::AUTHORIZATION)
        .map(|v| v.as_bytes().to_vec());

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);

    // Send something immediately so Render/reverse proxies do not timeout
    // while waiting for NVIDIA NIM's first byte.
    let _ = tx.send(Ok(b": connected\n\n".to_vec())).await;

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let mut req_fut = Box::pin(send_nim_request(state, auth, payload));

        // Wait for upstream response while sending heartbeat comments.
        let upstream_result = loop {
            tokio::select! {
                _ = interval.tick() => {
                    if tx.send(Ok(b": ping\n\n".to_vec())).await.is_err() {
                        return;
                    }
                }
                res = req_fut.as_mut() => break res,
            }
        };

        let upstream = match upstream_result {
            Ok(upstream) => upstream,
            Err(e) => {
                send_backend_error(&tx, &format!("NIM request failed: {e}")).await;
                return;
            }
        };

        let status = upstream.status();
        info!("NIM proxy: upstream status {}", status.as_u16());

        if !status.is_success() {
            let body = upstream.text().await.unwrap_or_default();
            send_backend_error(
                &tx,
                &format!(
                    "NIM returned HTTP {}: {}",
                    status.as_u16(),
                    truncate_for_error(&body)
                ),
            )
            .await;
            return;
        }

        let mut stream = upstream.bytes_stream();

        // Stream NIM bytes to the browser while continuing heartbeat.
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if tx.send(Ok(b": ping\n\n".to_vec())).await.is_err() {
                        return;
                    }
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            if tx.send(Ok(bytes.to_vec())).await.is_err() {
                                return;
                            }
                        }
                        Some(Err(e)) => {
                            send_backend_error(&tx, &format!("NIM stream error: {e}")).await;
                            return;
                        }
                        None => break,
                    }
                }
            }
        }
    });

    let body_stream = ReceiverStream::new(rx).map(|item| item.map(bytes::Bytes::from));

    let resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body_stream))
        .map_err(|e| bad_gateway(format!("response build failed: {e}")))?;

    Ok(resp)
}

async fn send_nim_request(
    state: AppState,
    auth: Option<Vec<u8>>,
    payload: Value,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = format!("{}/chat/completions", state.nim_base);

    let mut req = state
        .client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header(reqwest::header::USER_AGENT, "OmniStream/1.0");

    if let Some(auth) = auth {
        if let Ok(value) = reqwest::header::HeaderValue::from_bytes(&auth) {
            req = req.header(reqwest::header::AUTHORIZATION, value);
        }
    }

    req.json(&payload).send().await
}

async fn send_backend_error(
    tx: &tokio::sync::mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    msg: &str,
) {
    let error_event = json!({
        "ok": false,
        "error": msg,
    });

    let error_event = serde_json::to_string(&error_event)
        .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialization failed\"}".to_string());

    // SSE error event for future frontend handling.
    let _ = tx
        .send(Ok(format!("event: error\ndata: {error_event}\n\n").into_bytes()))
        .await;

    // Also inject a visible message into the normal chat stream so existing
    // frontends that only parse `data:` chunks will show the error.
    let visible = json!({
        "choices": [{
            "delta": {
                "content": format!("\n\n⚠ {msg}")
            }
        }]
    });

    let visible = serde_json::to_string(&visible)
        .unwrap_or_else(|_| "{\"choices\":[{\"delta\":{\"content\":\"Backend error\"}}]}".to_string());

    let _ = tx
        .send(Ok(format!("data: {visible}\n\n").into_bytes()))
        .await;

    let _ = tx.send(Ok(b"data: [DONE]\n\n".to_vec())).await;
}

fn truncate_for_error(s: &str) -> String {
    s.chars().take(400).collect()
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
            if let Some(out) = axum_to_tungstenite(msg) {
                if upstream_sink.send(out).await.is_err() {
                    break;
                }
            }
        }
    };

    let upstream_to_client = async move {
        while let Some(Ok(msg)) = upstream_stream.next().await {
            if let Some(out) = tungstenite_to_axum(msg) {
                if client_sink.send(out).await.is_err() {
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = client_to_upstream => {},
        _ = upstream_to_client => {},
    }
}

#[allow(unreachable_patterns)]
fn axum_to_tungstenite(msg: AxumMessage) -> Option<TungsteniteMessage> {
    match msg {
        AxumMessage::Text(s) => Some(TungsteniteMessage::Text(s.to_string())),
        AxumMessage::Binary(b) => Some(TungsteniteMessage::Binary(b.to_vec())),
        AxumMessage::Ping(p) => Some(TungsteniteMessage::Ping(p.to_vec())),
        AxumMessage::Pong(p) => Some(TungsteniteMessage::Pong(p.to_vec())),
        AxumMessage::Close(_) => Some(TungsteniteMessage::Close(None)),

        // Ignore unknown or raw frame variants.
        _ => None,
    }
}

#[allow(unreachable_patterns)]
fn tungstenite_to_axum(msg: TungsteniteMessage) -> Option<AxumMessage> {
    match msg {
        TungsteniteMessage::Text(s) => Some(AxumMessage::Text(s.to_string().into())),
        TungsteniteMessage::Binary(b) => Some(AxumMessage::Binary(b.to_vec().into())),
        TungsteniteMessage::Ping(p) => Some(AxumMessage::Ping(p.to_vec().into())),
        TungsteniteMessage::Pong(p) => Some(AxumMessage::Pong(p.to_vec().into())),
        TungsteniteMessage::Close(_) => Some(AxumMessage::Close(None)),

        // tungstenite 0.21 includes Frame(_), which Axum does not need directly.
        TungsteniteMessage::Frame(_) => None,

        // Ignore unknown future variants.
        _ => None,
    }
}
