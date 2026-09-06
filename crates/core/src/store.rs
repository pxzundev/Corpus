use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::bm25::Bm25;
use crate::config::{CHUNKER_VERSION, EMBED_DIM, EMBED_MODEL, NORMALIZATION};
use crate::embed::cosine;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub text: String,
    pub filename: String,
    /// 1-based PDF page, or -1 when the source has no pages.
    pub page: i32,
    /// Content hash of the source file this chunk came from.
    pub fhash: String,
    /// True when the text is a vision model's description of a figure rather
    /// than text printed on the page. Citations must say so: a caption can
    /// state a number that is not in the drawing.
    #[serde(default)]
    pub figure: bool,
}

/// Everything that invalidates stored vectors, recorded so a mismatch is a
/// startup error instead of a silent retrieval-quality collapse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMetadata {
    pub model: String,
    pub dim: usize,
    pub chunker_version: String,
    pub normalization: String,
    pub chunks: usize,
    pub documents: usize,
}

impl IndexMetadata {
    pub fn current() -> Self {
        Self {
            model: EMBED_MODEL.to_string(),
            dim: EMBED_DIM,
            chunker_version: CHUNKER_VERSION.to_string(),
            normalization: NORMALIZATION.to_string(),
            chunks: 0,
            documents: 0,
        }
    }

    fn matches_current_build(&self) -> std::result::Result<(), String> {
        if self.model != EMBED_MODEL {
            return Err(format!(
                "index was built with {} but this app is pinned to {EMBED_MODEL}",
                self.model
            ));
        }
        if self.dim != EMBED_DIM {
            return Err(format!(
                "index stores {}-dim vectors, this app embeds {EMBED_DIM}-dim",
                self.dim
            ));
        }
        if self.chunker_version != CHUNKER_VERSION {
            return Err(format!(
                "index was chunked with version {} , this app uses {CHUNKER_VERSION}",
                self.chunker_version
            ));
        }
        Ok(())
    }
}

/// Where a document came from. Kept beside the vectors so the GUI can show and
/// open the original file without re-scanning a source directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRecord {
    pub filename: String,
    /// Absolute path of the file that produced these chunks.
    pub path: String,
    pub fhash: String,
    pub chunks: usize,
    pub pages: usize,
}

pub struct Index {
    pub metadata: IndexMetadata,
    pub chunks: Vec<Chunk>,
    /// Parallel to `chunks`.
    pub vectors: Vec<Vec<f32>>,
    bm25: Bm25,
}

impl Index {
    pub fn empty() -> Self {
        Self {
            metadata: IndexMetadata::current(),
            chunks: Vec::new(),
            vectors: Vec::new(),
            bm25: Bm25::new(&[]),
        }
    }

    fn metadata_path(dir: &Path) -> PathBuf {
        dir.join("index.json")
    }

    fn chunks_path(dir: &Path) -> PathBuf {
        dir.join("chunks.jsonl")
    }

    fn vectors_path(dir: &Path) -> PathBuf {
        dir.join("vectors.f32")
    }

    pub fn open(dir: &Path) -> Result<Self> {
        let metadata_path = Self::metadata_path(dir);
        if !metadata_path.exists() {
            bail!(
                "no index at {} — run `corpus index <documents dir>` first",
                dir.display()
            );
        }

        let metadata: IndexMetadata = serde_json::from_slice(
            &fs::read(&metadata_path).with_context(|| format!("could not read {}", metadata_path.display()))?,
        )
        .context("index metadata is corrupt")?;

        metadata
            .matches_current_build()
            .map_err(|reason| anyhow::anyhow!("{reason}\nRe-index to continue."))?;

        let chunks = read_chunks(&Self::chunks_path(dir))?;
        let vectors = read_vectors(&Self::vectors_path(dir), metadata.dim, chunks.len())?;

        let texts: Vec<String> = chunks.iter().map(|chunk| chunk.text.clone()).collect();
        Ok(Self {
            bm25: Bm25::new(&texts),
            metadata,
            chunks,
            vectors,
        })
    }

    /// Writes chunks and vectors before metadata, so an interrupted write cannot
    /// leave metadata claiming a chunk count the data files do not have.
    pub fn save(dir: &Path, chunks: Vec<Chunk>, vectors: Vec<Vec<f32>>) -> Result<Self> {
        let documents = chunks
            .iter()
            .map(|chunk| &chunk.filename)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let metadata = IndexMetadata {
            chunks: chunks.len(),
            documents,
            ..IndexMetadata::current()
        };

        let lines: String = chunks
            .iter()
            .map(|chunk| serde_json::to_string(chunk))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        fs::write(Self::chunks_path(dir), lines).context("could not write chunks")?;

        let mut bytes = Vec::with_capacity(vectors.len() * metadata.dim * 4);
        for vector in &vectors {
            for value in vector {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        fs::write(Self::vectors_path(dir), bytes).context("could not write vectors")?;

        fs::write(
            Self::metadata_path(dir),
            serde_json::to_vec_pretty(&metadata)?,
        )
        .context("could not write index metadata")?;

        let texts: Vec<String> = chunks.iter().map(|chunk| chunk.text.clone()).collect();
        Ok(Self {
            bm25: Bm25::new(&texts),
            metadata,
            chunks,
            vectors,
        })
    }

    pub fn texts(&self) -> Vec<String> {
        self.chunks.iter().map(|chunk| chunk.text.clone()).collect()
    }

    /// Documents that contain a given source file's content hash.
    pub fn files(&self) -> BTreeMap<String, String> {
        self.chunks
            .iter()
            .map(|chunk| (chunk.filename.clone(), chunk.fhash.clone()))
            .collect()
    }

    pub fn dense_search(&self, query: &[f32], limit: usize, scope: Option<&[String]>) -> Vec<(usize, f32)> {
        let mut scored: Vec<(usize, f32)> = self
            .vectors
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                scope.is_none_or(|names| names.iter().any(|name| self.chunks[*index].filename == *name))
            })
            .map(|(index, vector)| (index, cosine(query, vector)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit);
        scored
    }

    pub fn lexical_search(&self, query: &str, limit: usize, scope: Option<&[String]>) -> Vec<(usize, f32)> {
        let mut scored: Vec<(usize, f32)> = self
            .bm25
            .scores(query)
            .into_iter()
            .enumerate()
            .filter(|(index, score)| {
                *score > 0.0
                    && scope.is_none_or(|names| names.iter().any(|name| self.chunks[*index].filename == *name))
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit);
        scored
    }

    /// filename -> (chunk count, tracked page count)
    pub fn document_stats(&self) -> BTreeMap<String, (usize, usize)> {
        let mut stats: BTreeMap<String, (usize, BTreeSet<i32>)> = BTreeMap::new();
        for chunk in &self.chunks {
            let entry = stats.entry(chunk.filename.clone()).or_default();
            entry.0 += 1;
            if chunk.page != -1 {
                entry.1.insert(chunk.page);
            }
        }
        stats
            .into_iter()
            .map(|(name, (chunks, pages))| (name, (chunks, pages.len())))
            .collect()
    }

    /// One page of a document's chunks in reading order, optionally filtered to
    /// chunks containing `query` (plain substring match — this is browsing, not
    /// retrieval, so it must not depend on the embedding model).
    pub fn page_of_document(
        &self,
        filename: &str,
        query: Option<&str>,
        page: Option<i32>,
        offset: usize,
        limit: usize,
    ) -> (usize, Vec<Chunk>) {
        let needle = query.map(|text| text.to_lowercase());
        let matching: Vec<&Chunk> = self
            .chunks
            .iter()
            .filter(|chunk| chunk.filename == filename)
            .filter(|chunk| page.is_none_or(|page| chunk.page == page))
            .filter(|chunk| {
                needle
                    .as_ref()
                    .is_none_or(|needle| chunk.text.to_lowercase().contains(needle))
            })
            .collect();

        let total = matching.len();
        let items = matching
            .into_iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect();
        (total, items)
    }

    pub fn documents_path(dir: &Path) -> PathBuf {
        dir.join("documents.json")
    }

    pub fn read_documents(dir: &Path) -> Result<HashMap<String, DocumentRecord>> {
        let path = Self::documents_path(dir);
        if !path.exists() {
            return Ok(HashMap::new());
        }
        serde_json::from_slice(&fs::read(&path)?).context("document records are corrupt")
    }

    pub fn write_documents(dir: &Path, records: &HashMap<String, DocumentRecord>) -> Result<()> {
        fs::write(Self::documents_path(dir), serde_json::to_vec_pretty(records)?)
            .context("could not write document records")
    }
}

fn read_chunks(path: &Path) -> Result<Vec<Chunk>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("chunk record is corrupt"))
        .collect()
}

fn read_vectors(path: &Path, dim: usize, expected: usize) -> Result<Vec<Vec<f32>>> {
    let bytes = fs::read(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() != expected * dim * 4 {
        bail!(
            "{} holds {} bytes, expected {} for {expected} chunks at {dim} dims",
            path.display(),
            bytes.len(),
            expected * dim * 4
        );
    }
    Ok(bytes
        .chunks_exact(dim * 4)
        .map(|row| {
            row.chunks_exact(4)
                .map(|value| f32::from_le_bytes(value.try_into().expect("4 bytes")))
                .collect()
        })
        .collect())
}
