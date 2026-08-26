# DOM Ladder — Multi-Venue BTCUSDT

A single-file BTCUSDT depth-of-market ladder, trade tape, and charting terminal, aggregating resting order size across 18 exchange feeds directly in the browser. No build step, no local server — open the HTML file and it connects directly to each exchange's public WebSocket API from the client, with an optional AI analyst panel powered by a Cloudflare proxy.

## Contents

- [Features](#features)
- [Quick start](#quick-start)
- [How it works](#how-it-works)
- [Venue reference](#venue-reference)
- [Ladder anatomy](#ladder-anatomy)
- [Configuration](#configuration)
- [Performance](#performance)
- [Known limitations](#known-limitations)
- [Browser requirements](#browser-requirements)
- [File map](#file-map)
- [Disclaimer](#disclaimer)

## Features

- **18-venue combined order book.** Binance spot is the authoritative reference (snapshot + diff stream, gap-detected auto-resync); 17 other venues stream directly into the same combined ladder.
- **2-column ladder** — Size, Price — with ask rows tinted red and bid rows tinted green, brighter on round "major" price levels (every 10 ticks).
- **Wall highlighting** — any price level holding more than 75% of the currently visible max size gets a glow.
- **Chase tracker** — a small dot on the best bid/ask price cell that brightens with sustained directional movement and fades on a stall or reversal.
- **Multi-venue trade tape** — every trade across all 18 feeds, color-coded by exchange via a left border.
- **Lightweight Charts v5 panel.** Togglable bottom chart with selectable timeframes (1m–4h), OHLC candles, and a buy/sell volume delta histogram.
- **Volume-scaled absorption bubbles.** Automatically plots `B` (green, market absorbing sells) and `S` (red, market absorbing buys) markers on the chart based on volume z-scores and subsequent price rejection.
- **AI Orderflow Analyst.** Integrated side panel (LLM via NVIDIA NIM) that ingests a comprehensive JSON snapshot of the DOM, tape, CVD, and chart state to provide real-time orderflow analysis, with optional screen-capture cross-referencing.
- **Live feed count and connection status** in the header.
- **Adjustable tick size** — regroups the whole ladder into wider or narrower price buckets on the fly.
- **Fully client-side core.** No backend required for the ladder/tape/chart. The AI feature uses a lightweight Cloudflare Worker proxy to protect the API key.

## Quick start

Open the HTML file in a browser. The ladder and tape connect immediately. 
- To use the **Chart**, click the chart icon and select a timeframe to fetch historical klines.
- To use the **AI Analyst**, click the robot icon, enter your NVIDIA NIM API key (stored locally in `localStorage`), and ask questions about the current orderflow.

## How it works

Binance is the *authoritative* book: it's the only venue with true bid/ask separation (`book.bids` / `book.asks`, kept in sync via Binance's standard snapshot-then-diff procedure — REST `/api/v3/depth` snapshot, buffered `@depth@100ms` diff events, gap detection on `U`/`u` sequence IDs with automatic resync). Binance's best bid/ask is what anchors the ladder's price grid, the mid used for classification, and the chase tracker.

The other 17 venues don't get their own bid/ask columns. Each one writes into a single `Map<price, size>` (no side tag) via `venueBooks[venue]`. At render time, every price from every venue map is classified live — `price <= mid → bid, price > mid → ask` — and summed into the same bucketed arrays Binance's book feeds. This is why removing a resting order from one venue can never clobber another venue's size sitting at the same price: each venue keeps its own map, and only the *bucketed sum* is shared.

```mermaid
flowchart TB
    subgraph BN["Binance — authoritative"]
        BS["REST snapshot\n/api/v3/depth"] --> BOOK["book.bids / book.asks"]
        BD["@depth@100ms diff"] --> BOOK
        BT["@trade + @ticker"] --> ING
        BK["REST /api/v3/klines"] --> CHART
    end
    subgraph VN["17 other venues"]
        direction TB
        V1["Bybit / OKX / Kraken / Coinbase\n/ Bitfinex / Gate / HTX / ...\n(native WS per venue)"] --> VB["venueBooks[venue]\nMap&lt;price, size&gt;, no side tag"]
        V1 --> ING
    end
    BOOK -->|"authoritative bid/ask + grid anchor"| RENDER["renderLadder()"]
    VB -->|"classified live via mid"| RENDER
    RENDER --> LADDER["49-row DOM ladder\n(pooled, reused nodes)"]
    ING["ingestTrade(exchange, ...)"] --> TAPE["trade tape\n(60-row batched rebuild)"]
    ING --> HDR["header price / % change"]
    ING --> CHART["Lightweight Charts v5\n(live candle update)"]
    CHART --> ABS["Absorption Detection\n(vol z-score + delta)"]
    RENDER --> AI["AI Snapshot Context\n(JSON DOM + Tape + CVD)"]
    TAPE --> AI
    ABS --> AI
    AI -->|"Cloudflare Worker Proxy"| LLM["NVIDIA NIM LLM"]
