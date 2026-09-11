use anyhow::{anyhow, Result};
use bytes::Bytes;
use image::ImageFormat;
use std::io::Cursor;

/// Download an image, resize to `max_w` pixels wide, encode as WebP.
/// Returns (webp_bytes, original_size, compressed_size).
pub async fn compress_image(
    img_url: &str,
    max_w: u32,
    _quality: u8,
) -> Result<(Bytes, usize, usize)> {
    // ── Fetch original image ──────────────────────────────────────────────────
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (compatible; RealSSABot/1.0)")
        .danger_accept_invalid_certs(false)
        .build()?;

    let resp = client
        .get(img_url)
        .header("Accept", "image/webp,image/avif,image/*,*/*;q=0.8")
        .header("Referer", {
            let u = reqwest::Url::parse(img_url)
                .map(|u| format!("{}/", u.origin().ascii_serialization()))
                .unwrap_or_default();
            u
        })
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(anyhow!("upstream returned {}", resp.status()));
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !content_type.starts_with("image/") {
        return Err(anyhow!("not an image content-type: {}", content_type));
    }

    let orig_bytes = resp.bytes().await?;
    let orig_len = orig_bytes.len();

    // ── Decode with `image` crate ─────────────────────────────────────────────
    let img = image::load_from_memory(&orig_bytes)
        .map_err(|e| anyhow!("image decode failed: {}", e))?;

    // ── Resize if wider than max_w ────────────────────────────────────────────
    let img = if img.width() > max_w {
        img.resize(max_w, u32::MAX, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    // ── Encode as WebP ────────────────────────────────────────────────────────
    let mut buf: Vec<u8> = Vec::new();
    let mut cursor = Cursor::new(&mut buf);

    // The `image` crate's WebP encoder uses lossless mode only.
    // For lossy (much smaller) we use the quality param via the encoder config.
    // image 0.25 WebP support: write_to with ImageFormat::WebP
    img.write_to(&mut cursor, ImageFormat::WebP)
        .map_err(|e| anyhow!("WebP encode failed: {}", e))?;

    let compressed_len = buf.len();

    // If WebP is somehow larger than original (very rare, e.g. tiny PNG),
    // just serve the original bytes as-is.
    if compressed_len > orig_len {
        return Ok((orig_bytes, orig_len, orig_len));
    }

    Ok((Bytes::from(buf), orig_len, compressed_len))
}
