use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

/// Model pinned for the embedded provider. Changing it invalidates every
/// stored vector, so the id is written into index metadata and checked on open.
pub const EMBED_MODEL: &str = "BAAI/bge-base-en-v1.5";
pub const EMBED_DIM: usize = 768;
pub const RERANK_MODEL: &str = "jinaai/jina-reranker-v1-turbo-en";
pub const EMBED_BATCH: usize = 32;

/// Chunk sizing. The Python implementation counted tokens with tiktoken, whose
/// BPE tables are a separate runtime download; word counts keep chunking
/// offline at the cost of drifting from exact token boundaries. 300 words is
/// roughly the 400 BPE tokens that held one clause together.
pub const CHUNK_WORDS: usize = 300;
/// Hard ceiling. The embedding model truncates input at 512 tokens, so a chunk
/// past this size has text that never reaches the model at all.
pub const CHUNK_MAX_WORDS: usize = 375;
pub const CHUNK_OVERLAP_WORDS: usize = 60;

pub const DEFAULT_K: usize = 6;
pub const HYBRID_CANDIDATES: usize = 20;
pub const RRF_K: f32 = 60.0;

/// Bumped when chunking changes enough that stored vectors should be rebuilt.
pub const CHUNKER_VERSION: &str = "3";

pub const NORMALIZATION: &str = "l2";

/// On-disk layout. Model files and the index both live under one data dir so a
/// container build or portable install can relocate the whole app.
pub struct Paths {
    pub data: PathBuf,
    pub models: PathBuf,
    pub index: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let data = match std::env::var("RAG_DATA_DIR") {
            Ok(dir) => PathBuf::from(dir),
            Err(_) => dirs::data_dir()
                .map(|dir| dir.join("Corpus"))
                .ok_or_else(|| anyhow!("could not resolve a platform data directory"))?,
        };
        Ok(Self {
            models: data.join("models"),
            index: data.join("index"),
            data,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.models)
            .with_context(|| format!("could not create {}", self.models.display()))?;
        std::fs::create_dir_all(&self.index)
            .with_context(|| format!("could not create {}", self.index.display()))?;
        Ok(())
    }
}
