use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Vision settings, from environment so CLI and GUI can override without a
/// config file. Defaults point at the user's local engine.
pub struct VisionConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_secs: u64,
}

impl VisionConfig {
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("RAG_VISION_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11234/v1".to_string()),
            api_key: std::env::var("RAG_VISION_KEY").unwrap_or_default(),
            model: std::env::var("RAG_VISION_MODEL")
                .unwrap_or_else(|_| "ddalcu/Qwen3.8-Flash-Next-MLX-Serve-mixed-4-8bit".to_string()),
            timeout_secs: std::env::var("RAG_VISION_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(600),
        }
    }

    pub fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// 127.0.0.1, ::1, localhost, `.local` mDNS names, and the private ranges.
/// Cloud endpoints are not allowed for now, so anything else is refused before
/// a single byte leaves.
pub fn is_local_host(host: &str) -> bool {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") || host == "::1" || host == "127.0.0.1" {
        return true;
    }
    if host.to_ascii_lowercase().ends_with(".local") {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
        }
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}

/// Refuse a non-local chat endpoint before a single byte leaves. Shared by
/// captioning and chatting: the same local-only rule guards both paths out of
/// this machine, so a reworded chat prompt cannot become a leak around it.
pub fn ensure_local_endpoint(url: &str) -> Result<()> {
    let Some(host) = url.split("://").nth(1).and_then(|rest| rest.split('/').next()) else {
        return Ok(());
    };
    let host = host_without_port(host);
    if is_local_host(host) {
        Ok(())
    } else {
        bail!("refusing a non-local endpoint: {host} (local-only for now)")
    }
}

/// One chat turn against the configured engine. `body` is the caller's
/// completed request; the local-only guard runs before the request is sent.
pub fn chat(config: &VisionConfig, body: serde_json::Value) -> Result<String> {
    let url = config.chat_url();
    ensure_local_endpoint(&url)?;

    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let request = ureq::post(&url)
        .header("Content-Type", "application/json")
        .config()
        .timeout_global(Some(timeout))
        .build();
    let mut response = request
        .send(body.to_string())
        .with_context(|| format!("request to {url} failed"))?;

    let status = response.status();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    if status != 200 {
        bail!(
            "model returned {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&text).with_context(|| "model returned non-JSON")?;
    Ok(parsed["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string())
}

/// Caption a page image. `png` is the rendered page; `hint` is the source
/// context (filename, page number) so the model can tie labels to meaning.
/// Reads `content` from the response — the reasoning field is not a caption.
/// Empty content is a failure, not a success.
/// Strip a port (and IPv6 brackets) so `127.0.0.1:8000` checks as `127.0.0.1`.
fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[').and_then(|h| h.split(']').next()) {
        return rest;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
}

/// Prompt text, versioned because a caption cache keyed only on the page image
/// would keep serving captions made by an outdated prompt. Bump this whenever
/// the wording below changes.
pub const PROMPT_VERSION: &str = "v1";

pub fn caption_prompt(hint: &str) -> String {
    format!(
        "You are indexing a document for retrieval. Describe this figure/diagram clearly \
for search. Name the key elements and transcribe their labels \u{2014} altitudes, angles, \
distances, gradients, callouts \u{2014} exactly as printed. Answer in two or three plain \
sentences with no headings, bold or bullet lists. Context: {hint}"
    )
}

/// What the user configured, as distinct from what one run uses. Persisted
/// beside the index so the GUI remembers it between launches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisionSettings {
    /// Off by default: captioning costs seconds of GPU time per figure, so it
    /// should be something the user turns on knowing that.
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl Default for VisionSettings {
    fn default() -> Self {
        let configured = VisionConfig::from_env();
        Self {
            enabled: false,
            base_url: configured.base_url,
            api_key: configured.api_key,
            model: configured.model,
        }
    }
}

impl VisionSettings {
    fn path_for(paths: &crate::config::Paths) -> PathBuf {
        paths.data.join("vision.json")
    }

    /// A missing file means defaults. A file that will not parse is an error
    /// rather than a silent reset, because resetting would quietly drop the
    /// user's endpoint and model.
    pub fn load(paths: &crate::config::Paths) -> Result<Self> {
        let path = Self::path_for(paths);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("{} is not readable vision settings", path.display()))
    }

    pub fn save(&self, paths: &crate::config::Paths) -> Result<()> {
        paths.ensure()?;
        let path = Self::path_for(paths);
        std::fs::write(&path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", path.display()))
    }

    pub fn config(&self) -> VisionConfig {
        VisionConfig {
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            timeout_secs: VisionConfig::from_env().timeout_secs,
        }
    }
}

/// A small synthetic figure, so "does this endpoint take images?" can be
/// answered without asking the user to pick a file. Advertised capabilities
/// have already proved unreliable here, so the only real test is sending one.
pub fn probe_image() -> Result<Vec<u8>> {
    let mut image = image::RgbaImage::from_pixel(96, 64, image::Rgba([255, 255, 255, 255]));
    // A dark bar and a few marks: enough that a vision model has something to
    // say, small enough that the request is a couple of hundred bytes.
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        if y > 40 || (x + y) % 17 == 0 {
            *pixel = image::Rgba([20, 20, 20, 255]);
        }
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .context("could not encode the probe image")?;
    Ok(cursor.into_inner())
}

/// Send a real image and report what came back. An empty answer means the
/// endpoint took the request but could not see the image.
pub fn probe_endpoint(config: &VisionConfig) -> Result<String> {
    caption_image(
        config,
        &probe_image()?,
        "This is a test image. Describe what you can see in one short sentence.",
    )
}

pub fn caption_image(
    config: &VisionConfig,
    png: &[u8],
    hint: &str,
) -> Result<String> {
    let prompt = caption_prompt(hint);
    let body = json!({
        "model": config.model,
        "temperature": 0.2,
        // A caption becomes one indexed chunk, and a chunk over 375 words is silently
        // truncated at the embedder's 512-token limit. An unbounded prompt on a dense
        // ICAO figure produced a 416-word essay; this keeps it far inside the cap.
        "max_tokens": 256,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": {
                    "url": format!("data:image/png;base64,{}", base64_encode(png))
                }}
            ]
        }]
    });

    let content = chat(config, body)?;
    if content.is_empty() {
        bail!("vision model returned an empty caption (degenerate image or tiny budget)");
    }
    Ok(content)
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        let third = if chunk.len() > 1 { TABLE[((triple >> 6) & 63) as usize] as char } else { '=' };
        let fourth = if chunk.len() > 2 { TABLE[(triple & 63) as usize] as char } else { '=' };
        out.push(third);
        out.push(fourth);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_private_hosts_are_allowed() {
        assert!(is_local_host("127.0.0.1"));
        assert!(is_local_host("localhost"));
        assert!(is_local_host("192.168.1.20"));
        assert!(is_local_host("10.0.0.5"));
        assert!(is_local_host("::1"));
    }

    #[test]
    fn public_hosts_are_refused() {
        assert!(!is_local_host("api.openai.com"));
        assert!(!is_local_host("8.8.8.8"));
        assert!(!is_local_host("172.32.0.1"));
    }

    #[test]
    fn the_chat_guard_refuses_public_endpoints() {
        ensure_local_endpoint("http://127.0.0.1:11234/v1/chat/completions").unwrap();
        ensure_local_endpoint("http://192.168.1.20:8080/v1").unwrap();
        ensure_local_endpoint("http://localhost:8000/v1").unwrap();
        assert!(ensure_local_endpoint("https://api.openai.com/v1").is_err());
        assert!(ensure_local_endpoint("https://8.8.8.8/v1").is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }

    /// Live caption against the local engine. Needs the vision engine; the PDF
    /// defaults to the copy in this repo's `docs/`, or set RAG_VISION_PDF.
    #[test]
    #[ignore = "needs the local vision engine"]
    fn caption_a_real_page() {
        let config = VisionConfig::from_env();
        let path = std::env::var("RAG_VISION_PDF").unwrap_or_else(|_| {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/Doc 8168 Vol 2 - Aircraft Operations, Construction of Visual and Instrument Flight Procedures.pdf"
            )
            .to_string()
        });
        let png = crate::render::render_page_png(std::path::Path::new(&path), 0, 1200, 1600)
            .unwrap();
        let caption = caption_image(&config, &png, "Doc 8168 Vol 2, page 1").unwrap();
        println!("caption: {caption}");
        assert!(caption.len() > 20);
    }

    #[test]
    #[ignore = "live: sends the probe image to the configured endpoint"]
    fn probe_a_real_endpoint() {
        let config = VisionConfig::from_env();
        let answer = probe_endpoint(&config).unwrap();
        println!("probe said: {answer}");
        assert!(answer.len() > 10, "endpoint returned an empty description");
    }

    fn temp_data(name: &str) -> crate::config::Paths {
        let root = std::env::temp_dir().join(format!("rag-vision-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        crate::config::Paths {
            data: root.clone(),
            models: root.join("models"),
            index: root.join("index"),
        }
    }

    #[test]
    fn vision_settings_round_trip() {
        let paths = temp_data("round-trip");
        let settings = VisionSettings {
            enabled: true,
            base_url: "http://192.168.1.20:8080/v1".to_string(),
            api_key: "secret".to_string(),
            model: "some-vl".to_string(),
        };
        settings.save(&paths).unwrap();
        assert_eq!(VisionSettings::load(&paths).unwrap(), settings);
        std::fs::remove_dir_all(&paths.data).ok();
    }

    #[test]
    fn vision_settings_default_to_disabled() {
        let paths = temp_data("disabled");
        let loaded = VisionSettings::load(&paths).unwrap();
        assert!(!loaded.enabled, "captioning must be opted into, not assumed");
    }

    #[test]
    fn unreadable_vision_settings_is_an_error_not_a_reset() {
        let paths = temp_data("unreadable");
        std::fs::write(paths.data.join("vision.json"), "{ truncated").unwrap();
        let error = VisionSettings::load(&paths).unwrap_err();
        assert!(
            error.to_string().contains("not readable vision settings"),
            "the error should name the file: {error}"
        );
        std::fs::remove_dir_all(&paths.data).ok();
    }

    #[test]
    fn the_probe_image_is_a_png_a_model_can_actually_read() {
        let png = probe_image().unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // A blank 96x64 encodes to well under this; if it grew to megabytes the
        // probe would be slower than a real caption.
        assert!(png.len() < 100_000, "probe image was {} bytes", png.len());
    }
}
