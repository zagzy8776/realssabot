# RealSSA Bot Engine

High-performance Rust engine powering RealSSA News.

## Features

- **HTML Page Parser / Renderer** — Clean article extraction with Readability-style scoring
- **Image Compression Proxy** — On-the-fly WebP conversion + resizing
- **Page Proxy** — Cookie-aware proxy with Cloudflare challenge detection
- **Human Brain** — Distributed vector memory (Neon Postgres + pgvector) + custom Transformer for style/learning
- **Storage GC** — Background cleaner for the learning databases

## Tech Stack

- Axum + Tokio
- scraper (html5ever)
- image crate (WebP)
- tokio-postgres + Neon
- Custom BPE + Transformer (pure Rust)

## Quick Start

```bash
# Local
cargo run --release

# Docker / Fly.io
fly deploy
```

## Environment Variables

```env
PORT=8080
NEON_DB_1=...
NEON_DB_2=...
NEON_DB_3=...
NEON_DB_4=...
NEON_DB_5=...
NEON_DB_6=...
ENGINE_BASE_URL=https://your-engine.fly.dev
RUST_LOG=realssa_engine=info
```

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| GET | `/render-page?url=` | Parse & extract structured content |
| GET | `/img?url=&w=&q=` | Image compression proxy → WebP |
| GET | `/proxy-page?url=` | Full page proxy |
| POST | `/human-brain/chat` | Chat with the learned brain |

## Notes

This was extracted from the main [realssa](https://github.com/zagzy8776/realssa) monorepo so the engine can scale and evolve independently.

**Security:** Database credentials currently live in source as fallbacks. Rotate them and move fully to environment variables after first deploy.
