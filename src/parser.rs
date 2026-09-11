use scraper::{Html, Selector, ElementRef};
use url::Url;
use crate::types::{PageMeta, PageNode, ParseResult, is_ad_url, is_junk_element};

// ─── Image proxy URL builder ──────────────────────────────────────────────────
fn img_proxy_url(src: &str, engine_base: &str, max_w: u32) -> String {
    if src.starts_with("data:") {
        return src.to_string();
    }
    format!("{}/img?url={}&w={}", engine_base, urlencoding::encode(src), max_w)
}

// ─── Resolve relative URL to absolute ────────────────────────────────────────
fn resolve_url(href: &str, base: &Url) -> Option<String> {
    if href.is_empty() { return None; }
    if href.starts_with("data:") { return Some(href.to_string()); }
    if href.starts_with("//") {
        return Some(format!("https:{}", href));
    }
    base.join(href).ok().map(|u| u.to_string())
}

// ─── Extract YouTube video ID ─────────────────────────────────────────────────
fn youtube_id(url: &str) -> Option<String> {
    let patterns = [
        "youtube.com/embed/",
        "youtube.com/watch?v=",
        "youtu.be/",
        "youtube-nocookie.com/embed/",
    ];
    for pat in &patterns {
        if let Some(pos) = url.find(pat) {
            let after = &url[pos + pat.len()..];
            let id: String = after.chars()
                .take(11)
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if id.len() == 11 { return Some(id); }
        }
    }
    None
}

// ─── Get meta tag content ─────────────────────────────────────────────────────
fn meta_content(doc: &Html, property: &str, name: &str) -> String {
    let sel = if !property.is_empty() {
        Selector::parse(&format!("meta[property='{}']", property)).ok()
    } else {
        None
    };
    let sel2 = if !name.is_empty() {
        Selector::parse(&format!("meta[name='{}']", name)).ok()
    } else {
        None
    };

    if let Some(s) = sel {
        if let Some(el) = doc.select(&s).next() {
            if let Some(v) = el.value().attr("content") {
                let v = v.trim().to_string();
                if !v.is_empty() { return v; }
            }
        }
    }
    if let Some(s) = sel2 {
        if let Some(el) = doc.select(&s).next() {
            if let Some(v) = el.value().attr("content") {
                return v.trim().to_string();
            }
        }
    }
    String::new()
}

// ─── Score a block element for content likelihood ─────────────────────────────
fn score_element(el: &ElementRef) -> f32 {
    let text = el.text().collect::<String>();
    let text_len = text.trim().len() as f32;
    if text_len < 100.0 { return 0.0; }

    let link_sel = Selector::parse("a").unwrap();
    let link_chars: f32 = el.select(&link_sel)
        .map(|a| a.text().collect::<String>().len() as f32)
        .sum();
    let link_density = link_chars / text_len.max(1.0);

    let p_sel = Selector::parse("p").unwrap();
    let p_count = el.select(&p_sel).count() as f32;

    let tag = el.value().name();
    let class = el.value().attr("class").unwrap_or("");
    let id = el.value().attr("id").unwrap_or("");

    let tag_bonus = match tag {
        "article" | "main" => 30.0,
        _ => 0.0,
    };
    let class_bonus = if class.contains("article") || class.contains("content") ||
                         class.contains("entry") || class.contains("post") ||
                         class.contains("story") || class.contains("body") ||
                         id.contains("article") || id.contains("content") ||
                         id.contains("main") {
        20.0
    } else { 0.0 };

    let junk_penalty = if is_junk_element(class, id) { -1000.0 } else { 0.0 };

    (text_len * 0.5)
        + (p_count * 15.0)
        + tag_bonus
        + class_bonus
        + junk_penalty
        - (link_density * 200.0)
}

// ─── Walk element tree and emit typed nodes ───────────────────────────────────
fn walk_element(el: &ElementRef, base: &Url, engine_base: &str, nodes: &mut Vec<PageNode>) {
    let tag = el.value().name().to_lowercase();
    let class = el.value().attr("class").unwrap_or("").to_lowercase();
    let id = el.value().attr("id").unwrap_or("").to_lowercase();

    if is_junk_element(&class, &id) { return; }

    match tag.as_str() {
        "script" | "style" | "noscript" | "head" | "meta" | "link" |
        "svg" | "path" | "nav" | "aside" | "footer" | "form" |
        "button" | "input" | "select" | "textarea" => return,
        _ => {}
    }

    match tag.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = tag.chars().last().unwrap().to_digit(10).unwrap_or(2) as u8;
            let text = el.text().collect::<String>().trim().to_string();
            if text.len() > 1 {
                nodes.push(PageNode::Heading { level, text });
            }
        }
        "p" => {
            let text = el.text().collect::<String>().trim().to_string();
            if text.len() > 20 {
                nodes.push(PageNode::Paragraph { text });
            }
        }
        "img" => {
            let raw_src = el.value().attr("src")
                .or_else(|| el.value().attr("data-src"))
                .or_else(|| el.value().attr("data-lazy-src"))
                .or_else(|| el.value().attr("data-original"))
                .unwrap_or("");

            if raw_src.is_empty() || raw_src.starts_with("data:image/gif") { return; }

            if let Some(abs_src) = resolve_url(raw_src, base) {
                if is_ad_url(&abs_src) { return; }

                let w: u32 = el.value().attr("width").and_then(|w| w.parse().ok()).unwrap_or(999);
                let h: u32 = el.value().attr("height").and_then(|h| h.parse().ok()).unwrap_or(999);
                if w > 0 && h > 0 && (w < 10 || h < 10) { return; }

                let alt = el.value().attr("alt").unwrap_or("").to_string();
                let proxied_src = img_proxy_url(&abs_src, engine_base, 900);

                nodes.push(PageNode::Image {
                    src: proxied_src,
                    alt,
                    caption: String::new(),
                });
            }
        }
        "figure" => {
            let cap_sel = Selector::parse("figcaption").unwrap();
            let caption = el.select(&cap_sel)
                .next()
                .map(|c| c.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let img_sel = Selector::parse("img").unwrap();
            if let Some(img) = el.select(&img_sel).next() {
                let raw_src = img.value().attr("src")
                    .or_else(|| img.value().attr("data-src"))
                    .unwrap_or("");
                if let Some(abs_src) = resolve_url(raw_src, base) {
                    if !is_ad_url(&abs_src) {
                        let alt = img.value().attr("alt").unwrap_or("").to_string();
                        let proxied_src = img_proxy_url(&abs_src, engine_base, 900);
                        nodes.push(PageNode::Image { src: proxied_src, alt, caption });
                    }
                }
            }
        }
        "ul" | "ol" => {
            let li_sel = Selector::parse("li").unwrap();
            let items: Vec<String> = el.select(&li_sel)
                .map(|li| li.text().collect::<String>().trim().to_string())
                .filter(|t| t.len() > 3)
                .collect();
            if !items.is_empty() {
                nodes.push(PageNode::List {
                    ordered: tag == "ol",
                    items,
                });
            }
        }
        "blockquote" => {
            let text = el.text().collect::<String>().trim().to_string();
            if text.len() > 10 {
                nodes.push(PageNode::Blockquote { text });
            }
        }
        "pre" => {
            let code_sel = Selector::parse("code").unwrap();
            let (content, lang) = if let Some(code) = el.select(&code_sel).next() {
                let lang = code.value().attr("class")
                    .and_then(|c| {
                        c.split_whitespace()
                            .find(|p| p.starts_with("language-"))
                            .map(|p| p.replace("language-", ""))
                    })
                    .unwrap_or_default();
                (code.text().collect::<String>().trim().to_string(), lang)
            } else {
                (el.text().collect::<String>().trim().to_string(), String::new())
            };

            if content.len() > 5 {
                nodes.push(PageNode::Code { language: lang, content });
            }
        }
        "table" => {
            let th_sel = Selector::parse("thead th, thead td").unwrap();
            let headers: Vec<String> = el.select(&th_sel)
                .map(|th| th.text().collect::<String>().trim().to_string())
                .collect();

            let tr_sel = Selector::parse("tbody tr").unwrap();
            let td_sel = Selector::parse("td, th").unwrap();
            let rows: Vec<Vec<String>> = el.select(&tr_sel)
                .map(|tr| {
                    tr.select(&td_sel)
                        .map(|td| td.text().collect::<String>().trim().to_string())
                        .collect()
                })
                .filter(|row: &Vec<String>| row.iter().any(|c| !c.is_empty()))
                .collect();

            if !rows.is_empty() {
                nodes.push(PageNode::Table { headers, rows });
            }
        }
        "iframe" => {
            let src = el.value().attr("src").unwrap_or("");
            if src.contains("youtube") || src.contains("youtu.be") {
                if let Some(video_id) = youtube_id(src) {
                    nodes.push(PageNode::Video {
                        platform: "youtube".to_string(),
                        video_id: Some(video_id),
                        src: None,
                    });
                }
            } else if src.contains("vimeo") {
                nodes.push(PageNode::Video {
                    platform: "vimeo".to_string(),
                    video_id: None,
                    src: Some(src.to_string()),
                });
            }
        }
        "hr" => {
            if !nodes.last().map(|n| matches!(n, PageNode::Divider)).unwrap_or(true) {
                nodes.push(PageNode::Divider);
            }
        }
        _ => {
            for child in el.children() {
                if let Some(child_el) = ElementRef::wrap(child) {
                    walk_element(&child_el, base, engine_base, nodes);
                }
            }
        }
    }
}

// ─── Main parse function ──────────────────────────────────────────────────────
pub fn parse_page(html: &str, url: &str, engine_base: &str) -> ParseResult {
    let base_url = match Url::parse(url) {
        Ok(u) => u,
        Err(_) => return ParseResult::proxy_fallback(url, "invalid_url", PageMeta::default()),
    };

    let doc = Html::parse_document(html);

    let og_title = meta_content(&doc, "og:title", "");
    let title_sel = Selector::parse("title").unwrap();
    let page_title = if !og_title.is_empty() {
        og_title
    } else {
        doc.select(&title_sel)
            .next()
            .map(|t| t.text().collect::<String>().trim().to_string())
            .unwrap_or_default()
    };

    let hostname = base_url.host_str().unwrap_or("").to_string();
    let site_name_raw = meta_content(&doc, "og:site_name", "");
    let site_name = if site_name_raw.is_empty() {
        hostname.trim_start_matches("www.").to_string()
    } else {
        site_name_raw
    };

    let icon_sel = Selector::parse(
        "link[rel='icon'], link[rel='shortcut icon'], link[rel='apple-touch-icon']"
    ).unwrap();
    let favicon_raw = doc.select(&icon_sel)
        .next()
        .and_then(|e| e.value().attr("href"))
        .unwrap_or("/favicon.ico");
    let favicon = resolve_url(favicon_raw, &base_url)
        .unwrap_or_else(|| format!("{}/favicon.ico", base_url.origin().ascii_serialization()));

    let meta = PageMeta {
        title: page_title,
        description: {
            let d = meta_content(&doc, "og:description", "description");
            if d.is_empty() { meta_content(&doc, "twitter:description", "") } else { d }
        },
        image: {
            let i = meta_content(&doc, "og:image", "");
            if i.is_empty() { meta_content(&doc, "twitter:image", "") } else { i }
        },
        site_name,
        favicon,
        lang: {
            let html_sel = Selector::parse("html").unwrap();
            doc.select(&html_sel)
                .next()
                .and_then(|h| h.value().attr("lang"))
                .unwrap_or("en")
                .to_string()
        },
        author: {
            let a = meta_content(&doc, "article:author", "author");
            if a.is_empty() { meta_content(&doc, "twitter:creator", "") } else { a }
        },
        published_time: meta_content(&doc, "article:published_time", ""),
        reading_time: 0,
        url: url.to_string(),
    };

    let body_sel = Selector::parse("body").unwrap();
    let body_text = doc.select(&body_sel)
        .next()
        .map(|b| b.text().collect::<String>())
        .unwrap_or_default();
    let body_text_len = body_text.trim().len();

    let spa_sel = Selector::parse("#root, #app, #__next, [data-reactroot]").unwrap();
    let has_spa_root = doc.select(&spa_sel).next().is_some();

    if has_spa_root && body_text_len < 350 {
        return ParseResult::proxy_fallback(url, "spa", meta);
    }

    if html.contains("__cf_chl_rt_tk") || html.contains("cf_chl_")
        || (html.contains("Cloudflare") && html.contains("challenge"))
    {
        return ParseResult::proxy_fallback(url, "cloudflare_challenge", meta);
    }

    let container_sel = Selector::parse(
        "article, main, [class*='article'], [class*='content'], \
         [class*='entry'], [class*='post-body'], [class*='story'], div"
    ).unwrap();

    let best_el = doc.select(&container_sel)
        .filter_map(|el| {
            let s = score_element(&el);
            if s > 0.0 { Some((el, s)) } else { None }
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut nodes: Vec<PageNode> = Vec::new();

    if let Some((best, _score)) = best_el {
        for child in best.children() {
            if let Some(child_el) = ElementRef::wrap(child) {
                walk_element(&child_el, &base_url, engine_base, &mut nodes);
            }
        }
    }

    let paragraph_count = nodes.iter().filter(|n| matches!(n, PageNode::Paragraph { .. })).count();
    if paragraph_count < 2 {
        return ParseResult::proxy_fallback(url, "low_content", meta);
    }

    ParseResult {
        success: true,
        requires_proxy: false,
        reason: None,
        meta,
        nodes,
        url: url.to_string(),
        cached: None,
    }
}
