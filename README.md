# Omni Stream Backend — Rust Proxy

Lightweight Rust backend proxy for the **Omni Stream** open-source crypto terminal frontend.

This backend is designed to run separately from the static frontend and provide server-side proxy routes for services that cannot be reliably accessed directly from the browser due to CORS, WebSocket origin restrictions, or API routing limitations.

The backend is written in Rust using:

- **Axum** — HTTP/WebSocket server
- **Tokio** — async runtime
- **Reqwest** — HTTP client
- **tokio-tungstenite** — upstream WebSocket client
- **tower-http** — CORS handling

---

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Why This Backend Exists](#why-this-backend-exists)
- [Project Structure](#project-structure)
- [Requirements](#requirements)
- [Environment Variables](#environment-variables)
- [Local Development](#local-development)
- [Docker](#docker)
- [Render Deployment](#render-deployment)
- [API Routes](#api-routes)
- [Health Check](#health-check)
- [KuCoin Proxy](#kucoin-proxy)
- [NVIDIA NIM Proxy](#nvidia-nim-proxy)
- [Phemex WebSocket Proxy](#phemex-websocket-proxy)
- [Frontend Integration](#frontend-integration)
- [CORS Configuration](#cors-configuration)
- [Security Notes](#security-notes)
- [Performance Notes](#performance-notes)
- [Troubleshooting](#troubleshooting)
- [Development Notes](#development-notes)
- [Disclaimer](#disclaimer)
- [License](#license)

---

## Overview

This service provides the following proxy routes:

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/` | Basic service metadata |
| `GET` | `/healthz` | Health check endpoint |
| `POST` | `/api/kucoin/bullet-public` | Fetch KuCoin public WebSocket bullet token |
| `POST` | `/api/nim/chat/completions` | Proxy NVIDIA NIM chat completion requests |
| `GET` | `/api/ws/phemex` | WebSocket proxy to Phemex public market data |

The backend is stateless and does not store API keys.

User-supplied NVIDIA NIM keys are passed through the `Authorization` header from the frontend to NVIDIA. The backend does not persist them.

---

## Architecture

┌────────────────────────┐
│ Static Frontend        │
│ HTML/CSS/JS            │
│                        │
│ User enters backend URL│
└───────────┬────────────┘
            │ HTTPS / WSS
            ▼
┌────────────────────────┐
│ Rust Backend Proxy     │
│ Axum + Tokio           │
│                        │
│ /healthz               │
│ /api/kucoin/...        │
│ /api/nim/...           │
│ /api/ws/phemex         │
└───────┬───────┬────────┘
        │       │
        │       └──────────────┐
        │                      │
        ▼                      ▼
┌──────────────┐      ┌────────────────────┐
│ KuCoin API   │      │ NVIDIA NIM API     │
└──────────────┘      └────────────────────┘

        ▲
        │
        ▼
┌────────────────────┐
│ Phemex WebSocket   │
│ wss://vapi...      │
└────────────────────┘

---

## Why This Backend Exists

The static frontend cannot reliably call all required services directly from the browser.

### NVIDIA NIM

Browser calls to NVIDIA NIM endpoints may fail due to CORS or network restrictions.

This backend proxies:

```txt
POST /api/nim/chat/completions
```

to:

```txt
https://integrate.api.nvidia.com/v1/chat/completions
```

### KuCoin

KuCoin's public bullet endpoint may be blocked by browser CORS policies:

```txt
https://api.kucoin.com/api/v1/bullet-public
```

This backend proxies:

```txt
POST /api/kucoin/bullet-public
```

server-side.

### Phemex

Phemex public WebSocket handshakes from browsers may fail with:

```txt
Unexpected response code: 403
```

This backend proxies:

```txt
wss://your-backend/api/ws/phemex
```

to:

```txt
wss://vapi.phemex.com/ws
```

The backend establishes the upstream WebSocket connection server-side and relays messages between the browser and Phemex.

---

## Project Structure

```txt
backend-rust/
├── Cargo.toml
├── Dockerfile
├── .dockerignore
├── .env.example
├── README.md
└── src/
    └── main.rs
```

---

## Requirements

### Local Development

- Rust `1.86` or newer
- Cargo
- OpenSSL is not required because TLS is handled via `rustls`

Check your Rust version:

```bash
rustc --version
```

If needed, update Rust:

```bash
rustup update stable
```

### Docker

- Docker Engine or Docker Desktop

---

## Environment Variables

| Variable | Required | Default | Description |
|---|---:|---|---|
| `PORT` | No | `3000` | HTTP port for the backend |
| `ALLOWED_ORIGINS` | No | empty | Comma-separated CORS allowlist. Empty or `*` allows all origins |
| `NIM_BASE` | No | `https://integrate.api.nvidia.com/v1` | NVIDIA NIM API base URL |
| `KUCOIN_BASE` | No | `https://api.kucoin.com` | KuCoin API base URL |
| `PHEMEX_WS` | No | `wss://vapi.phemex.com/ws` | Phemex public WebSocket URL |
| `PHEMEX_ORIGIN` | No | `https://phemex.com` | Origin header used when connecting to Phemex from the backend |

Example:

```env
PORT=3000

# For public testing:
ALLOWED_ORIGINS=*

# For production:
# ALLOWED_ORIGINS=https://frontend-crypto-terminal.onrender.com,https://yourname.github.io

NIM_BASE=https://integrate.api.nvidia.com/v1
KUCOIN_BASE=https://api.kucoin.com
PHEMEX_WS=wss://vapi.phemex.com/ws
PHEMEX_ORIGIN=https://phemex.com
```

---

## Local Development

Clone the repository and enter the backend directory:

```bash
git clone https://github.com/sergeabchiyt-cyber/Crypto-terminal.git
cd Crypto-terminal/backend-rust
```

Copy the example environment file:

```bash
cp .env.example .env
```

Run the backend:

```bash
cargo run --release
```

The server should start on:

```txt
http://localhost:3000
```

Test the health endpoint:

```bash
curl http://localhost:3000/healthz
```

Expected response:

```json
{
  "ok": true,
  "service": "omni-stream-backend-rust",
  "time": 1768000000,
  "routes": {
    "kucoin": "/api/kucoin/bullet-public",
    "nim": "/api/nim/chat/completions",
    "phemex_ws": "/api/ws/phemex"
  }
}
```

---

## Docker

Build the Docker image:

```bash
docker build -t omni-stream-backend-rust .
```

Run the container:

```bash
docker run --rm -p 3000:3000 \
  -e PORT=3000 \
  -e ALLOWED_ORIGINS=* \
  omni-stream-backend-rust
```

Test:

```bash
curl http://localhost:3000/healthz
```

---

## Render Deployment

Deploy this backend as a **Web Service**, not a static site.

### Render Settings

If the backend is inside a folder such as `backend-rust/`, configure:

```txt
Root Directory: backend-rust
Runtime: Docker
Dockerfile Path: Dockerfile
```

If the backend is at the repository root, configure:

```txt
Root Directory:
Runtime: Docker
Dockerfile Path: Dockerfile
```

### Environment Variables

Minimum:

```txt
PORT=3000
ALLOWED_ORIGINS=*
```

Production example:

```txt
PORT=3000
ALLOWED_ORIGINS=https://frontend-crypto-terminal.onrender.com
```

### Health Check

Configure Render health check path:

```txt
/healthz
```

### First Build Note

The first Rust build can take several minutes because Cargo compiles all dependencies.

If the build fails with an error like:

```txt
rustc 1.85.1 is not supported by the following packages
```

make sure your Dockerfile uses at least:

```dockerfile
FROM rust:1.86-slim AS builder
```

or newer.

---

## API Routes

### Base URL

```txt
https://your-backend-domain.com
```

Example:

```txt
https://omni-stream-backend.onrender.com
```

---

## Health Check

### Request

```http
GET /healthz
```

### Example

```bash
curl https://your-backend-domain.com/healthz
```

### Response

```json
{
  "ok": true,
  "service": "omni-stream-backend-rust",
  "time": 1768000000,
  "routes": {
    "kucoin": "/api/kucoin/bullet-public",
    "nim": "/api/nim/chat/completions",
    "phemex_ws": "/api/ws/phemex"
  }
}
```

The frontend status page uses this endpoint to verify that the backend is reachable.

The frontend only requires:

```json
{
  "ok": true
}
```

---

## KuCoin Proxy

### Request

```http
POST /api/kucoin/bullet-public
```

### Example

```bash
curl -X POST https://your-backend-domain.com/api/kucoin/bullet-public
```

### Upstream Request

The backend sends a server-side request to:

```txt
https://api.kucoin.com/api/v1/bullet-public
```

### Response

The backend returns KuCoin's response unchanged.

Example shape:

```json
{
  "code": "200000",
  "data": {
    "token": "example-token",
    "instanceServers": [
      {
        "endpoint": "wss://ws-api-spot.kucoin.com",
        "encrypt": true,
        "protocol": "websocket",
        "pingInterval": 18000,
        "pingTimeout": 10000
      }
    ]
  }
}
```

The frontend uses this token to open a direct KuCoin WebSocket connection.

---

## NVIDIA NIM Proxy

### Request

```http
POST /api/nim/chat/completions
```

### Headers

| Header | Required | Description |
|---|---:|---|
| `Content-Type` | Yes | Must be `application/json` |
| `Authorization` | Yes | `Bearer nvapi-...` user API key |

### Example

```bash
curl -N -X POST https://your-backend-domain.com/api/nim/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $NVIDIA_API_KEY" \
  -d '{
    "model": "z-ai/glm-5.2",
    "stream": true,
    "temperature": 0.6,
    "top_p": 0.9,
    "max_tokens": 1024,
    "messages": [
      {
        "role": "system",
        "content": "You are a professional crypto market analyst."
      },
      {
        "role": "user",
        "content": "Summarize current BTC market conditions."
      }
    ]
  }'
```

### Upstream Request

The backend forwards the request to:

```txt
https://integrate.api.nvidia.com/v1/chat/completions
```

### Streaming

The backend supports server-sent events, or SSE.

Example streamed chunks:

```txt
data: {"choices":[{"delta":{"content":"BTC"}}]}

data: {"choices":[{"delta":{"content":" is"}}]}

data: [DONE]
```

### Error Handling

If the upstream request fails, the backend may return:

```json
{
  "ok": false,
  "error": "NIM request failed: ..."
}
```

Common upstream statuses:

| Status | Meaning |
|---:|---|
| `401` | Invalid or expired `nvapi-` key |
| `403` | Key does not have access to the requested model |
| `422` | Invalid request payload or model name |
| `429` | Rate limit reached |
| `502` | Backend could not reach upstream |

---

## Phemex WebSocket Proxy

### Endpoint

```txt
wss://your-backend-domain.com/api/ws/phemex
```

Local:

```txt
ws://localhost:3000/api/ws/phemex
```

### Description

The browser connects to the backend WebSocket endpoint.

The backend then connects to:

```txt
wss://vapi.phemex.com/ws
```

Messages are relayed bidirectionally.

The backend does not modify trade payload messages.

It forwards:

- text messages
- binary messages
- ping messages
- pong messages
- close frames

It ignores raw low-level WebSocket `Frame(_)` messages because they are not needed by the frontend.

### Example Using `wscat`

Install `wscat`:

```bash
npm install -g wscat
```

Connect:

```bash
wscat -c wss://your-backend-domain.com/api/ws/phemex
```

Subscribe to BTC trades:

```json
{
  "id": 1,
  "method": "trade.subscribe",
  "params": ["BTCUSDT"]
}
```

Ping:

```json
{
  "id": 99,
  "method": "server.ping",
  "params": []
}
```

Expected Phemex trade message shape:

```json
{
  "type": "snapshot",
  "trades": [
    [
      1768000000000000000,
      "Buy",
      650000000,
      0.001
    ]
  ]
}
```

The frontend parses Phemex trades as:

```txt
[timestamp_ns, side, price_ep×10000, qty]
```

---

## Frontend Integration

The static frontend should provide a startup/status page where the user enters the backend URL.

Example user input:

```txt
https://omni-stream-backend.onrender.com
```

The frontend should check:

```txt
https://omni-stream-backend.onrender.com/healthz
```

Then use:

```txt
POST https://omni-stream-backend.onrender.com/api/kucoin/bullet-public
POST https://omni-stream-backend.onrender.com/api/nim/chat/completions
WS   wss://omni-stream-backend.onrender.com/api/ws/phemex
```

The frontend should store the backend URL in `localStorage` so the user does not need to enter it every time.

Example:

```js
localStorage.setItem("backend_url", "https://omni-stream-backend.onrender.com");
```

---

## CORS Configuration

CORS is controlled by the `ALLOWED_ORIGINS` environment variable.

### Allow All Origins

For testing:

```env
ALLOWED_ORIGINS=*
```

or leave it empty:

```env
ALLOWED_ORIGINS=
```

Both result in permissive CORS.

### Allow Specific Origins

For production:

```env
ALLOWED_ORIGINS=https://frontend-crypto-terminal.onrender.com,https://yourname.github.io
```

The backend will allow:

- `GET`
- `POST`
- `OPTIONS`

and these headers:

```txt
Content-Type
Authorization
```

### WebSocket CORS Note

Browser WebSocket connections are not governed by standard CORS headers in the same way as HTTP requests.

If you need to restrict WebSocket access by origin, add an explicit `Origin` header check inside the WebSocket handler.

---

## Security Notes

### API Keys

This backend does not store NVIDIA NIM API keys.

The frontend stores the user's `nvapi-` key in `sessionStorage` and sends it as:

```txt
Authorization: Bearer nvapi-...
```

The backend forwards that header to NVIDIA.

### HTTPS

Always deploy the backend behind HTTPS in production.

Render provides HTTPS automatically.

### Public Backend Abuse

If you deploy this backend publicly with:

```env
ALLOWED_ORIGINS=*
```

any website can call your backend routes.

For production, restrict CORS:

```env
ALLOWED_ORIGINS=https://your-frontend-domain.com
```

For additional protection, consider adding:

- rate limiting
- IP allowlisting
- shared access token
- reverse proxy protection
- Cloudflare protection

### No Trading Execution

This backend only proxies public market data and AI analysis requests.

It does not place trades.

It does not manage funds.

It does not sign transactions.

---

## Performance Notes

Rust was chosen to reduce runtime overhead compared to Node.js.

The backend is lightweight and suitable for small VPS or free-tier container deployments.

The release profile is configured for a practical balance between build time and runtime performance:

```toml
[profile.release]
opt-level = 2
lto = false
codegen-units = 16
strip = true
panic = "abort"
```

For maximum optimization after builds are stable, you can use:

```toml
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

Note that stronger optimization increases build time and memory usage.

---

## Troubleshooting

### Build fails with `rustc 1.85.1 is not supported`

Example:

```txt
error: rustc 1.85.1 is not supported by the following packages:
  icu_collections@2.2.0 requires rustc 1.86
```

Fix:

Use at least Rust 1.86 in your Dockerfile:

```dockerfile
FROM rust:1.86-slim AS builder
```

or newer.

---

### Build fails with `Message::Frame(_) not covered`

Fix:

Make sure the WebSocket conversion functions handle:

```rust
TungsteniteMessage::Frame(_)
```

or use a wildcard fallback:

```rust
_ => None
```

---

### Build fails with `HeaderValue` type mismatch

Example:

```txt
required for `axum::http::HeaderValue` to implement `TryFrom<reqwest::header::HeaderValue>`
```

Fix:

Use the pinned dependency versions from the provided `Cargo.toml`.

Also ensure header values are converted using bytes:

```rust
HeaderValue::from_bytes(value.as_bytes()).ok()
```

---

### `/healthz` returns nothing

Check:

1. Backend is running.
2. `PORT` matches the exposed port.
3. Docker port mapping is correct.
4. Render health check path is `/healthz`.

Local test:

```bash
curl http://localhost:3000/healthz
```

---

### Frontend says backend is unreachable

Check:

1. Backend URL uses `https://` in production.
2. Backend is awake.
3. `/healthz` returns `ok: true`.
4. `ALLOWED_ORIGINS` includes the frontend origin.
5. Browser console does not show CORS errors.

---

### CORS error in browser console

Example:

```txt
No 'Access-Control-Allow-Origin' header is present on the requested resource.
```

Fix:

Set:

```env
ALLOWED_ORIGINS=https://your-frontend-domain.com
```

or for testing:

```env
ALLOWED_ORIGINS=*
```

Then redeploy.

---

### NVIDIA NIM returns `401`

Meaning:

```txt
Invalid or expired nvapi- key.
```

Fix:

Generate a new key from:

```txt
https://build.nvidia.com
```

Make sure the key starts with:

```txt
nvapi-
```

---

### NVIDIA NIM returns `403`

Meaning:

```txt
Key lacks access to this model.
```

Fix:

Verify that your NVIDIA account has access to:

```txt
z-ai/glm-5.2
```

---

### NVIDIA NIM returns `429`

Meaning:

```txt
Rate limit reached.
```

Fix:

Wait and retry.

---

### KuCoin proxy returns `502`

Possible causes:

- KuCoin upstream unreachable
- network egress blocked
- upstream response invalid
- temporary exchange issue

Test directly from a machine with network access:

```bash
curl -X POST https://api.kucoin.com/api/v1/bullet-public
```

---

### Phemex WebSocket disconnects or returns `403`

Possible causes:

- Phemex is blocking the origin
- Phemex is blocking the IP region
- Phemex endpoint changed
- upstream WebSocket handshake rejected

The backend sets:

```txt
Origin: https://phemex.com
```

by default.

You can override it:

```env
PHEMEX_ORIGIN=https://phemex.com
```

---

### Old frontend still says `/sw.js`

If your deployed frontend still shows:

```txt
Stored in sessionStorage only. Routed through /sw.js proxy.
```

then the old frontend is still deployed.

Redeploy the new static frontend version that includes the backend status page.

After redeploying:

1. Open DevTools.
2. Go to **Application → Service Workers**.
3. Unregister old service workers.
4. Hard reload the page.

---

## Development Notes

### Run with logs

```bash
cargo run --release
```

Logs are emitted using `tracing`.

Example:

```txt
Omni Stream Rust backend listening on 3000
Health: http://0.0.0.0:3000/healthz
```

### Format code

```bash
cargo fmt
```

### Check code

```bash
cargo check
```

### Run tests

```bash
cargo test
```

No tests are currently included.

---

## Disclaimer

This project is an open-source development package.

The codebase is provided as-is, without warranty.

Users must verify code behavior before using it in any environment involving real capital, real exchange accounts, or real trading operations.

This backend does not execute trades and does not handle funds.

It only proxies public market data and AI analysis requests.

---

## License

This project is released under the [MIT Liscence](https://github.com/WisTex/The-MIT-License-Files/blob/main/LICENSE).
```
