use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Thumbnail {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Thumbnail {
    /// Upscale common YT Music / YouTube thumbnail URLs for better art quality.
    pub fn upscaled_url(&self) -> String {
        upscale_thumbnail_url(&self.url)
    }
}

pub fn upscale_thumbnail_url(url: &str) -> String {
    if url.contains("lh3.googleusercontent.com") || url.contains("googleusercontent.com") {
        if let Some(idx) = url.rfind('=') {
            return format!("{}=w544-h544-l90-rj", &url[..idx]);
        }
        return format!("{url}=w544-h544-l90-rj");
    }
    if url.contains("i.ytimg.com") {
        // Prefer hqdefault — maxresdefault 404s for a large share of Music uploads
        // and used to make every cover fetch fail silently.
        return url
            .replace("maxresdefault.jpg", "hqdefault.jpg")
            .replace("sddefault.jpg", "hqdefault.jpg")
            .replace("mqdefault.jpg", "hqdefault.jpg")
            .replace("hq720.jpg", "hqdefault.jpg")
            .replace("/default.jpg", "/hqdefault.jpg");
    }
    url.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtistRef {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlbumRef {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub album: Option<AlbumRef>,
    pub duration: Option<Duration>,
    pub thumbnails: Vec<Thumbnail>,
    pub explicit: bool,
    pub liked: Option<bool>,
    #[serde(default)]
    pub from_radio: bool,
}

impl Track {
    pub fn artist_names(&self) -> String {
        self.artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn best_thumbnail(&self) -> Option<&Thumbnail> {
        self.thumbnails.iter().max_by_key(|t| t.width.unwrap_or(0))
    }

    /// Best available art URL, with a YouTube CDN fallback when Innertube
    /// omitted thumbnails (common on some playlist/radio rows).
    pub fn art_url(&self) -> Option<String> {
        self.best_thumbnail()
            .map(|t| t.url.clone())
            .or_else(|| {
                if self.video_id.is_empty() {
                    None
                } else {
                    Some(format!(
                        "https://i.ytimg.com/vi/{}/hqdefault.jpg",
                        self.video_id
                    ))
                }
            })
    }

    pub fn duration_label(&self) -> String {
        match self.duration {
            Some(d) => format_duration(d),
            None => "--:--".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub year: Option<String>,
    pub thumbnails: Vec<Thumbnail>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub thumbnails: Vec<Thumbnail>,
    pub top_songs: Vec<Track>,
    pub albums: Vec<Album>,
    pub singles: Vec<Album>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub track_count: Option<u32>,
    pub thumbnails: Vec<Thumbnail>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShelfItem {
    Track(Track),
    Album(Album),
    Playlist(Playlist),
    Artist(Artist),
}

impl ShelfItem {
    /// Cache key for `App.covers` / `FetchArt`.
    pub fn cover_key(&self) -> &str {
        match self {
            Self::Track(t) => &t.video_id,
            Self::Album(a) => &a.id,
            Self::Playlist(p) => &p.id,
            Self::Artist(a) => &a.id,
        }
    }

    /// Best thumbnail URL for this shelf card, if Innertube provided one.
    pub fn art_url(&self) -> Option<String> {
        match self {
            Self::Track(t) => t.art_url(),
            Self::Album(a) => best_thumb_url(&a.thumbnails),
            Self::Playlist(p) => best_thumb_url(&p.thumbnails),
            Self::Artist(a) => best_thumb_url(&a.thumbnails),
        }
    }
}

fn best_thumb_url(thumbnails: &[Thumbnail]) -> Option<String> {
    thumbnails
        .iter()
        .max_by_key(|t| t.width.unwrap_or(0))
        .map(|t| t.url.clone())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shelf {
    pub title: String,
    pub items: Vec<ShelfItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub channel_id: Option<String>,
    pub thumbnails: Vec<Thumbnail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lyrics {
    pub synced: Option<Vec<LyricLine>>,
    pub plain: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricLine {
    pub time: Duration,
    pub text: String,
}

pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upscale_googleusercontent() {
        let url = "https://lh3.googleusercontent.com/abc=w60-h60-l90-rj";
        assert!(upscale_thumbnail_url(url).ends_with("=w544-h544-l90-rj"));
    }

    #[test]
    fn upscale_ytimg() {
        let url = "https://i.ytimg.com/vi/xyz/mqdefault.jpg";
        assert!(upscale_thumbnail_url(url).contains("hqdefault"));
        assert!(!upscale_thumbnail_url(url).contains("maxresdefault"));
        let max = "https://i.ytimg.com/vi/xyz/maxresdefault.jpg";
        assert!(upscale_thumbnail_url(max).contains("hqdefault"));
    }

    #[test]
    fn format_dur() {
        assert_eq!(format_duration(Duration::from_secs(65)), "1:05");
        assert_eq!(format_duration(Duration::from_secs(3661)), "1:01:01");
    }
}
