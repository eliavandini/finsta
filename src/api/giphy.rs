//! GIF and sticker search through GIPHY, with your own API key (Preferences).

use anyhow::{Result, bail};
use serde_json::Value;

/// A GIF or sticker from GIPHY: its id, a small (animated) preview, and its page.
#[derive(Debug, Clone)]
pub struct Gif {
    pub id: String,
    pub preview: String,
    /// Height / width.
    pub aspect: f64,
    pub page: String,
    pub sticker: bool,
}

impl super::Client {
    /// GIFs (or stickers) matching `query`, or trending ones when it's empty.
    /// Only the key and the query go to GIPHY, never anything about your account.
    pub async fn search_gifs(&self, key: &str, query: &str, stickers: bool) -> Result<Vec<Gif>> {
        // Can point at a mock server for testing (see dev/mock_api.py).
        let base = std::env::var("FINSTA_GIPHY_BASE").unwrap_or_else(|_| "https://api.giphy.com/v1/".into());
        let kind = if stickers { "stickers" } else { "gifs" };
        let query = query.trim();
        let mut q = vec![("api_key", key.trim()), ("limit", "24")];
        let path = if query.is_empty() {
            "trending"
        } else {
            q.push(("q", query));
            "search"
        };
        // Errors leave out the address: it holds the key.
        let res = self.inner.http.get(format!("{base}{kind}/{path}")).query(&q).send().await.map_err(|e| e.without_url())?;
        let status = res.status();
        if status == 401 || status == 403 {
            bail!("GIPHY didn't accept the API key");
        }
        if !status.is_success() {
            bail!("GIPHY answered {status}");
        }
        let v: Value = res.json().await.map_err(|e| e.without_url())?;
        Ok(parse(&v, stickers))
    }
}

fn parse(v: &Value, sticker: bool) -> Vec<Gif> {
    let s = |x: &Value| x.as_str().unwrap_or_default().to_string();
    let n = |x: &Value| x.as_str().and_then(|t| t.parse::<f64>().ok()).or(x.as_f64()).unwrap_or(0.0);
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            // Animated, small; a sticker's first frame is often empty.
            let size = ["fixed_width_small", "fixed_width_still"]
                .map(|k| &g["images"][k])
                .into_iter()
                .find(|i| !s(&i["webp"]).is_empty() || !s(&i["url"]).is_empty())?;
            let id = s(&g["id"]);
            if id.is_empty() {
                return None;
            }
            let preview = Some(s(&size["webp"])).filter(|u| !u.is_empty()).unwrap_or_else(|| s(&size["url"]));
            let (w, h) = (n(&size["width"]), n(&size["height"]));
            Some(Gif { id, preview, aspect: if w > 0.0 { h / w } else { 1.0 }, page: s(&g["url"]), sticker })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_results() {
        let v = serde_json::json!({"data": [
            {"id": "abc", "url": "https://giphy.com/gifs/abc", "images": {
                "fixed_width_still": {"url": "https://media.giphy.com/s.gif", "width": "200", "height": "100"},
                "fixed_width_small": {"url": "https://media.giphy.com/a.gif", "webp": "https://media.giphy.com/a.webp", "width": "100", "height": "50"}}},
            {"id": "still", "images": {"fixed_width_still": {"url": "https://media.giphy.com/s2.gif", "width": "200", "height": "300"}}},
            {"id": "", "images": {"fixed_width_still": {"url": "x"}}},
            {"id": "none", "images": {}},
        ]});
        let gifs = super::parse(&v, true);
        assert_eq!(gifs.len(), 2);
        assert_eq!((gifs[0].id.as_str(), gifs[0].preview.as_str()), ("abc", "https://media.giphy.com/a.webp"));
        assert!((gifs[0].aspect - 0.5).abs() < 1e-9);
        assert_eq!(gifs[1].preview, "https://media.giphy.com/s2.gif");
        assert!(gifs[1].sticker);
    }
}
