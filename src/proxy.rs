use anyhow::{anyhow, Result};

const HISTORY_GUARD: &str = r#"<script>
(function(){
  function safe(fn){
    return function(s,t,u){try{fn.call(history,s,t,u);}catch(e){}};
  }
  if(window.history){
    history.replaceState=safe(history.replaceState);
    history.pushState=safe(history.pushState);
  }
})();
</script>"#;

const LINK_INTERCEPT: &str = r#"<script>
(function(){
  document.addEventListener('click',function(e){
    var el=e.target.closest('a[href]');
    if(!el)return;
    var h=el.getAttribute('href');
    if(!h||h.startsWith('#')||h.startsWith('javascript'))return;
    e.preventDefault();
    try{
      var abs=el.href;
      window.parent.postMessage({type:'REALSSA_NAVIGATE',url:abs},'*');
    }catch(_){}
  },true);
})();
</script>"#;

/// Cloudflare challenge HTML page returned to the iframe when we detect CF.
pub fn cloudflare_fallback(hostname: &str, original_url: &str) -> String {
    format!(r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Browser Verification Required</title>
<style>
body{{background:#0b0f17;color:#e5e7eb;font-family:system-ui,sans-serif;
  display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;padding:1rem;}}
.card{{text-align:center;max-width:360px;}}
.icon{{font-size:3rem;margin-bottom:1rem;}}
h2{{color:#f59e0b;font-size:1.2rem;margin:0 0 .5rem;}}
p{{color:#9ca3af;font-size:.85rem;line-height:1.6;margin:0 0 1.5rem;}}
a{{display:inline-block;background:#f59e0b;color:#000;font-weight:700;
  padding:.6rem 1.4rem;border-radius:.75rem;text-decoration:none;font-size:.85rem;}}
.domain{{color:#f59e0b;font-weight:600;}}
</style>
</head>
<body>
<div class="card">
  <div class="icon">🛡️</div>
  <h2>Browser Verification Required</h2>
  <p><span class="domain">{hostname}</span> uses Cloudflare bot protection which requires a real browser session.</p>
  <a href="{original_url}" target="_blank" rel="noopener">Open in External Browser ↗</a>
</div>
</body>
</html>"#, hostname = hostname, original_url = original_url)
}

/// Prefix cookie names with the target domain to prevent collision.
/// e.g. "session_id=123" -> "_realssa_twitter_com_session_id=123"
pub fn prefix_cookie(set_cookie_val: &str, target_host: &str) -> String {
    let sanitized_host = target_host.replace('.', "_");
    let parts: Vec<&str> = set_cookie_val.split(';').collect();
    if parts.is_empty() { return set_cookie_val.to_string(); }
    
    // Clean and rebuild attributes
    let mut new_parts = Vec::new();
    
    // Re-parse the KV first
    let kv = parts[0].trim();
    let prefixed_kv = if let Some(eq_idx) = kv.find('=') {
        let name = &kv[..eq_idx];
        let val = &kv[eq_idx..];
        format!("_realssa_{}_{}", sanitized_host, name) + val
    } else {
        kv.to_string()
    };
    new_parts.push(prefixed_kv);
    
    for part in parts.iter().skip(1) {
        let trimmed = part.trim();
        let lower = trimmed.to_lowercase();
        if lower.starts_with("domain=") {
            // Strip domain so it binds to the proxy host
            continue;
        }
        if lower == "secure" {
            // Strip secure to allow localhost HTTP testing
            continue;
        }
        if lower.starts_with("samesite=") {
            // Override SameSite to None to allow running inside iframe
            continue;
        }
        new_parts.push(trimmed.to_string());
    }
    
    // Add SameSite=None & Secure for iframe cookie setting support
    new_parts.push("SameSite=None".to_string());
    new_parts.push("Secure".to_string());
    
    new_parts.join("; ")
}

/// Parse client cookies, extract and unprefix cookies matching the target domain.
pub fn extract_prefixed_cookies(cookie_header: &str, target_host: &str) -> String {
    let sanitized_host = target_host.replace('.', "_");
    let prefix = format!("_realssa_{}_", sanitized_host);
    
    let mut extracted = Vec::new();
    for cookie in cookie_header.split(';') {
        let trimmed = cookie.trim();
        if trimmed.starts_with(&prefix) {
            let without_prefix = &trimmed[prefix.len()..];
            extracted.push(without_prefix.to_string());
        }
    }
    extracted.join("; ")
}

/// Fetch `url`, strip framing headers, inject our scripts, return modified HTML.
pub async fn proxy_page(url: &str, incoming_cookies: &str) -> Result<(String, Vec<String>)> {
    let parsed_url = reqwest::Url::parse(url)?;
    let host = parsed_url.host_str().unwrap_or("");
    
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .redirect(reqwest::redirect::Policy::limited(5))
        .danger_accept_invalid_certs(false)
        .build()?;

    // Extract cookies prefixed for this host
    let target_cookies = extract_prefixed_cookies(incoming_cookies, host);

    let mut req = client
        .get(url)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7")
        .header("Accept-Language", "en-NG,en-US;q=0.9,en;q=0.8")
        .header("Cache-Control", "max-age=0")
        .header("Upgrade-Insecure-Requests", "1")
        .header("Sec-Ch-Ua", "\"Chromium\";v=\"124\", \"Google Chrome\";v=\"124\", \"Not-A.Brand\";v=\"99\"")
        .header("Sec-Ch-Ua-Mobile", "?0")
        .header("Sec-Ch-Ua-Platform", "\"Windows\"")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-User", "?1");

    if !target_cookies.is_empty() {
        req = req.header("Cookie", target_cookies);
    }

    let resp = req.send().await?;

    // Collect Set-Cookie headers
    let mut set_cookies = Vec::new();
    for (key, val) in resp.headers().iter() {
        if key.as_str().to_lowercase() == "set-cookie" {
            if let Ok(val_str) = val.to_str() {
                // Prefix the cookie with the target host to prevent collision
                let prefixed = prefix_cookie(val_str, host);
                set_cookies.push(prefixed);
            }
        }
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !content_type.contains("text/html") {
        return Err(anyhow!("not HTML: {}", content_type));
    }

    let mut html = resp.text().await?;

    // ── Cloudflare challenge detection ────────────────────────────────────────
    let is_cf = html.contains("__cf_chl_rt_tk")
        || html.contains("cf_chl_")
        || html.contains("cf-chl-bypass")
        || (html.contains("Cloudflare") && html.contains("challenge"));

    if is_cf {
        let hostname = parsed_url.host_str().unwrap_or(url);
        return Ok((cloudflare_fallback(hostname, url), set_cookies));
    }

    // ── Base URL for relative assets ──────────────────────────────────────────
    let origin = format!("{}/", parsed_url.origin().ascii_serialization());

    // 1. Inject <base> tag
    let base_tag = format!("<base href=\"{}\" target=\"_blank\">", origin);
    if let Some(pos) = html.to_lowercase().find("<head") {
        if let Some(close) = html[pos..].find('>') {
            let insert_at = pos + close + 1;
            html.insert_str(insert_at, &format!("\n{}\n{}", base_tag, HISTORY_GUARD));
        }
    } else {
        html = format!("{}\n{}\n{}", base_tag, HISTORY_GUARD, html);
    }

    // 2. Strip meta CSP tags
    while let Some(start) = html.to_lowercase().find("<meta") {
        if let Some(end) = html[start..].find('>') {
            let tag = &html[start..start + end + 1].to_lowercase();
            if tag.contains("http-equiv") && tag.contains("content-security-policy") {
                html.drain(start..start + end + 1);
                continue;
            }
        }
        break;
    }

    // 3. Inject link interception before </body>
    if let Some(pos) = html.to_lowercase().rfind("</body>") {
        html.insert_str(pos, &format!("{}\n", LINK_INTERCEPT));
    } else {
        html.push_str(LINK_INTERCEPT);
    }

    Ok((html, set_cookies))
}
