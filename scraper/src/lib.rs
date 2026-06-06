use std::sync::OnceLock;

pub mod archive_scraper;
pub mod scraper;

#[cfg(test)]
pub mod tests;

/// How HTTP clients this process builds should handle proxies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyMode {
    /// reqwest default: read proxy env vars *and* perform OS proxy detection
    /// (macOS SystemConfiguration). This is the normal desktop behavior.
    #[default]
    System,
    /// No proxy at all — direct connections. Skips OS proxy detection, so it
    /// avoids the macOS SystemConfiguration mach lookup that panics in sandboxes
    /// without a proxy. Use where egress is direct.
    None,
    /// Use the proxy from the standard env vars (`HTTPS_PROXY`/`HTTP_PROXY`/
    /// `ALL_PROXY`) explicitly, while skipping OS proxy detection. Use in
    /// sandboxes that *require* an egress proxy but block the SystemConfiguration
    /// mach lookup (e.g. Claude Code).
    FromEnv,
}

/// Process-wide proxy setting. Set once at startup from a CLI flag; `None` (the
/// `OnceLock` being empty) means [`ProxyMode::System`].
static PROXY_MODE: OnceLock<ProxyMode> = OnceLock::new();

/// Select how all HTTP clients this process builds handle proxies. Call once at
/// startup, before any scraping; later calls are ignored.
pub fn set_proxy_mode(mode: ProxyMode) {
    let _ = PROXY_MODE.set(mode);
}

fn proxy_mode() -> ProxyMode {
    PROXY_MODE.get().copied().unwrap_or_default()
}

/// Resolve the two mutually-exclusive CLI proxy flags into a [`ProxyMode`].
/// `--no-proxy` wins if both are somehow set (the CLI marks them conflicting).
pub fn proxy_mode_from_flags(no_proxy: bool, proxy_from_env: bool) -> ProxyMode {
    match (no_proxy, proxy_from_env) {
        (true, _) => ProxyMode::None,
        (false, true) => ProxyMode::FromEnv,
        (false, false) => ProxyMode::System,
    }
}

/// First non-empty value among the standard proxy env vars (upper- and
/// lower-case), preferring the HTTPS/ALL variants since scrape targets are HTTPS.
fn env_proxy_url() -> Option<String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

/// Build a blocking HTTP client for the given [`ProxyMode`]. Pure in its argument
/// (testable). `None`/`FromEnv` both skip reqwest's OS proxy detection (the part
/// that panics in a sandboxed macOS), with `FromEnv` adding an explicit proxy
/// read from the environment.
pub fn build_http_client(mode: ProxyMode) -> reqwest::blocking::Client {
    let mut builder = reqwest::blocking::Client::builder();
    match mode {
        ProxyMode::System => {}
        ProxyMode::None => {
            builder = builder.no_proxy();
        }
        ProxyMode::FromEnv => {
            // `no_proxy()` (called first) disables OS detection + env auto-pickup;
            // we then add the env proxy explicitly so the only proxy used is the
            // one we resolved, never the SystemConfiguration one.
            builder = builder.no_proxy();
            if let Some(url) = env_proxy_url() {
                if let Ok(proxy) = reqwest::Proxy::all(&url) {
                    builder = builder.proxy(proxy);
                }
            }
        }
    }
    builder.build().expect("failed to build HTTP client")
}

/// A blocking HTTP client honoring the process-wide [`set_proxy_mode`] setting.
/// Use this instead of `reqwest::blocking::Client::new()` so the proxy flags take
/// effect everywhere.
pub fn http_client() -> reqwest::blocking::Client {
    build_http_client(proxy_mode())
}

#[cfg(test)]
mod http_tests {
    use super::*;

    #[test]
    fn build_http_client_none_constructs() {
        // `None` skips reqwest's macOS SystemConfiguration proxy lookup, so this
        // must build even where that mach lookup is blocked.
        let _client = build_http_client(ProxyMode::None);
    }

    #[test]
    fn build_http_client_from_env_constructs() {
        // `FromEnv` also skips OS detection; it builds whether or not a proxy env
        // var is set (an explicit proxy is only added when one is present).
        let _client = build_http_client(ProxyMode::FromEnv);
    }

    #[test]
    fn proxy_mode_defaults_to_system() {
        assert_eq!(ProxyMode::default(), ProxyMode::System);
    }

    #[test]
    fn proxy_mode_from_flags_maps_each_combination() {
        assert_eq!(proxy_mode_from_flags(false, false), ProxyMode::System);
        assert_eq!(proxy_mode_from_flags(true, false), ProxyMode::None);
        assert_eq!(proxy_mode_from_flags(false, true), ProxyMode::FromEnv);
        // Defensive: if both arrive, no-proxy wins.
        assert_eq!(proxy_mode_from_flags(true, true), ProxyMode::None);
    }
}

pub use crate::archive_scraper::{
    fetch_archive_month, get_last_day_of_month, parse_archive_html, ConcertListing,
};
pub use crate::scraper::{
    extract_content, extract_musicians, extract_og_description, extract_preview_image_url,
    extract_set_list, extract_teaser_from_html, fetch_bytes, fetch_html, parse_concert_info,
    save_concert_info, save_concert_info_to, scrape_data, ConcertInfo, Musician, Song,
};
