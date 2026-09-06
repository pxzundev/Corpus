use anyhow::{Context, Result, bail};
use fastembed::{
    EmbeddingModel, InitOptions, RerankInitOptions, RerankerModel, TextEmbedding, TextRerank,
};

use crate::config::{EMBED_BATCH, EMBED_DIM, EMBED_MODEL, NORMALIZATION, Paths, RERANK_MODEL};

pub struct Encoder {
    inner: TextEmbedding,
}

impl Encoder {
    /// Loads the pinned embedded model, downloading it into the app data dir on
    /// first use. fastembed defaults its cache to a relative `.fastembed_cache`
    /// in the process CWD, which would scatter model files across wherever the
    /// app happened to be launched from.
    pub fn load(paths: &Paths) -> Result<Self> {
        let options = InitOptions::new(EmbeddingModel::BGEBaseENV15)
            .with_cache_dir(paths.models.clone())
            .with_show_download_progress(true);
        let inner = TextEmbedding::try_new(options)
            .with_context(|| format!("failed to load embedded embedding model {EMBED_MODEL}"))?;
        Ok(Self { inner })
    }

    /// Embeds texts into unit-length vectors so a dot product is a cosine
    /// similarity. fastembed handles CLS pooling; L2 normalization is applied
    /// here unless the model already emits unit vectors.
    pub fn encode(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut vectors = self
            .inner
            .embed(texts, Some(EMBED_BATCH))
            .context("embedding inference failed")?;

        let already_normalized = vectors.iter().all(|v| is_normalized(v));
        for vector in &mut vectors {
            if vector.len() != EMBED_DIM {
                bail!(
                    "model returned {}-dim vectors, index expects {}-dim",
                    vector.len(),
                    EMBED_DIM
                );
            }
            if !already_normalized {
                normalize(vector);
            }
        }
        Ok(vectors)
    }

    pub fn encode_one(&mut self, text: &str) -> Result<Vec<f32>> {
        self.encode(&[text])
            .map(|mut vectors| vectors.remove(0))
    }
}

/// Cross-encoder reranking. Loaded only when a query actually reranks, since
/// its weights are a separate download from the embedding model.
pub struct Reranker {
    inner: TextRerank,
}

impl Reranker {
    pub fn load(paths: &Paths) -> Result<Self> {
        let options = RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
            .with_cache_dir(paths.models.clone())
            .with_show_download_progress(true);
        let inner = TextRerank::try_new(options)
            .with_context(|| format!("failed to load reranker {RERANK_MODEL}"))?;
        Ok(Self { inner })
    }

    /// Scores every document against the query, returning scores in the same
    /// order as the input documents.
    pub fn score(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>> {
        let results = self
            .inner
            .rerank(query, documents, false, Some(EMBED_BATCH))
            .context("reranking inference failed")?;

        let mut scores = vec![0.0f32; documents.len()];
        for result in results {
            scores[result.index] = result.score;
        }
        Ok(scores)
    }
}

fn is_normalized(vector: &[f32]) -> bool {
    let norm: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    (norm - 1.0).abs() < 1e-3
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        vector.iter_mut().for_each(|x| *x /= norm);
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Recorded in index metadata so a future change to how vectors are produced is
/// visible instead of silently breaking retrieval.
pub fn normalization_policy() -> &'static str {
    NORMALIZATION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_unit_vector_with_itself_is_one() {
        let v = vec![0.6, 0.8];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_produces_unit_length() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!(is_normalized(&v));
    }

    #[test]
    fn is_normalized_rejects_unnormalized() {
        assert!(is_normalized(&[0.6, 0.8]));
        assert!(!is_normalized(&[3.0, 4.0]));
    }
}
