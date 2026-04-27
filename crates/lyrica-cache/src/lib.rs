use std::path::PathBuf;

use anyhow::Result;
use tracing::info;

use lyrica_core::lyrics::Lyrics;

/// File-based lyrics cache. Default location is per-platform:
///   Linux:   `$XDG_CACHE_HOME/lyrica/lyrics` (or `~/.cache/lyrica/lyrics`)
///   macOS:   `~/Library/Caches/lyrica/lyrics`
pub struct LyricsCache {
    cache_dir: PathBuf,
}

impl LyricsCache {
    pub fn new(cache_dir: Option<&str>) -> Result<Self> {
        let dir = if let Some(d) = cache_dir {
            if d.is_empty() {
                default_cache_dir()
            } else {
                PathBuf::from(d)
            }
        } else {
            default_cache_dir()
        };

        std::fs::create_dir_all(&dir)?;
        info!(path = %dir.display(), "Lyrics cache initialized");

        Ok(Self { cache_dir: dir })
    }

    /// Generate cache key from title and artist.
    fn cache_key(title: &str, artist: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        title.to_lowercase().hash(&mut hasher);
        artist.to_lowercase().hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Try to load cached lyrics for a given title/artist.
    pub fn get(&self, title: &str, artist: &str) -> Option<Lyrics> {
        let key = Self::cache_key(title, artist);
        let path = self.cache_dir.join(format!("{}.json", key));

        if !path.exists() {
            return None;
        }

        let data = std::fs::read_to_string(&path).ok()?;
        let lyrics: Lyrics = serde_json::from_str(&data).ok()?;
        info!(title, artist, "Loaded lyrics from cache");
        Some(lyrics)
    }

    /// Save lyrics to cache.
    pub fn put(&self, title: &str, artist: &str, lyrics: &Lyrics) -> Result<()> {
        let key = Self::cache_key(title, artist);
        let path = self.cache_dir.join(format!("{}.json", key));

        let data = serde_json::to_string_pretty(lyrics)?;
        std::fs::write(&path, data)?;
        info!(title, artist, "Saved lyrics to cache");
        Ok(())
    }
}

fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("lyrica")
        .join("lyrics")
}
