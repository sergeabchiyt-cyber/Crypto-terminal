# DOM Ladder — Multi-Venue BTCUSDT

A single-file, backend-free BTCUSDT depth-of-market ladder and trade tape, aggregating resting order size across 18 exchange feeds in the browser. No build step, no server, no dependencies — open the HTML file and it connects directly to each exchange's public WebSocket API from the client.

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
- **Live feed count and connection status** in the header.
- **Adjustable tick size** — regroups the whole ladder into wider or narrower price buckets on the fly.
- **Fully client-side.** No backend, no API keys, no localStorage (browser storage isn't available inside artifact sandboxes, so all state is in-memory and resets on reload).

## Quick start

Open the HTML file in a browser. That's it — no server, no install, no config. It connects on load and starts streaming immediately.

## How it works

Binance is the *authoritative* book: it's the only venue with true bid/ask separation (`book.bids` / `book.asks`, kept in sync via Binance's standard snapshot-then-diff procedure — REST `/api/v3/depth` snapshot, buffered `@depth@100ms` diff events, gap detection on `U`/`u` sequence IDs with automatic resync). Binance's best bid/ask is what anchors the ladder's price grid, the mid used for classification, and the chase tracker.

The other 17 venues don't get their own bid/ask columns. Each one writes into a single `Map<price, size>` (no side tag) via `venueBooks[venue]`. At render time, every price from every venue map is classified live — `price <= mid → bid, price > mid → ask` — and summed into the same bucketed arrays Binance's book feeds. This is why removing a resting order from one venue can never clobber another venue's size sitting at the same price: each venue keeps its own map, and only the *bucketed sum* is shared.

```mermaid
flowchart TB
    subgraph BN["Binance — authoritative"]
        BS["REST snapshot\n/api/v3/depth"] --> BOOK["book.bids / book.asks"]
        BD["@depth@100ms diff"] --> BOOK
        BT["@trade + @ticker"] --> ING
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
```

Rendering is decoupled from message frequency: incoming WebSocket messages just set a `dirty` flag and touch data structures — nothing paints synchronously. A single `requestAnimationFrame` loop checks the flag and repaints at most every 120ms (~8/sec), regardless of whether 1 or 100 updates arrived in that window. The trade tape works the same way — trades queue into an array, and the visible list (capped at 60 rows) is rebuilt with one bounded `innerHTML` write per tick instead of a DOM mutation per trade.

Each non-Binance venue connects through a shared `makeVenueWS()` wrapper handling reconnect with exponential backoff (starts at 1.5s, ×1.6 per attempt, capped at 30s), a keepalive ping where the venue's protocol requires one, and gzip-compressed frame decoding (via the browser's native `DecompressionStream`) for the three venues — Bitfinex, HTX, BingX — that push binary-compressed WebSocket frames.

## Venue reference

| Venue | Market | Symbol | Depth feed | Notes |
|---|---|---|---|---|
| Binance | Spot | `btcusdt` | Snapshot + diff (authoritative) | Reference book; also `@trade`+`@ticker` for tape/header |
| Binance Futures | USDⓈ-M Perp | `btcusdt` | Snapshot push, top 20 @ 500ms | `aggTrade` + `depth20` combined stream |
| Bybit | Spot | `BTCUSDT` | Snapshot + delta | `orderbook.200`, 20s ping |
| Bybit Linear | USDT Perp | `BTCUSDT` | Snapshot + delta | `orderbook.500`, 20s ping |
| OKX | Spot | `BTC-USDT` | Snapshot + delta | `books` channel, 25s text ping |
| OKX Futures | Perp Swap | `BTC-USDT-SWAP` | Snapshot + delta | `books` channel, 25s text ping |
| Gate.io | Spot | `BTC_USDT` | Snapshot push | `spot.order_book`, 20s JSON ping |
| Bitget | Spot | `BTCUSDT` | Snapshot push | `books15`, 25s text ping |
| Bitstamp | Spot | `btcusdt` | Snapshot push | `order_book_*`, no ping needed |
| Gemini | Spot | `BTCUSD` | Delta only | `l2_updates`, no ping needed |
| Kraken | Spot | `BTC/USDT` | Snapshot + delta | v2 `book` channel, 30s JSON ping |
| Coinbase | Spot | `BTC-USDT` | Snapshot + delta (client-reconstructed) | `level2`, no ping needed |
| Bitfinex | Spot | `tBTCUSD` | Snapshot + delta | raw book P0/F0, gzip-capable, no ping |
| HTX (Huobi) | Spot | `btcusdt` | Snapshot push | `depth.step0`, always gzip, protocol ping/pong |
| Poloniex | Spot | `BTC_USDT` | Snapshot + delta | `book_lv2`, 30s JSON ping |
| BingX | Spot | `BTC-USDT` | Snapshot push | `@depth`, gzip-capable, text `Ping`/`Pong` |
| WOO X | Spot | `SPOT_BTC_USDT` | Snapshot push | `@orderbook`, 25s JSON ping |
| Deribit | Perp | `BTC_USDT` | Snapshot + delta | `book.100ms`, 30s JSON-RPC ping |

**Not included:** KuCoin and Phemex require a signed/authenticated connection or a CORS proxy that only exists behind a backend — not reachable from a standalone client-side file. MEXC is disabled at the source (the legacy terminal this was ported from disables it unconditionally too).

## Ladder anatomy

- **Size / Price** — the only two columns. Size is resting order size at that price, aggregated across every connected venue; which side it's on is conveyed by row color, not column position.
- **Red rows** = ask side (above spread), **green rows** = bid side (below spread), both brighter on major price levels (multiples of 10× the current tick).
- **Center spread row** — divider showing best-ask minus best-bid.
- **Glowing size text** — a wall: that price level holds over 75% of the currently visible maximum size.
- **Purple outline on price** — the row nearest the last executed trade price.
- **Small dot on best bid/ask** — the chase tracker; opacity ramps up with consecutive same-direction best-price moves and decays on a stall or reversal.

## Configuration

The **TICK** field in the ladder footer sets the price bucket width (defaults to 0.5). Changing it regroups every row on the next render — wider buckets aggregate more price levels together, narrower buckets show finer granularity but a shorter visible price range for the same 49 rows.

## Performance

The render loop and tape both went through a couple of rounds of tightening:

- Ladder repaint throttled to ~8/sec instead of running on every dirty animation frame — the full order book (Binance + 17 venues, thousands of price levels) was being re-aggregated up to 60×/sec before this.
- Cross-venue best-bid/ask extension pass removed — venues now only feed the bucket sums, not the reference best price, cutting venue iteration from two passes to one per render.
- Trade tape rebuilds as one bounded `innerHTML` write (max 60 rows) per tick instead of a DOM insert+evict pair per trade — decouples DOM cost from trade volume instead of scaling with it.
- Dead trade-bucket aggregation system (an earlier per-price buy/sell volume column that was later removed from the UI) fully deleted rather than left running unused.
- Unnecessary `toFixed`/string round-trips in the hot bucketing path removed in favor of pure arithmetic, relying on `Math.round`'s tolerance for floating-point noise at that magnitude. (An earlier version also guarded a redundant chart-library `applyOptions()` call that fired on every trade; moot now since the chart panel was removed entirely — see Known limitations.)

## Known limitations

- **BTCUSDT only** — no symbol switching; every venue's symbol is hardcoded in `SYM_BTC`.
- **No historical chart** — an earlier version had a 10,000-candle lightweight-charts panel; it was removed to cut footprint and simplify to just the ladder and order flow.
- **Best bid/ask is Binance-only** — other venues contribute to the aggregated size figures and the tape, but not to the reference spread/price shown in the header or spread row (a deliberate speed trade-off; see Performance).
- **No persistence** — refreshing the page drops all local state. This is intentional: the artifact sandbox this runs in doesn't support `localStorage`.
- **Liquidity is exchange-reported, not executable-guaranteed** — resting size can include iceberg orders, partial fills in flight, or stale levels during a venue's own resync; treat the ladder as a liquidity *indicator*, not a guarantee of fillable size.

## Browser requirements

Needs the [`DecompressionStream`](https://developer.mozilla.org/en-US/docs/Web/API/DecompressionStream) API (used to decode gzip-compressed frames from Bitfinex, HTX, and BingX): Chrome/Edge 80+, Firefox 113+, Safari 16.4+. Everything else is standard WebSocket/fetch, supported everywhere relevant.

## File map

Single HTML file, roughly 650 lines, no external requests except the exchange endpoints themselves and a Google Fonts stylesheet link.

| Lines | Section |
|---|---|
| 1–91 | `<style>` — theme variables, layout, ladder/tape CSS |
| 93–122 | Body markup — header, ladder panel, tape panel |
| 127–176 | Constants, shared state, formatting/bucketing utilities |
| 178–233 | Multi-venue infra — `venueBooks`, reconnect/backoff, gzip decode |
| 234–359 | `A` — one entry per venue: connect, subscribe, parse |
| 361–486 | Ladder pool, chase tracker, `renderLadder()`, `paintLadder()` |
| 487–520 | Throttled render/tape loop |
| 522–618 | Binance authoritative feed — depth snapshot+diff, trade/ticker stream |
| 620–648 | Price display, tick-input wiring, boot sequence |

## Disclaimer

Personal/educational trading tool, not financial advice. Displayed depth is a sum of self-reported resting order sizes across venues at a point in time and can diverge from true executable liquidity, all of this code was purely written by AI use with awarness.

##License
Open-Source under the[MIT License](https://github.com/WisTex/The-MIT-License-Files/blob/main/LICENSE)
