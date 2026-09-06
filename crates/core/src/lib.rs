pub mod bm25;
pub mod caption_cache;
pub mod chat;
pub mod chunk;
pub mod config;
pub mod embed;
pub mod figures;
pub mod render;
pub mod vision;
pub mod ingest;
pub mod search;
pub mod store;

pub use config::{DEFAULT_K, Paths};
pub use embed::{Encoder, Reranker};
pub use ingest::{IngestProgress, IngestReport, Stage, ingest, ingest_files, remove_document};
pub use search::{
    SearchResult, format_documents, format_results, format_results_numbered, search,
};
pub use store::{Chunk, DocumentRecord, Index, IndexMetadata};
