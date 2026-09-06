use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Paths;

/// Captions of figure pages, kept outside the index because they cost seconds
/// of vision inference each. Keyed on the rendered page image; a caption made
/// by a different model or an older prompt is treated as a miss and replaced.
pub struct CaptionCache {
    path: PathBuf,
    entries: HashMap<String, CaptionEntry>,
    dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CaptionEntry {
    /// Model that produced this caption.
    model: String,
    prompt_version: String,
    caption: String,
}

/// Hash of a rendered page image — the stable identity of a figure. A page that
/// re-renders identically hits the cache; a re-render after a PDFium or DPI
/// change re-captions, which is the correct response to a different image.
pub fn image_key(png: &[u8]) -> String {
    Sha256::digest(png)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl CaptionCache {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.data.join("captions.jsonl");
        let mut entries = HashMap::new();
        if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                let Ok(record) = serde_json::from_str::<(String, CaptionEntry)>(line) else {
                    continue;
                };
                entries.insert(record.0, record.1);
            }
        }
        Ok(Self {
            path,
            entries,
            dirty: false,
        })
    }

    /// A caption only counts as a hit while the model and prompt that made it
    /// still match; otherwise a stale caption would outlive both.
    pub fn get(&self, png: &[u8], model: &str, prompt_version: &str) -> Option<&str> {
        self.entries
            .get(&image_key(png))
            .filter(|entry| entry.model == model && entry.prompt_version == prompt_version)
            .map(|entry| entry.caption.as_str())
    }

    pub fn put(&mut self, png: &[u8], model: &str, prompt_version: &str, caption: &str) {
        self.entries.insert(
            image_key(png),
            CaptionEntry {
                model: model.to_string(),
                prompt_version: prompt_version.to_string(),
                caption: caption.to_string(),
            },
        );
        self.dirty = true;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Written only when something was added, so a caption-free ingest does not
    /// rewrite the file.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut lines = String::new();
        let mut entries: Vec<(&String, &CaptionEntry)> = self.entries.iter().collect();
        entries.sort_by_key(|(key, _)| key.as_str());
        for (key, entry) in entries {
            lines.push_str(&serde_json::to_string(&(key, entry))?);
            lines.push('\n');
        }
        fs::write(&self.path, lines)
            .with_context(|| format!("could not write {}", self.path.display()))?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(name: &str) -> Paths {
        let root = std::env::temp_dir().join(format!(
            "rag-captions-{}-{}",
            name,
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Paths {
            data: root.clone(),
            models: root.join("models"),
            index: root.join("index"),
        }
    }

    #[test]
    fn same_image_hits_the_cache() {
        let paths = temp_paths("hit");
        let mut cache = CaptionCache::load(&paths).unwrap();
        let png = b"fake page image";
        assert!(cache.get(png, "m1", "v1").is_none());
        cache.put(png, "m1", "v1", "A departure profile with a 4.5% gradient.");
        assert_eq!(
            cache.get(png, "m1", "v1"),
            Some("A departure profile with a 4.5% gradient.")
        );
        assert!(!cache.is_empty());
    }

    #[test]
    fn a_different_image_or_prompt_is_a_miss() {
        let paths = temp_paths("miss");
        let mut cache = CaptionCache::load(&paths).unwrap();
        cache.put(b"figure a", "m1", "v1", "caption a");
        assert!(cache.get(b"figure b", "m1", "v1").is_none());
        assert!(cache.get(b"figure a", "m1", "v2").is_none());
        assert!(cache.get(b"figure a", "m2", "v1").is_none());
    }

    #[test]
    fn captions_survive_a_reload() {
        let paths = temp_paths("reload");
        {
            let mut cache = CaptionCache::load(&paths).unwrap();
            cache.put(b"page", "m1", "v1", "OIS gradients of 2.5% and 3.3%.");
            cache.save().unwrap();
        }
        let cache = CaptionCache::load(&paths).unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(
            cache.get(b"page", "m1", "v1"),
            Some("OIS gradients of 2.5% and 3.3%.")
        );
        std::fs::remove_dir_all(&paths.data).ok();
    }

    #[test]
    fn a_torn_line_does_not_lose_the_rest() {
        let paths = temp_paths("torn");
        {
            let mut cache = CaptionCache::load(&paths).unwrap();
            cache.put(b"page", "m1", "v1", "kept");
            cache.save().unwrap();
        }
        let path = paths.data.join("captions.jsonl");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(r#"{"captions":["cut off mid-write"#);
        std::fs::write(&path, text).unwrap();

        let cache = CaptionCache::load(&paths).unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(b"page", "m1", "v1"), Some("kept"));
        std::fs::remove_dir_all(&paths.data).ok();
    }
}
