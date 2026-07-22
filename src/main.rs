use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{
        ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
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

use redis::AsyncCommands;
use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    nim_base: String,
    kucoin_base: String,
    phemex_ws: String,
    phemex_origin: String,
    redis: redis::Client,
    aes_key: Vec<u8>,
    backend_url: String,
}

type AppError = (StatusCode, Json<Value>);

fn bad_gateway(err: impl std::fmt::Display) -> AppError {
    (StatusCode::BAD_GATEWAY, Json(json!({ "ok": false, "error": err.to_string() })))
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_target(false).with_level(true).init();

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let nim_base = std::env::var("NIM_BASE").unwrap_or_else(|_| "https://integrate.api.nvidia.com/v1".to_string());
    let kucoin_base = std::env::var("KUCOIN_BASE").unwrap_or_else(|_| "https://api.kucoin.com".to_string());
    let phemex_ws = std::env::var("PHEMEX_WS").unwrap_or_else(|_| "wss://vapi.phemex.com/ws".to_string());
    let phemex_origin = std::env::var("PHEMEX_ORIGIN").unwrap_or_else(|_| "https://phemex.com".to_string());
    
    let redis_url = std::env::var("REDIS_URL").expect("REDIS_URL must be set");
    let aes_hex = std::env::var("AES_SECRET_HEX").expect("AES_SECRET_HEX must be set");
    let backend_url = std::env::var("BACKEND_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());

    let client = reqwest::Client::builder().pool_max_idle_per_host(5).build().expect("failed to build reqwest client");
    let redis_client = redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client. Check URL format.");
    let aes_key = hex::decode(&aes_hex).expect("Invalid AES hex");

    let state = AppState {
        client, nim_base, kucoin_base, phemex_ws, phemex_origin,
        redis: redis_client, aes_key, backend_url,
    };

    let cors = build_cors();

    let app = Router::new()
        .route("/", get(root))
        .route("/healthz", get(healthz))
        // Original Proxies
        .route("/api/kucoin/bullet-public", post(kucoin_bullet))
        .route("/api/nim/chat/completions", post(nim_chat))
        .route("/api/ws/phemex", get(phemex_ws_handler))
        // New Grey Hat Features
        .route("/api/config/keys", post(set_api_keys))
        .route("/api/chart/save", post(save_chart_state))
        .route("/api/chart/load", get(load_chart_state))
        .route("/tv-proxy/*path", get(tv_http_proxy))
        .route("/api/ws/tv-proxy", get(tv_ws_proxy))
        .layer(cors)
        .with_state(state);

    let addr = ("0.0.0.0", port);
    let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind port");
    info!("Omni Stream Rust backend listening on {}", port);
    axum::serve(listener, app).await.expect("server failed");
}

fn build_cors() -> CorsLayer {
    let raw = std::env::var("ALLOWED_ORIGINS").unwrap_or_default();
    let origins: Vec<String> = raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if origins.is_empty() || origins.iter().any(|s| s == "*") { return CorsLayer::permissive(); }
    let headers: Vec<HeaderValue> = origins.iter().filter_map(|s| HeaderValue::from_str(s).ok()).collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(headers))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

async fn root() -> Json<Value> {
    Json(json!({ "ok": true, "service": "omni-stream-backend-rust" }))
}

async fn healthz() -> Json<Value> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Json(json!({ "ok": true, "time": now }))
}

// ==========================================
// ORIGINAL PROXIES (KuCoin, NIM, Phemex)
// ==========================================

async fn kucoin_bullet(State(state): State<AppState>) -> Result<Response, AppError> {
    let url = format!("{}/api/v1/bullet-public", state.kucoin_base);
    let upstream = state.client.post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, "OmniStream/1.0")
        .body("{}").send().await.map_err(|e| bad_gateway(format!("KuCoin request failed: {e}")))?;
    let status = upstream.status();
    let content_type = upstream.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok());
    let bytes = upstream.bytes().await.map_err(|e| bad_gateway(format!("KuCoin body read failed: {e}")))?;
    let mut builder = Response::builder().status(status.as_u16());
    builder = match content_type { Some(ct) => builder.header(header::CONTENT_TYPE, ct), None => builder.header(header::CONTENT_TYPE, "application/json") };
    builder = builder.header(header::CACHE_CONTROL, "no-store");
    Ok(builder.body(Body::from(bytes)).map_err(|e| bad_gateway(format!("response build failed: {e}")))?)
}

async fn nim_chat(State(state): State<AppState>, headers: HeaderMap, Json(payload): Json<Value>) -> Result<Response, AppError> {
    info!("NIM proxy: opening early SSE stream");
    let auth = headers.get(header::AUTHORIZATION).map(|v| v.as_bytes().to_vec());
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);
    let _ = tx.send(Ok(b": connected\n\n".to_vec())).await;

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut req_fut = Box::pin(send_nim_request(state, auth, payload));
        let upstream_result = loop {
            tokio::select! {
                _ = interval.tick() => { if tx.send(Ok(b": ping\n\n".to_vec())).await.is_err() { return; } }
                res = req_fut.as_mut() => break res,
            }
        };
        let upstream = match upstream_result {
            Ok(upstream) => upstream,
            Err(e) => { send_backend_error(&tx, &format!("NIM request failed: {e}")).await; return; }
        };
        let status = upstream.status();
        if !status.is_success() {
            let body = upstream.text().await.unwrap_or_default();
            send_backend_error(&tx, &format!("NIM returned HTTP {}: {}", status.as_u16(), truncate_for_error(&body))).await; return;
        }
        let mut stream = upstream.bytes_stream();
        loop {
            tokio::select! {
                _ = interval.tick() => { if tx.send(Ok(b": ping\n\n".to_vec())).await.is_err() { return; } }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => { if tx.send(Ok(bytes.to_vec())).await.is_err() { return; } }
                        Some(Err(e)) => { send_backend_error(&tx, &format!("NIM stream error: {e}")).await; return; }
                        None => break,
                    }
                }
            }
        }
    });

    let body_stream = ReceiverStream::new(rx).map(|item| item.map(bytes::Bytes::from));
    Ok(Response::builder().status(StatusCode::OK).header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform").header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body_stream)).map_err(|e| bad_gateway(format!("response build failed: {e}")))?)
}

async fn send_nim_request(state: AppState, auth: Option<Vec<u8>>, payload: Value) -> Result<reqwest::Response, reqwest::Error> {
    let url = format!("{}/chat/completions", state.nim_base);
    let mut req = state.client.post(&url).header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "text/event-stream").header(reqwest::header::USER_AGENT, "OmniStream/1.0");
    if let Some(auth) = auth { if let Ok(value) = reqwest::header::HeaderValue::from_bytes(&auth) { req = req.header(reqwest::header::AUTHORIZATION, value); } }
    req.json(&payload).send().await
}

async fn send_backend_error(tx: &tokio::sync::mpsc::Sender<Result<Vec<u8>, std::io::Error>>, msg: &str) {
    let error_event = json!({ "ok": false, "error": msg });
    let error_str = serde_json::to_string(&error_event).unwrap_or_default();
    let _ = tx.send(Ok(format!("event: error\ndata: {error_str}\n\n").into_bytes())).await;
    let visible = json!({ "choices": [{ "delta": { "content": format!("\n\n⚠ {msg}") } }] });
    let visible_str = serde_json::to_string(&visible).unwrap_or_default();
    let _ = tx.send(Ok(format!("data: {visible_str}\n\n").into_bytes())).await;
    let _ = tx.send(Ok(b"data: [DONE]\n\n".to_vec())).await;
}

fn truncate_for_error(s: &str) -> String { s.chars().take(400).collect() }

async fn phemex_ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response { ws.on_upgrade(move |socket| handle_phemex(socket, state)) }

async fn handle_phemex(socket: WebSocket, state: AppState) {
    let mut request = match state.phemex_ws.as_str().into_client_request() { Ok(r) => r, Err(e) => { tracing::warn!("Phemex client request build failed: {e}"); return; } };
    let origin = HeaderValue::from_str(&state.phemex_origin).unwrap_or_else(|_| HeaderValue::from_static("https://phemex.com"));
    request.headers_mut().insert(header::ORIGIN, origin);
    request.headers_mut().insert(header::USER_AGENT, HeaderValue::from_static("OmniStream/1.0"));
    let upstream = match connect_async(request).await { Ok((ws, _)) => ws, Err(e) => { tracing::warn!("Phemex upstream WebSocket failed: {e}"); return; } };
    let (mut client_sink, mut client_stream) = socket.split();
    let (mut upstream_sink, mut upstream_stream) = upstream.split();
    let client_to_upstream = async move { while let Some(Ok(msg)) = client_stream.next().await { if let Some(out) = axum_to_tungstenite(msg) { if upstream_sink.send(out).await.is_err() { break; } } } };
    let upstream_to_client = async move { while let Some(Ok(msg)) = upstream_stream.next().await { if let Some(out) = tungstenite_to_axum(msg) { if client_sink.send(out).await.is_err() { break; } } } };
    tokio::select! { _ = client_to_upstream => {}, _ = upstream_to_client => {}, }
}

#[allow(unreachable_patterns)]
fn axum_to_tungstenite(msg: AxumMessage) -> Option<TungsteniteMessage> {
    match msg {
        AxumMessage::Text(s) => Some(TungsteniteMessage::Text(s.to_string())),
        AxumMessage::Binary(b) => Some(TungsteniteMessage::Binary(b.to_vec())),
        AxumMessage::Ping(p) => Some(TungsteniteMessage::Ping(p.to_vec())),
        AxumMessage::Pong(p) => Some(TungsteniteMessage::Pong(p.to_vec())),
        AxumMessage::Close(_) => Some(TungsteniteMessage::Close(None)),
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
        TungsteniteMessage::Frame(_) => None,
        _ => None,
    }
}

// ==========================================
// GREY HAT FEATURES (AES, Redis, TV MITM)
// ==========================================

#[derive(Deserialize)]
struct EncryptedPayload { iv: String, ciphertext: String }

async fn set_api_keys(State(state): State<AppState>, Json(payload): Json<EncryptedPayload>) -> impl IntoResponse {
    let key = Key::<Aes256Gcm>::from_slice(&state.aes_key);
    let cipher = Aes256Gcm::new(key);
    let iv_bytes = match BASE64.decode(&payload.iv) { Ok(b) => b, Err(_) => return (StatusCode::BAD_REQUEST, "Invalid IV base64").into_response() };
    let nonce = Nonce::from_slice(&iv_bytes);
    let ciphertext = match BASE64.decode(&payload.ciphertext) { Ok(b) => b, Err(_) => return (StatusCode::BAD_REQUEST, "Invalid ciphertext base64").into_response() };
    match cipher.decrypt(nonce, ciphertext.as_ref()) {
        Ok(_) => (StatusCode::OK, "API Key securely received and decrypted.").into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "Decryption failed").into_response(),
    }
}

// Flexible payload handler to accept both frontend {symbol, state} and injected script {session_id, state_json}
async fn save_chart_state(State(state): State<AppState>, Json(payload): Json<Value>) -> impl IntoResponse {
    let mut con = match state.redis.get_multiplexed_async_connection().await { 
        Ok(c) => c, 
        Err(e) => { tracing::error!("Redis connection failed: {}", e); return StatusCode::INTERNAL_SERVER_ERROR.into_response(); } 
    };
    
    let session_id = payload.get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("global_tv_state")
        .to_string();
        
    let state_str = if let Some(state_json) = payload.get("state_json").and_then(|v| v.as_str()) {
        state_json.to_string()
    } else {
        payload.get("state").unwrap_or(&payload).to_string()
    };

    let _: Result<(), _> = con.set_ex(&session_id, &state_str, 2592000).await;
    StatusCode::OK.into_response()
}

async fn load_chart_state(State(state): State<AppState>, Query(params): Query<std::collections::HashMap<String, String>>) -> impl IntoResponse {
    let mut con = match state.redis.get_multiplexed_async_connection().await { Ok(c) => c, Err(e) => { tracing::error!("Redis connection failed: {}", e); return StatusCode::INTERNAL_SERVER_ERROR.into_response(); } };
    let session_id = match params.get("session_id") { Some(id) => id, None => return StatusCode::BAD_REQUEST.into_response() };
    let state_json: Option<String> = con.get(session_id).await.unwrap_or(None);
    match state_json { Some(json) => (StatusCode::OK, json).into_response(), None => StatusCode::NOT_FOUND.into_response() }
}

// ==========================================
// TRADINGVIEW MITM HTTP PROXY
// ==========================================

async fn tv_http_proxy(State(state): State<AppState>, Path(path): Path<String>) -> impl IntoResponse {
    let clean_path = path.trim_start_matches('/');
    if clean_path.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let is_widget_domain = clean_path.contains("widgetembed") || clean_path.contains("static/bundles") || clean_path.contains("tv-chart");
    
    let urls_to_try = if is_widget_domain {
        vec![
            format!("https://www.tradingview-widget.com/{}", clean_path),
            format!("https://s3.tradingview.com/{}", clean_path),
        ]
    } else {
        vec![
            format!("https://s3.tradingview.com/{}", clean_path),
            format!("https://www.tradingview-widget.com/{}", clean_path),
        ]
    };

    let mut final_resp = None;

    for url in urls_to_try {
        match state.client.get(&url)
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
            .header(reqwest::header::REFERER, "https://www.tradingview.com/")
            .header(reqwest::header::ORIGIN, "https://www.tradingview.com")
            .header(reqwest::header::ACCEPT, "*/*")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                final_resp = Some(resp);
                break;
            }
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                continue;
            }
            Ok(resp) => {
                return process_tv_response(resp, &state, clean_path).await;
            }
            Err(_) => continue,
        }
    }

    let resp = match final_resp {
        Some(r) => r,
        None => {
            tracing::warn!("TV Proxy failed to fetch from all fallback domains for: {}", clean_path);
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };

    process_tv_response(resp, &state, clean_path).await
}

async fn process_tv_response(resp: reqwest::Response, state: &AppState, clean_path: &str) -> Response {
    let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let is_text = content_type.contains("javascript") || content_type.contains("html") || content_type.contains("json") || 
                  clean_path.ends_with(".js") || clean_path.ends_with(".html") || clean_path.ends_with(".css") || clean_path.ends_with(".map");
    
    let mut body = resp.text().await.unwrap_or_default();

    if is_text && !body.is_empty() {
        let backend_url = state.backend_url.trim_end_matches('/');
        let ws_backend = backend_url.replace("http://", "ws://").replace("https://", "wss://");
        let backend_host = backend_url.replace("http://", "").replace("https://", "");

        let proxy_prefix = format!("{}/tv-proxy/", backend_url);
        let proxy_prefix_escaped = proxy_prefix.replace("/", "\\/");
        let host_proxy_prefix = format!("//{}/tv-proxy/", backend_host);

        // 1. Hijack WebSocket URLs
        body = body.replace("wss://prodata.tradingview.com/socket.io/websocket", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://data.tradingview.com/socket.io/websocket", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://prodata.tradingview.com", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://data.tradingview.com", &format!("{}/api/ws/tv-proxy", ws_backend));
        
        // 2. Hijack absolute static asset URLs
        body = body.replace("https://www.tradingview-widget.com/", &proxy_prefix);
        body = body.replace("https://s3.tradingview.com/", &proxy_prefix);
        body = body.replace("https://s.tradingview.com/", &proxy_prefix);
        
        // 3. Hijack escaped absolute URLs
        body = body.replace("https:\\/\\/www.tradingview-widget.com\\/", &proxy_prefix_escaped);
        body = body.replace("https:\\/\\/s3.tradingview.com\\/", &proxy_prefix_escaped);
        body = body.replace("https:\\/\\/s.tradingview.com\\/", &proxy_prefix_escaped);

        // 4. Hijack protocol-relative URLs
        body = body.replace("//www.tradingview-widget.com/", &host_proxy_prefix);
        body = body.replace("//s3.tradingview.com/", &host_proxy_prefix);
        body = body.replace("//s.tradingview.com/", &host_proxy_prefix);

        // 5. Inject Redis Sync Script
        let redis_sync_script = format!(r#"
        <script>
            (function() {{
                setInterval(() => {{
                    try {{
                        const statePayload = {{}};
                        let hasData = false;
                        for (let i = 0; i < localStorage.length; i++) {{
                            const key = localStorage.key(i);
                            if (key && (key.startsWith('ss-') || key.startsWith('tradingview_') || key.includes('tv-'))) {{
                                statePayload[key] = localStorage.getItem(key);
                                hasData = true;
                            }}
                        }}
                        if (hasData) {{
                            fetch('{}/api/chart/save', {{
                                method: 'POST',
                                headers: {{'Content-Type': 'application/json'}},
                                body: JSON.stringify({{ session_id: 'global_tv_state', state_json: JSON.stringify(statePayload) }})
                            }}).catch(() => {{}});
                        }}
                    }} catch(e) {{}}
                }}, 5000);
            }})();
        </script>
        "#, backend_url);
        
        if body.contains("</body>") {
            body = body.replace("</body>", &format!("{} </body>", redis_sync_script));
        } else if body.contains("</head>") {
            body = body.replace("</head>", &format!("{} </head>", redis_sync_script));
        } else {
            body = format!("{}{}", body, redis_sync_script);
        }
    }

    let mut headers = HeaderMap::new();
    for (key, value) in resp.headers() {
        if key == reqwest::header::CONTENT_TYPE || key == reqwest::header::CACHE_CONTROL || key == reqwest::header::ETAG {
            if let Ok(val) = value.to_str() {
                if let Ok(header_name) = header::HeaderName::from_bytes(key.as_ref()) {
                    if let Ok(header_val) = header::HeaderValue::from_str(val) {
                        headers.insert(header_name, header_val);
                    }
                }
            }
        }
    }
    
    if is_text {
        headers.insert(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".parse().unwrap());
    }

    // FIX: Convert reqwest StatusCode to axum StatusCode for IntoResponse compatibility
    let status = axum::http::StatusCode::from_u16(resp.status().as_u16()).unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    (status, headers, body).into_response()
}

// ==========================================
// TRADINGVIEW WEBSOCKET PROXY
// ==========================================

async fn tv_ws_proxy(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_tv_socket(socket, state))
}

async fn fetch_10k_candles(client: &reqwest::Client, symbol: &str, interval: &str) -> Vec<Value> {
    let mut all_candles = Vec::new();
    let mut end_time = Utc::now().timestamp_millis();
    for _ in 0..10 {
        let url = format!("https://testnet.binance.vision/api/v3/klines?symbol={}&interval={}&endTime={}&limit=1000", symbol, interval, end_time);
        if let Ok(resp) = client.get(&url).send().await {
            if let Ok(data) = resp.json::<Vec<Value>>().await {
                if data.is_empty() { break; }
                if let Some(oldest) = data.first() { if let Some(t) = oldest[0].as_i64() { end_time = t - 1; } }
                all_candles.extend(data);
            }
        }
    }
    all_candles.reverse();
    all_candles
}

async fn handle_tv_socket(mut tv_socket: WebSocket, state: AppState) {
    info!("[PROXY] TradingView Iframe connected.");
    let binance_url = "wss://testnet.binance.vision/ws/btcusdt@kline_1m";
    
    let mut binance_ws = match connect_async(binance_url).await { 
        Ok((ws, _)) => ws, 
        Err(e) => { tracing::error!("Binance WS failed: {}", e); return; } 
    };

    loop {
        tokio::select! {
            Some(Ok(msg)) = tv_socket.recv() => {
                if let AxumMessage::Text(text) = msg {
                    if text.contains("~h~") {
                        let _ = tv_socket.send(AxumMessage::Text(text)).await;
                    } else if text.contains("create_series") {
                        info!("[PROXY] Intercepted create_series. Fetching 10k candles...");
                        let candles = fetch_10k_candles(&state.client, "BTCUSDT", "1m").await;
                        let tv_data: Vec<Value> = candles.iter().filter_map(|c| {
                            let t = c[0].as_f64()? / 1000.0;
                            let o = c[1].as_str()?.parse::<f64>().ok()?;
                            let h = c[2].as_str()?.parse::<f64>().ok()?;
                            let l = c[3].as_str()?.parse::<f64>().ok()?;
                            let cl = c[4].as_str()?.parse::<f64>().ok()?;
                            let v = c[5].as_str()?.parse::<f64>().ok()?;
                            Some(json!({ "i": c[0].as_i64().unwrap_or(0), "v": [t, o, h, l, cl, v] }))
                        }).collect();

                        let payload = json!({ "m": "timescale_update", "p": ["cs_local", {"sds_1": {"s": tv_data}}] });
                        let payload_str = payload.to_string();
                        let wrapped = format!("~m~{}~m~{}", payload_str.len(), payload_str);
                        let _ = tv_socket.send(AxumMessage::Text(wrapped.into())).await;
                    }
                }
            }
            Some(Ok(msg)) = binance_ws.next() => {
                if let TungsteniteMessage::Text(text) = msg {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                        if parsed["e"] == "kline" {
                            let k = &parsed["k"];
                            let t = k["t"].as_f64().unwrap_or(0.0) / 1000.0;
                            let close_time = t + 60.0;
                            let du_payload = json!({
                                "m": "du",
                                "p": ["cs_local", {"sds_1": {"s": [{
                                    "i": 0,
                                    "v": [t, k["o"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["h"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["l"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["c"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["v"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0)]
                                }], "ns": {"d": "", "indexes": "nochange"}, "t": "sds_1", "lbs": {"bar_close_time": close_time}}}]
                            });
                            let payload_str = du_payload.to_string();
                            let wrapped = format!("~m~{}~m~{}", payload_str.len(), payload_str);
                            if tv_socket.send(AxumMessage::Text(wrapped.into())).await.is_err() { break; }
                        }
                    }
                }
            }
            else => break,
        }
    }
}
