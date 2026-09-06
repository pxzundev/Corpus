use std::collections::HashMap;

use anyhow::Result;

use crate::config::{DEFAULT_K, HYBRID_CANDIDATES, RRF_K};
use crate::embed::{Encoder, Reranker};
use crate::store::Index;

pub struct SearchResult {
    /// Id of the retrieved chunk, so a citation can land on the exact card
    /// rather than anywhere on its page.
    pub id: String,
    pub score: f32,
    /// "rerank" when a cross-encoder rescored the pool, "rrf" for fused ranks only.
    pub score_kind: &'static str,
    pub filename: String,
    pub page: i32,
    pub text: String,
    /// A vision model's description of a figure, not printed page text.
    pub figure: bool,
}

/// Hybrid retrieval: dense vectors and BM25 each nominate candidates, Reciprocal
/// Rank Fusion merges the two ranked lists by position only (so the two
/// incomparable score scales never have to be calibrated), and the cross-encoder
/// reranks the fused pool. `scope` narrows the candidate pool to a document set
/// before any scoring, so a scoped question still gets a full k of passages;
/// `Some(&[])` grounds against nothing and yields no passages.
pub fn search(
    index: &Index,
    encoder: &mut Encoder,
    reranker: Option<&mut Reranker>,
    query: &str,
    k: usize,
    scope: Option<&[String]>,
) -> Result<Vec<SearchResult>> {
    let limit = HYBRID_CANDIDATES.min(index.chunks.len());
    let mut fused: HashMap<usize, f32> = HashMap::new();

    let query_vector = encoder.encode_one(query)?;
    for (rank, (chunk, _)) in index
        .dense_search(&query_vector, limit, scope)
        .iter()
        .enumerate()
    {
        *fused.entry(*chunk).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
    }
    for (rank, (chunk, _)) in index
        .lexical_search(query, limit, scope)
        .iter()
        .enumerate()
    {
        *fused.entry(*chunk).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
    }

    let mut pool: Vec<(f32, usize)> = fused
        .into_iter()
        .map(|(chunk, score)| (score, chunk))
        .collect();
    pool.sort_by(|a, b| b.0.total_cmp(&a.0));
    if pool.is_empty() {
        return Ok(Vec::new());
    }

    let Some(reranker) = reranker else {
        return Ok(collect(index, &pool[..k.min(pool.len())], "rrf"));
    };

    let documents: Vec<&str> = pool
        .iter()
        .map(|(_, chunk)| index.chunks[*chunk].text.as_str())
        .collect();
    let scores = reranker.score(query, &documents)?;

    let mut reranked: Vec<(f32, usize)> = scores
        .into_iter()
        .zip(pool.iter().map(|(_, chunk)| *chunk))
        .collect();
    reranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(collect(index, &reranked[..k.min(reranked.len())], "rerank"))
}

fn collect(index: &Index, scored: &[(f32, usize)], score_kind: &'static str) -> Vec<SearchResult> {
    scored
        .iter()
        .map(|(score, chunk)| {
            let chunk = &index.chunks[*chunk];
            SearchResult {
                id: chunk.id.clone(),
                score: *score,
                score_kind,
                filename: chunk.filename.clone(),
                page: chunk.page,
                text: chunk.text.clone(),
                figure: chunk.figure,
            }
        })
        .collect()
}

/// Citation format is deliberately unchanged from the Python server so clients
/// that already parse it keep working.
pub fn format_results(results: &[SearchResult]) -> String {
    format_results_with(results, false)
}

/// Chat prompt variant: each passage opens with the number the model is asked
/// to cite, so a [2] in an answer resolves to results[1] by construction.
/// Without the number the model invents its own scheme and the citation
/// click lands on an unrelated passage.
pub fn format_results_numbered(results: &[SearchResult]) -> String {
    format_results_with(results, true)
}

fn format_results_with(results: &[SearchResult], numbered: bool) -> String {
    if results.is_empty() {
        return "No relevant passages found in the indexed documents.".to_string();
    }
    results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let citation = if result.page == -1 {
                result.filename.clone()
            } else {
                format!("{} p.{}", result.filename, result.page)
            };
            // A caption is a model's reading of a drawing, so it must never
            // present itself the same way as printed body text.
            let citation = if result.figure {
                format!("{citation} · figure")
            } else {
                citation
            };
            let header = format!("[{citation} | {} {:.2}]", result.score_kind, result.score);
            let header = if numbered {
                format!("[{}] {header}", index + 1)
            } else {
                header
            };
            format!("{header}\n{}", result.text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

pub fn format_documents(index: &Index) -> String {
    let stats = index.document_stats();
    if stats.is_empty() {
        return "No documents indexed. Run `rag index <documents dir>` first.".to_string();
    }
    let mut lines = vec![format!("{} document(s) indexed:\n", stats.len())];
    for (filename, (chunks, pages)) in stats {
        let page_info = if pages == 0 {
            "no pages tracked".to_string()
        } else {
            format!("{pages} pages")
        };
        lines.push(format!("  {filename}  —  {chunks} chunks, {page_info}"));
    }
    lines.push(format!("\n{} chunks total.", index.metadata.chunks));
    lines.join("\n")
}

/// Default number of passages a query returns.
pub const DEFAULT_RESULTS: usize = DEFAULT_K;
