use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{
        ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
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
    tungstenite::Message as TungsteniteMessage,
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{info, warn, error};

use redis::AsyncCommands;
use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    nim_base: String,
    kucoin_base: String,
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
    eprintln!("[BOOT 1/8] Starting application...");
    dotenvy::dotenv().ok();
    
    eprintln!("[BOOT 2/8] Initializing tracing...");
    tracing_subscriber::fmt().with_target(false).with_level(true).init();

    eprintln!("[BOOT 3/8] Loading environment variables...");
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(10000);
    let nim_base = std::env::var("NIM_BASE").unwrap_or_else(|_| "https://integrate.api.nvidia.com/v1".to_string());
    let kucoin_base = std::env::var("KUCOIN_BASE").unwrap_or_else(|_| "https://api.kucoin.com".to_string());
    
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| {
        eprintln!("[FATAL] REDIS_URL is missing!");
        std::process::exit(1);
    });
    
    let aes_hex = std::env::var("AES_SECRET_HEX").unwrap_or_else(|_| {
        eprintln!("[FATAL] AES_SECRET_HEX is missing!");
        std::process::exit(1);
    });
    
    let backend_url = std::env::var("BACKEND_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());

    eprintln!("[BOOT 4/8] Building HTTP client...");
    let client = reqwest::Client::builder().pool_max_idle_per_host(5).build().expect("failed to build reqwest client");
    
    eprintln!("[BOOT 5/8] Validating Redis URL...");
    let redis_client = redis::Client::open(redis_url.as_str()).unwrap_or_else(|e| {
        eprintln!("[FATAL] Redis URL invalid: {}", e);
        std::process::exit(1);
    });
    
    eprintln!("[BOOT 6/8] Decoding AES key...");
    let aes_key = hex::decode(&aes_hex).unwrap_or_else(|_| {
        eprintln!("[FATAL] AES_SECRET_HEX is not valid hex!");
        std::process::exit(1);
    });

    eprintln!("[BOOT 7/8] Building Router...");
    let state = AppState {
        client, nim_base, kucoin_base,
        redis: redis_client, aes_key, backend_url,
    };

    let cors = build_cors();

    let app = Router::new()
        .route("/", get(root))
        .route("/healthz", get(healthz))
        .route("/api/kucoin/bullet-public", post(kucoin_bullet))
        .route("/api/nim/chat/completions", post(nim_chat))
        .route("/api/config/keys", post(set_api_keys))
        .route("/api/chart/save", post(save_chart_state))
        .route("/api/chart/load", get(load_chart_state))
        .route("/tv-proxy/*path", get(tv_http_proxy))
        .route("/api/ws/tv-proxy", get(tv_ws_proxy))
        .layer(cors)
        .with_state(state);

    let addr = ("0.0.0.0", port);
    eprintln!("[BOOT 8/8] Binding to port {}...", port);
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

async fn root() -> Json<Value> { Json(json!({ "ok": true, "service": "omni-stream-backend-rust" })) }
async fn healthz() -> Json<Value> { Json(json!({ "ok": true, "time": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() })) }

// ==========================================
// ORIGINAL PROXIES (KuCoin, NIM)
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

async fn save_chart_state(State(state): State<AppState>, Json(payload): Json<Value>) -> impl IntoResponse {
    let mut con = match state.redis.get_multiplexed_async_connection().await { 
        Ok(c) => c, 
        Err(e) => { error!("Redis connection failed: {}", e); return StatusCode::INTERNAL_SERVER_ERROR.into_response(); } 
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
    let mut con = match state.redis.get_multiplexed_async_connection().await { Ok(c) => c, Err(e) => { error!("Redis connection failed: {}", e); return StatusCode::INTERNAL_SERVER_ERROR.into_response(); } };
    let session_id = match params.get("session_id") { Some(id) => id, None => return StatusCode::BAD_REQUEST.into_response() };
    let state_json: Option<String> = con.get(session_id).await.unwrap_or(None);
    match state_json { Some(json) => (StatusCode::OK, json).into_response(), None => StatusCode::NOT_FOUND.into_response() }
}

// ==========================================
// TRADINGVIEW MITM HTTP PROXY
// ==========================================

async fn tv_http_proxy(
    State(state): State<AppState>, 
    Path(path): Path<String>,
    uri: Uri, 
) -> impl IntoResponse {
    let original_path = path.clone();
    let mut clean_path = path.trim_start_matches('/').to_string();
    
    info!("[TV-PROXY] ➡️ Incoming request: path='{}', full_uri='{}'", original_path, uri);

    if let Some(query) = uri.query() {
        clean_path = format!("{}?{}", clean_path, query);
        info!("[TV-PROXY] 🔗 Reattached query string. Final target path: '{}'", clean_path);
    }

    if clean_path.is_empty() {
        warn!("[TV-PROXY] ❌ Empty path after trimming. Returning 400 Bad Request.");
        return StatusCode::BAD_REQUEST.into_response();
    }

    let urls_to_try = vec![
        format!("https://www.tradingview-widget.com/{}", clean_path),
        format!("https://s.tradingview.com/{}", clean_path),
        format!("https://s3.tradingview.com/{}", clean_path),
    ];

    let mut final_resp = None;

    for url in &urls_to_try {
        info!("[TV-PROXY] 🌐 Attempting fetch from upstream: {}", url);
        match state.client.get(url)
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7")
            .header("Accept-Language", "en-US,en;q=0.9")
            // NOTE: Accept-Encoding is intentionally omitted to prevent Brotli binary garbage
            .header("Referer", "https://www.tradingview-widget.com/")
            .header("Sec-Fetch-Dest", "iframe")
            .header("Sec-Fetch-Mode", "navigate")
            .header("Sec-Fetch-Site", "cross-site")
            .header("Sec-Ch-Ua", "\"Chromium\";v=\"126\", \"Google Chrome\";v=\"126\", \"Not-A.Brand\";v=\"8\"")
            .header("Sec-Ch-Ua-Mobile", "?0")
            .header("Sec-Ch-Ua-Platform", "\"Windows\"")
            .send()
            .await
        {
            Ok(resp) => {
                info!("[TV-PROXY] 📥 Response from {}: Status {}", url, resp.status());
                if resp.status().is_success() {
                    final_resp = Some(resp);
                    break;
                } else if resp.status() == reqwest::StatusCode::NOT_FOUND || resp.status() == reqwest::StatusCode::FORBIDDEN {
                    info!("[TV-PROXY] ⚠️ {} for {}, trying next fallback domain...", resp.status(), url);
                    continue;
                } else {
                    warn!("[TV-PROXY] 🛑 Non-success/non-404 status {} for {}. Returning immediately.", resp.status(), url);
                    return process_tv_response(resp, &state, &clean_path).await;
                }
            }
            Err(e) => {
                warn!("[TV-PROXY] ❌ Network error fetching {}: {}", url, e);
                continue;
            }
        }
    }

    let resp = match final_resp {
        Some(r) => r,
        None => {
            error!("[TV-PROXY] 💀 Failed to fetch from ALL fallback domains for: {}", clean_path);
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };

    process_tv_response(resp, &state, &clean_path).await
}

async fn process_tv_response(resp: reqwest::Response, state: &AppState, clean_path: &str) -> Response {
    let status = resp.status();
    let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let is_text = content_type.contains("javascript") || content_type.contains("html") || content_type.contains("json") || 
                  clean_path.ends_with(".js") || clean_path.ends_with(".html") || clean_path.ends_with(".css") || clean_path.ends_with(".map");
    
    let is_html = content_type.contains("html") || clean_path.ends_with(".html") || clean_path.ends_with("/");

    info!("[TV-PROXY] ⚙️ Processing: '{}' | Status: {} | IsText: {} | IsHTML: {}", clean_path, status, is_text, is_html);

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

    let mut body = resp.text().await.unwrap_or_default();

    if is_text && !body.is_empty() {
        let backend_url = state.backend_url.trim_end_matches('/');
        let ws_backend = backend_url.replace("http://", "ws://").replace("https://", "wss://");

        let proxy_prefix = format!("{}/tv-proxy/", backend_url);
        let proxy_prefix_no_slash = proxy_prefix.trim_end_matches('/'); 
        
        let proxy_prefix_escaped = proxy_prefix.replace("/", "\\/");
        let proxy_prefix_escaped_no_slash = proxy_prefix_no_slash.replace("/", "\\/");

        // 1. Hijack Exact WebSocket Paths FIRST (to prevent Axum 404s from trailing paths)
        body = body.replace("wss://prodata.tradingview.com/socket.io/websocket", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://data.tradingview.com/socket.io/websocket", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://pushstream.tradingview.com/message-pipe-ws/public", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://widgetdata.tradingview.com/socket.io/websocket", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://widgetdata-backup.tradingview.com/socket.io/websocket", &format!("{}/api/ws/tv-proxy", ws_backend));
        
        // 2. Hijack Base WebSocket Domains (Fallback catch-all)
        body = body.replace("wss://prodata.tradingview.com", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://data.tradingview.com", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://pushstream.tradingview.com", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://widgetdata.tradingview.com", &format!("{}/api/ws/tv-proxy", ws_backend));
        body = body.replace("wss://widgetdata-backup.tradingview.com", &format!("{}/api/ws/tv-proxy", ws_backend));
        
        // 3. Hijack absolute static asset URLs
        body = body.replace("https://www.tradingview-widget.com", proxy_prefix_no_slash);
        body = body.replace("https://s3.tradingview.com", proxy_prefix_no_slash);
        body = body.replace("https://s.tradingview.com", proxy_prefix_no_slash);
        
        body = body.replace("https://www.tradingview-widget.com/", &proxy_prefix);
        body = body.replace("https://s3.tradingview.com/", &proxy_prefix);
        body = body.replace("https://s.tradingview.com/", &proxy_prefix);
        
        // 4. Hijack escaped absolute URLs
        body = body.replace("https:\\/\\/www.tradingview-widget.com", &proxy_prefix_escaped_no_slash);
        body = body.replace("https:\\/\\/s3.tradingview.com", &proxy_prefix_escaped_no_slash);
        body = body.replace("https:\\/\\/s.tradingview.com", &proxy_prefix_escaped_no_slash);

        body = body.replace("https:\\/\\/www.tradingview-widget.com\\/", &proxy_prefix_escaped);
        body = body.replace("https:\\/\\/s3.tradingview.com\\/", &proxy_prefix_escaped);
        body = body.replace("https:\\/\\/s.tradingview.com\\/", &proxy_prefix_escaped);

        // 5. Hijack protocol-relative URLs
        let backend_host = backend_url.replace("http://", "").replace("https://", "");
        let host_proxy_prefix = format!("//{}/tv-proxy/", backend_host);
        body = body.replace("//www.tradingview-widget.com/", &host_proxy_prefix);
        body = body.replace("//s3.tradingview.com/", &host_proxy_prefix);
        body = body.replace("//s.tradingview.com/", &host_proxy_prefix);

        // 6. Inject Redis Sync Script (STRICTLY HTML ONLY)
        if is_html {
            info!("[TV-PROXY] 💉 Injecting Redis script into HTML document: '{}'", clean_path);
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
        } else {
            info!("[TV-PROXY] ⏭️ Skipping HTML injection for non-HTML file: '{}'", clean_path);
        }

        headers.insert(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".parse().unwrap());
    }

    let axum_status = axum::http::StatusCode::from_u16(status.as_u16()).unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    (axum_status, headers, body).into_response()
}

// ==========================================
// TRADINGVIEW WEBSOCKET PROXY (BINANCE ONLY)
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

async fn handle_tv_socket(tv_socket: WebSocket, state: AppState) {
    info!("[WS-PROXY] 🟢 TradingView client connected.");
    
    let (mut tv_sink, mut tv_stream) = tv_socket.split();

    // Channel for all outgoing messages to the TV client
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<AxumMessage>(32);

    // Writer task: pulls from out_rx and sends to tv_sink
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if tv_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let out_tx_clone = out_tx.clone();

    // 1. IMMEDIATE PROTOCOL HANDSHAKE
    let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let sid = format!("sid_{}", timestamp_ms);
    let socket_io_open = format!("0{{\"sid\":\"{}\",\"upgrades\":[],\"pingInterval\":25000,\"pingTimeout\":5000}}", sid);
    
    let session_id = format!("cs_{}", timestamp_ms);
    let session_json = format!("{{\"session_id\":\"{}\",\"timestamp\":{},\"name\":\"crypto-terminal\",\"premium\":false}}", session_id, timestamp_ms);
    let tv_session = format!("~m~{}~m~{}", session_json.len(), session_json);

    let _ = out_tx.send(AxumMessage::Text(socket_io_open.into())).await;
    let _ = out_tx.send(AxumMessage::Text(tv_session.into())).await;
    info!("[WS-PROXY] ✅ Protocol handshake sent.");

    // Channel to pass chart_id from reader to main task
    let (tx_chart_id, mut rx_chart_id) = tokio::sync::mpsc::channel::<String>(8);
    
    // 2. READER TASK
    tokio::spawn(async move {
        let mut current_chart_id = String::from("cs_default");
        while let Some(Ok(msg)) = tv_stream.next().await {
            match msg {
                    AxumMessage::Text(text) => {
                    let t_str = text.as_str();
                    info!("[WS-PROXY] ⬅️ TV says: {}", t_str);
                    
                    if t_str.contains("~h~") {
                        let _ = out_tx_clone.send(AxumMessage::Text(text)).await;
                    } 
                    else if t_str.contains("chart_create_session") {
                        if let Some(start) = t_str.find("\"p\":[\"") {
                            let rest = &t_str[start + 7..];
                            if let Some(end) = rest.find('"') {
                                current_chart_id = rest[..end].to_string();
                                info!("[WS-PROXY] 🆔 Extracted chart ID: {}", current_chart_id);
                                let _ = tx_chart_id.send(current_chart_id.clone()).await;
                            }
                        }
                        let _ = out_tx_clone.send(AxumMessage::Text(text)).await;
                    }
                    else if t_str.contains("resolve_symbol") || t_str.contains("create_series") || t_str.contains("modify_series") {
                        let _ = out_tx_clone.send(AxumMessage::Text(text)).await;
                    }
                }
                AxumMessage::Close(_) => {
                    info!("[WS-PROXY] 🚪 TV client sent Close frame.");
                    break;
                }
                AxumMessage::Ping(p) => {
                    let _ = out_tx_clone.send(AxumMessage::Pong(p)).await;
                }
                _ => {}
            }
        }
    });

    // 3. WAIT FOR CHART ID
    let chart_id = match tokio::time::timeout(Duration::from_secs(5), rx_chart_id.recv()).await {
        Ok(Some(id)) => id,
        _ => {
            warn!("[WS-PROXY] ⚠️ Timeout or error waiting for chart_create_session. Using fallback.");
            String::from("cs_local")
        }
    };
    info!("[WS-PROXY] 🎯 Using chart ID for data stream: {}", chart_id);

    // 4. BINANCE PIPELINE
    info!("[WS-PROXY] 🚀 Fetching 10k candles...");
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

    let payload = json!({ "m": "timescale_update", "p": [&chart_id, {"sds_1": {"s": tv_data}}] });
    let payload_str = payload.to_string();
    let wrapped = format!("~m~{}~m~{}", payload_str.len(), payload_str);
    info!("[WS-PROXY] 📦 Sending {} candles to TV.", tv_data.len());
    
    if out_tx.send(AxumMessage::Text(wrapped.into())).await.is_err() {
        error!("[WS-PROXY] ❌ Failed to send candles, client disconnected.");
        return;
    }

    // 5. BINANCE LIVE STREAM
    let binance_url = "wss://testnet.binance.vision/ws/btcusdt@kline_1m";
    info!("[WS-PROXY] 🔌 Connecting to Binance Testnet...");
    let mut binance_ws = match connect_async(binance_url).await { 
        Ok((ws, _)) => {
            info!("[WS-PROXY] ✅ Binance WS connected successfully.");
            ws
        }, 
        Err(e) => { 
            error!("[WS-PROXY] ❌ Binance WS connection failed: {}", e); 
            return; 
        } 
    };

    loop {
        tokio::select! {
            msg = binance_ws.next() => {
                match msg {
                    Some(Ok(TungsteniteMessage::Text(text))) => {
                        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                            if parsed["e"] == "kline" {
                                let k = &parsed["k"];
                                let t = k["t"].as_f64().unwrap_or(0.0) / 1000.0;
                                let close_time = t + 60.0;
                                
                                let du_payload = json!({
                                    "m": "du",
                                    "p": [&chart_id, {"sds_1": {"s": [{
                                        "i": 0,
                                        "v": [t, k["o"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["h"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["l"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["c"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0), k["v"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0)]
                                    }], "ns": {"d": "", "indexes": "nochange"}, "t": "sds_1", "lbs": {"bar_close_time": close_time}}}]
                                });
                                let payload_str = du_payload.to_string();
                                let wrapped = format!("~m~{}~m~{}", payload_str.len(), payload_str);
                                if out_tx.send(AxumMessage::Text(wrapped.into())).await.is_err() { 
                                    info!("[WS-PROXY] 🚪 Failed to send to TV, breaking loop.");
                                    break; 
                                }
                            }
                        }
                    }
                    Some(Ok(TungsteniteMessage::Ping(p))) => {
                        let _ = binance_ws.send(TungsteniteMessage::Pong(p)).await;
                    }
                    Some(Err(e)) => {
                        error!("[WS-PROXY] ❌ Binance WS error: {}", e);
                        break;
                    }
                    None => {
                        info!("[WS-PROXY] 🚪 Binance WS stream ended.");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    info!("[WS-PROXY] 🔴 Loop exited. Connection closed.");
}
