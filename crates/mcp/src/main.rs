//! MCP server over stdio, exposing the same tools the Python server did.
//!
//! Nothing here prints to stdout: stdout is the JSON-RPC channel, so progress
//! and diagnostics go to stderr.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result};
use corpus_core::{
    DEFAULT_K, Encoder, Index, Paths, Reranker, format_documents, format_results, search,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::transport::stdio;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;

/// Everything that needs exclusive access during inference. The ONNX sessions
/// inside the embedding and reranking models are not thread-safe, so queries are
/// serialized through this mutex rather than shared.
struct Runtime {
    index: Index,
    encoder: Encoder,
    reranker: Option<Reranker>,
    /// Set once loading the reranker has failed, so a broken download does not
    /// retry on every query; retrieval continues on the fused lexical+dense legs.
    reranker_unavailable: bool,
    /// mtime of index.json when `index` was loaded. The GUI and CLI write the same
    /// store from other processes, so a request reopens it when that file moves.
    stamp: Option<SystemTime>,
}

impl Runtime {
    /// Index the caller should search, reopened if another process replaced the
    /// store since it was last loaded.
    fn index_fresh(&mut self, paths: &Paths) {
        let path = paths.index.join("index.json");
        let current = fs::metadata(&path).and_then(|m| m.modified()).ok();
        if current == self.stamp {
            return;
        }
        match Index::open(&paths.index) {
            Ok(index) => {
                eprintln!(
                    "reloaded index: {} chunks across {} documents",
                    index.metadata.chunks, index.metadata.documents
                );
                self.stamp = current;
                self.index = index;
            }
            // A half-written or model-mismatched store must not end the session;
            // the copy already in memory is better than nothing.
            Err(error) => eprintln!("index reload skipped, serving the previous copy: {error:#}"),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Natural-language search query.
    pub query: String,
    /// Number of passages to return (default 6).
    #[serde(default)]
    k: Option<usize>,
    /// Optional filename to restrict the search to a single document.
    #[serde(default)]
    filename: Option<String>,
}

pub struct CorpusServer {
    paths: Arc<Paths>,
    runtime: Arc<Mutex<Runtime>>,
}

#[tool_router]
impl CorpusServer {
    /// Hybrid semantic+keyword search over the indexed documents. Returns the
    /// most relevant passages with filename and page citations. Use this
    /// whenever a question should be grounded in the user's documents rather
    /// than answered from general knowledge.
    #[tool(
        name = "search_docs",
        description = "Hybrid semantic+keyword search over the indexed documents. Returns the most relevant passages with filename and page citations. Use this whenever a question should be grounded in the user's documents rather than answered from general knowledge."
    )]
    async fn search_docs(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<String, String> {
        let paths = Arc::clone(&self.paths);
        let runtime = Arc::clone(&self.runtime);
        let k = params.k.unwrap_or(DEFAULT_K);
        let query = params.query.clone();
        let filename = params.filename.clone();

        tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut guard = runtime.lock().map_err(|_| "retrieval lock poisoned".to_string())?;
            guard.index_fresh(&paths);
            // Destructured so the index, encoder, and reranker borrow disjoint
            // fields rather than mutably borrowing the whole runtime.
            let Runtime {
                index,
                encoder,
                reranker,
                reranker_unavailable,
                ..
            } = &mut *guard;

            if !*reranker_unavailable && reranker.is_none() {
                match Reranker::load(&paths) {
                    Ok(loaded) => *reranker = Some(loaded),
                    Err(error) => {
                        eprintln!("reranker unavailable, answering without re-ranking: {error:#}");
                        *reranker_unavailable = true;
                    }
                }
            }

            let scope = filename.as_ref().map(|name| vec![name.clone()]);
            let results = search(
                index,
                encoder,
                reranker.as_mut(),
                &query,
                k,
                scope.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            Ok(format_results(&results))
        })
        .await
        .map_err(|error| format!("retrieval task failed: {error}"))?
    }

    /// List all documents currently in the index with their chunk and page counts.
    /// Use this before search_docs when you need to know which documents are
    /// available or to get the exact filename for the filename filter.
    #[tool(
        name = "list_documents",
        description = "List all documents currently in the index with their chunk and page counts. Use this before search_docs when you need to know which documents are available or to get the exact filename for the filename filter."
    )]
    async fn list_documents(&self) -> Result<String, String> {
        let runtime = Arc::clone(&self.runtime);
        let server_paths = Arc::clone(&self.paths);
        tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut guard = runtime.lock().map_err(|_| "retrieval lock poisoned".to_string())?;
            guard.index_fresh(&server_paths);
            Ok(format_documents(&guard.index))
        })
        .await
        .map_err(|error| format!("list task failed: {error}"))?
    }

}

#[tool_handler(
    name = "corpus",
    instructions = "Local hybrid retrieval over the user's indexed documents. Answers come from the source passages with page citations, not from model memory. Call list_documents first when you need an exact filename for the filename filter."
)]
impl ServerHandler for CorpusServer {}

impl CorpusServer {
    fn new(paths: Arc<Paths>) -> Result<Self> {
        // A missing index is not fatal: the GUI is how most users will create it,
        // and index_fresh picks the store up as soon as it appears.
        let (index, note) = match Index::open(&paths.index) {
            Ok(index) => (index, String::new()),
            Err(error) => {
                let note = format!("{error:#}");
                (Index::empty(), note)
            }
        };
        let encoder = Encoder::load(&paths)
            .with_context(|| format!("index at {} needs its embedding model", paths.index.display()))?;
        if note.is_empty() {
            eprintln!(
                "serving {} chunks across {} documents",
                index.metadata.chunks, index.metadata.documents
            );
        } else {
            eprintln!("{note}; waiting for the index to be built (it reloads itself once it exists)");
        }
        Ok(Self {
            runtime: Arc::new(Mutex::new(Runtime {
                stamp: index_stamp(&paths),
                index,
                encoder,
                reranker: None,
                reranker_unavailable: false,
            })),
            paths,
        })
    }
}

fn index_stamp(paths: &Paths) -> Option<SystemTime> {
    fs::metadata(paths.index.join("index.json"))
        .and_then(|meta| meta.modified())
        .ok()
}

#[tokio::main]
async fn main() -> Result<()> {
    let paths = Arc::new(Paths::resolve()?);
    let server = CorpusServer::new(Arc::clone(&paths))?;
    let service = server
        .serve(stdio())
        .await
        .map_err(|error| anyhow::anyhow!("MCP handshake on stdio failed: {error:?}"))?;
    service
        .waiting()
        .await
        .map_err(|error| anyhow::anyhow!("MCP session ended with an error: {error:?}"))?;
    Ok(())
}
