//! Desktop GUI: browse the index, add and remove documents, inspect chunks.
//!
//! Commands open the index from disk per request instead of caching it. The
//! index is small enough that reopening it is imperceptible, and doing so means
//! adding or removing a document cannot leave this window showing chunks that no
//! longer exist — a stale browser of a knowledge base is worse than a slightly
//! slower one. The embedding and reranking models are the expensive part, so
//! those are cached.

use std::collections::{HashMap, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Context;
use corpus_core::{
    Chunk, DEFAULT_K, DocumentRecord, Encoder, Index, IngestReport, Paths, Reranker, Stage,
    ingest_files,
    remove_document as core_remove_document,
    search,
};
use corpus_core::chat::{self, ChatEvent, ChatMeta, ChatSession, ChatTurn};
use corpus_core::figures;
use corpus_core::render;
use corpus_core::vision::{self, VisionConfig, VisionSettings};
use serde::Serialize;
use tauri::State;
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::{DialogExt, FilePath};

/// Cached model sessions. Locked in this order (encoder, then reranker) so the
/// two cannot deadlock against each other.
struct Models {
    encoder: Mutex<Option<Encoder>>,
    reranker: Mutex<Option<Reranker>>,
    reranker_failed: Mutex<bool>,
}

pub struct AppState {
    paths: Arc<Paths>,
    models: Models,
    /// File lists handed to the frontend by the native picker, keyed by token,
    /// so ingest is driven by a token rather than by paths the frontend holds.
    pending: Mutex<HashMap<String, Vec<PathBuf>>>,
    /// Monotonic counter for new chat ids, so two chats in the same millisecond
    /// cannot collide.
    chat_seq: Mutex<u64>,
    /// Set by `cancel_chat`: the chat worker in flight drops its answer at the
    /// next checkpoint instead of revealing and saving it.
    chat_cancel: AtomicBool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexStatus {
    chunks: usize,
    documents: usize,
    model: String,
    dimension: usize,
    chunker_version: String,
    index_path: String,
    models_path: String,
    index_present: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentRow {
    filename: String,
    path: String,
    chunks: usize,
    pages: usize,
    /// Captions among this document's chunks: drawings the index can see.
    figures: usize,
    /// False when the original file is no longer where it was indexed from.
    source_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChunkRow {
    id: String,
    page: i32,
    words: usize,
    text: String,
    /// A vision model's description of a figure rather than printed text.
    figure: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowsePage {
    document: String,
    total: usize,
    offset: usize,
    items: Vec<ChunkRow>,
    documents: Vec<DocumentRow>,
    status: IndexStatus,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    stage: &'static str,
    done: usize,
    total: usize,
    current: String,
    /// Only counting figures knows which of its two passes it is in; text
    /// extraction cannot report a position, so the window says so instead of
    /// showing a count that never moves.
    phase: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestSummary {
    indexed: Vec<String>,
    unchanged: Vec<String>,
    new_chunks: usize,
    total_chunks: usize,
    total_documents: usize,
    cancelled: bool,
    /// Figure pages found, captions made, and captions that failed. A run with
    /// figures and no captions means text-only indexing, and the window should
    /// say so rather than letting it look complete.
    figures: usize,
    captions_made: usize,
    caption_failures: usize,
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Extracting => "extracting",
        Stage::Captioning => "captioning",
        Stage::Embedding => "embedding",
        Stage::Saving => "saving",
        Stage::Done => "done",
    }
}

fn status(index: Option<&Index>, paths: &Paths) -> IndexStatus {
    IndexStatus {
        chunks: index.map(|index| index.metadata.chunks).unwrap_or(0),
        documents: index.map(|index| index.metadata.documents).unwrap_or(0),
        model: index
            .map(|index| index.metadata.model.clone())
            .unwrap_or_else(|| corpus_core::config::EMBED_MODEL.to_string()),
        dimension: index.map(|index| index.metadata.dim).unwrap_or(corpus_core::config::EMBED_DIM),
        chunker_version: index
            .map(|index| index.metadata.chunker_version.clone())
            .unwrap_or_else(|| corpus_core::config::CHUNKER_VERSION.to_string()),
        index_path: paths.index.display().to_string(),
        models_path: paths.models.display().to_string(),
        index_present: index.is_some(),
    }
}

/// Document rows combine the chunk counts in the index with recorded source
/// paths, and report whether each original file still exists on disk.
fn document_rows(index: &Index, paths: &Paths) -> Vec<DocumentRow> {
    let records = Index::read_documents(&paths.index).unwrap_or_default();
    let mut captioned: HashMap<String, usize> = HashMap::new();
    for chunk in &index.chunks {
        if chunk.figure {
            *captioned.entry(chunk.filename.clone()).or_default() += 1;
        }
    }
    let mut rows: Vec<DocumentRow> = index
        .document_stats()
        .into_iter()
        .map(|(filename, (chunks, pages))| {
            let path = records
                .get(&filename)
                .map(|record: &DocumentRecord| record.path.clone())
                .unwrap_or_default();
            let source_available = !path.is_empty() && PathBuf::from(&path).exists();
            let figures = captioned.get(&filename).copied().unwrap_or(0);
            DocumentRow {
                filename,
                path,
                chunks,
                pages,
                figures,
                source_available,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.chunks.cmp(&a.chunks).then(a.filename.cmp(&b.filename)));
    rows
}

/// Guard over a cached model slot. std's mapped lock guards are unstable, so
/// this derefs to the model instead of remapping the guard.
struct ModelGuard<'a, M> {
    guard: MutexGuard<'a, Option<M>>,
}

impl<M> std::ops::Deref for ModelGuard<'_, M> {
    type Target = M;
    fn deref(&self) -> &M {
        self.guard.as_ref().expect("model slot filled before deref")
    }
}

impl<M> std::ops::DerefMut for ModelGuard<'_, M> {
    fn deref_mut(&mut self) -> &mut M {
        self.guard.as_mut().expect("model slot filled before deref")
    }
}

/// Loads the embedding model on first use and keeps it for the session.
fn encoder(state: &AppState) -> Result<ModelGuard<'_, Encoder>, String> {
    let mut guard = state
        .models
        .encoder
        .lock()
        .map_err(|_| "embedding model lock poisoned".to_string())?;
    if guard.is_none() {
        *guard = Some(Encoder::load(&state.paths).map_err(|error| format!("{error:#}"))?);
    }
    Ok(ModelGuard { guard })
}

/// Loads the reranker on first use and remembers failure, so a missing or
/// half-downloaded reranker degrades ranking instead of breaking the query.
fn reranker(state: &AppState) -> Option<ModelGuard<'_, Reranker>> {
    let mut failed = state.models.reranker_failed.lock().ok()?;
    if *failed {
        return None;
    }
    let mut guard = state.models.reranker.lock().ok()?;
    if guard.is_none() {
        match Reranker::load(&state.paths) {
            Ok(loaded) => *guard = Some(loaded),
            Err(error) => {
                eprintln!("reranker unavailable, answering without re-ranking: {error:#}");
                *failed = true;
                return None;
            }
        }
    }
    Some(ModelGuard { guard })
}

/// Real file paths from the dialog, dropping URI results the app cannot index.
fn local_paths(paths: Vec<FilePath>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter_map(|path| match path {
            FilePath::Path(path) => Some(path),
            FilePath::Url(_) => None,
        })
        .collect()
}

#[tauri::command]
async fn index_status(state: State<'_, Arc<AppState>>) -> Result<IndexStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let paths = Arc::clone(&state.paths);
        let index = Index::open(&paths.index).ok();
        Ok(status(index.as_ref(), &paths))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// One page of a document's chunks plus the full document list. Called with no
/// document to fetch just the list and status.
#[tauri::command]
async fn browse(
    state: State<'_, Arc<AppState>>,
    document: Option<String>,
    filter: Option<String>,
    page: Option<i32>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<BrowsePage, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let paths = Arc::clone(&state.paths);
        let index = match Index::open(&paths.index) {
            Ok(index) => index,
            Err(_) => {
                return Ok(BrowsePage {
                    document: document.unwrap_or_default(),
                    total: 0,
                    offset: 0,
                    items: Vec::new(),
                    documents: Vec::new(),
                    status: status(None, &paths),
                });
            }
        };

        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(40).clamp(1, 200);
        let (total, items) = match &document {
            Some(filename) => {
                index.page_of_document(filename, filter.as_deref(), page, offset, limit)
            }
            None => (0, Vec::new()),
        };

        Ok(BrowsePage {
            document: document.unwrap_or_default(),
            total,
            offset,
            items: items
                .iter()
                .map(|chunk: &Chunk| ChunkRow {
                    id: chunk.id.clone(),
                    page: chunk.page,
                    words: chunk.text.split_whitespace().count(),
                    text: chunk.text.clone(),
                    figure: chunk.figure,
                })
                .collect(),
            documents: document_rows(&index, &paths),
            status: status(Some(&index), &paths),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn remove_document(
    state: State<'_, Arc<AppState>>,
    filename: String,
) -> Result<usize, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        core_remove_document(&state.paths, &filename).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Opens the original file with the OS default handler. Only paths already
/// recorded in the index are openable, so the webview cannot ask for arbitrary
/// files on the machine.
#[tauri::command]
async fn open_source(
    state: State<'_, Arc<AppState>>,
    filename: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let records = Index::read_documents(&state.paths.index).map_err(|error| error.to_string())?;
        let record = records
            .get(&filename)
            .ok_or_else(|| format!("no indexed document named {filename}"))?;
        if !PathBuf::from(&record.path).exists() {
            return Err(format!("{} no longer exists", record.path));
        }

        let (program, args) = if cfg!(target_os = "macos") {
            ("open", vec![record.path.clone()])
        } else if cfg!(target_os = "windows") {
            ("cmd", vec!["/C".into(), "start".into(), record.path.clone()])
        } else {
            ("xdg-open", vec![record.path.clone()])
        };
        Command::new(program)
            .args(args)
            .spawn()
            .map_err(|error| format!("could not open the source document: {error}"))?;
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Embeds and stores a chosen file list, reporting progress as events. Runs on
/// a blocking thread; the caller wraps it. Shared by the picker and the folder
/// path so both land in the same code path.
fn ingest_choice(
    app: &AppHandle,
    state: &Arc<AppState>,
    files: Vec<PathBuf>,
    vision: Option<VisionConfig>,
) -> Result<IngestSummary, String> {
    let paths = Arc::clone(&state.paths);
    // Loading the embedder is half a gigabyte of ONNX and happens before the
    // first ingest event, so without this the window looks idle for a minute.
    let _ = app.emit(
        "ingest-progress",
        ProgressEvent {
            stage: "preparing",
            done: 0,
            total: 0,
            current: "embedding model".to_string(),
            phase: None,
        },
    );
    let mut encoder = encoder(state)?;
    let report: IngestReport =
        ingest_files(&paths, &files, &mut encoder, vision.as_ref(), &mut |progress| {
        let _ = app.emit(
            "ingest-progress",
            ProgressEvent {
                stage: stage_name(progress.stage),
                done: progress.done,
                total: progress.total,
                current: progress.current.clone(),
                phase: None,
            },
        );
    })
    .map_err(|error| format!("{error:#}"))?;

    Ok(IngestSummary {
        indexed: report.indexed_files,
        unchanged: report.skipped_files,
        new_chunks: report.new_chunks,
        total_chunks: report.total_chunks,
        total_documents: report.total_documents,
        cancelled: false,
        figures: report.figures_found,
        captions_made: report.captions_made,
        caption_failures: report.caption_failures,
    })
}

/// How this install is configured for figure captioning. `pdfium` is false when
/// the runtime library is missing, which makes captioning impossible rather
/// than merely slow.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VisionStatus {
    enabled: bool,
    base_url: String,
    model: String,
    api_key_set: bool,
    pdfium: bool,
}

fn vision_status(state: &AppState) -> Result<VisionStatus, String> {
    let settings = VisionSettings::load(&state.paths).map_err(|error| format!("{error:#}"))?;
    Ok(VisionStatus {
        enabled: settings.enabled,
        base_url: settings.base_url.clone(),
        model: settings.model.clone(),
        api_key_set: !settings.api_key.is_empty(),
        pdfium: render::library_present(&state.paths),
    })
}

/// Settings are saved field by field so a form that does not know the API key
/// cannot blank it by submitting an empty box.
#[tauri::command]
async fn vision_settings(state: State<'_, Arc<AppState>>) -> Result<VisionStatus, String> {
    vision_status(&state)
}

#[tauri::command]
async fn save_vision_settings(
    state: State<'_, Arc<AppState>>,
    enabled: Option<bool>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
) -> Result<VisionStatus, String> {
    let mut settings =
        VisionSettings::load(&state.paths).map_err(|error| format!("{error:#}"))?;
    if let Some(enabled) = enabled {
        settings.enabled = enabled;
    }
    if let Some(base_url) = base_url.filter(|value| !value.trim().is_empty()) {
        settings.base_url = base_url.trim().to_string();
    }
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        settings.model = model.trim().to_string();
    }
    // An empty key means "leave it alone"; clearing one is not offered.
    if let Some(api_key) = api_key.filter(|value| !value.trim().is_empty()) {
        settings.api_key = api_key.trim().to_string();
    }
    settings
        .save(&state.paths)
        .map_err(|error| format!("{error:#}"))?;
    vision_status(&state)
}

/// Prove the endpoint can see an image. Advertised capabilities have already
/// proved unreliable, so this sends a real image and returns what the model said.
#[tauri::command]
async fn test_vision(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let settings = VisionSettings::load(&state.paths).map_err(|error| format!("{error:#}"))?;
    let config = settings.config();
    tauri::async_runtime::spawn_blocking(move || vision::probe_endpoint(&config))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

/// Chat over the indexed corpus, answered by the same local model that captions
/// figures. Retrieval runs fresh each turn; the conversation so far travels
/// with the question as context. The answer streams as `chat-event` events
/// (token / done / error) so the frontend can reveal it; when `session_id` is
/// set the turn is also persisted to that chat.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn chat_completion(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: Option<String>,
    question: String,
    history: Option<Vec<ChatTurn>>,
    k: Option<usize>,
    // The ticked grounding set: Some(list) retrieves only from those
    // documents, Some(empty) grounds against nothing, None is the whole index.
    documents: Option<Vec<String>>,
    regenerate: Option<bool>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("ask a question first".to_string());
    }
    let history = history.unwrap_or_default();
    let k = k.unwrap_or(DEFAULT_K);
    let session_id = session_id.unwrap_or_default();
    let regenerate = regenerate.unwrap_or(false);
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.chat_cancel.store(false, Ordering::Relaxed);
        let settings = VisionSettings::load(&state.paths).map_err(|error| format!("{error:#}"))?;
        // Chatting is the user's explicit act, so it runs whether or not the
        // captioning toggle is on; only an unreadable endpoint is refused.
        vision::ensure_local_endpoint(&settings.base_url)
            .map_err(|error| format!("{error:#}"))?;

        let index = Index::open(&state.paths.index).map_err(|_| "nothing is indexed yet".to_string())?;
        let mut encoder = encoder(&state)?;
        let mut reranker = reranker(&state);
        let results = search(
            &index,
            &mut encoder,
            reranker.as_deref_mut(),
            &question,
            k,
            documents.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        drop(encoder);
        drop(reranker);

        // The model call itself is one blocking request, so Stop lands at the
        // checkpoints around it: the reply is dropped rather than displayed.
        if state.chat_cancel.load(Ordering::Relaxed) {
            let _ = app.emit("chat-event", ChatEvent::Cancelled);
            return Ok(());
        }

        let stream = match chat::answer(&settings.config(), &question, &history, &results, regenerate) {
            Ok(stream) => stream,
            Err(error) => {
                let _ = app.emit("chat-event", ChatEvent::Error { message: format!("{error:#}") });
                return Ok(());
            }
        };

        let mut saved_answer: Option<String> = None;
        let mut saved_sources: Option<serde_json::Value> = None;
        for event in chat::collect_stream(stream) {
            if state.chat_cancel.load(Ordering::Relaxed) {
                let _ = app.emit("chat-event", ChatEvent::Cancelled);
                return Ok(());
            }
            if let ChatEvent::Done { value } = &event {
                saved_answer = value["answer"].as_str().map(|s| s.to_string());
                saved_sources = Some(value["sources"].clone());
            }
            let _ = app.emit("chat-event", event);
        }

        if !session_id.is_empty() {
            let mut session = ChatSession::load(&state.paths, &session_id)
                .map_err(|error| format!("{error:#}"))?;
            session.push_question(&question);
            if let (Some(answer), Some(sources)) = (saved_answer, saved_sources) {
                session.push_answer(&answer, sources);
            }
            session.save(&state.paths).map_err(|error| format!("{error:#}"))?;
        }
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Stop the chat answer in flight. The worker notices between its stages and
/// between streamed events, and a stopped exchange is never written to the
/// chat file.
#[tauri::command]
fn cancel_chat(state: State<'_, Arc<AppState>>) {
    state.chat_cancel.store(true, Ordering::Relaxed);
}

/// The saved chats, newest first, for the session picker.
#[tauri::command]
async fn list_chat_sessions(state: State<'_, Arc<AppState>>) -> Result<Vec<ChatMeta>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ChatSession::list(&state.paths).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// A fresh chat, saved immediately so it survives a crash before the first turn.
#[tauri::command]
async fn new_chat_session(
    state: State<'_, Arc<AppState>>,
    model: Option<String>,
) -> Result<ChatSession, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let settings = VisionSettings::load(&state.paths).map_err(|error| format!("{error:#}"))?;
        let model = model.unwrap_or_else(|| settings.model.clone());
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut seq = state.chat_seq.lock().map_err(|_| "chat counter poisoned".to_string())?;
        *seq += 1;
        let session = ChatSession::new(format!("{millis}-{seq}"), &model);
        session.save(&state.paths).map_err(|error| format!("{error:#}"))?;
        Ok(session)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// A saved chat, for the frontend to render its transcript.
#[tauri::command]
async fn load_chat_session(state: State<'_, Arc<AppState>>, id: String) -> Result<ChatSession, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ChatSession::load(&state.paths, &id).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Rename a chat in the picker.
#[tauri::command]
async fn rename_chat_session(
    state: State<'_, Arc<AppState>>,
    id: String,
    title: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = ChatSession::load(&state.paths, &id)
            .map_err(|error| format!("{error:#}"))?;
        session.title = title;
        session.save(&state.paths).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Delete a chat from disk. It lands in the chats/trash folder rather than
/// vanishing, so the GUI's Undo toast can offer restore even after a reload.
#[tauri::command]
async fn delete_chat_session(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ChatSession::trash(&state.paths, &id).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Put a trashed chat back after an Undo.
#[tauri::command]
async fn restore_chat_session(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<ChatSession, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ChatSession::restore(&state.paths, &id).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// What picking a set of documents would cost in captioning, before any of it
/// happens. Counting is a lopdf pass over page resources: no PDFium, no model.
/// It takes seconds on a whole corpus, so each document is reported while it
/// runs rather than in one silence that ends at the cost question.
fn figure_estimate(app: &AppHandle, files: &[PathBuf]) -> (usize, usize) {
    let pdfs: Vec<&PathBuf> = files
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")))
        .collect();
    let mut figures = 0;
    let mut pages = 0;

    for (index, path) in pdfs.iter().enumerate() {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = app.emit(
            "ingest-progress",
            ProgressEvent {
                stage: "scanning",
                done: index + 1,
                total: pdfs.len(),
                current: name.clone(),
                phase: Some("document"),
            },
        );

        match figures::scan_pdf_with(path, &mut |progress| {
            let _ = app.emit(
                "ingest-progress",
                ProgressEvent {
                    stage: "scanning",
                    done: progress.page,
                    total: progress.pages,
                    current: name.clone(),
                    phase: Some(if progress.text_phase { "text" } else { "pages" }),
                },
            );
        }) {
            Ok(scan) => {
                figures += scan.figure_pages.len();
                pages += scan.pages;
            }
            // An unreadable document is reported by the ingest itself; the
            // estimate only has to say what it could not see.
            Err(_) => continue,
        }
    }
    (figures, pages)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingIngest {
    /// Hand this back to `commit_ingest`; the paths themselves stay here.
    token: String,
    documents: usize,
    pages: usize,
    figures: usize,
    /// Captioning goes through this model, so the question names it.
    model: String,
    /// Rough wall-clock for captioning, from the measured 4–15 s per figure.
    caption_minutes_low: f64,
    caption_minutes_high: f64,
    /// Captioning cannot run at all without the PDFium runtime library.
    pdfium: bool,
    /// What the user last chose, so the dialog opens on their standing preference.
    caption_default: bool,
}

fn pending_token(files: &[PathBuf]) -> String {
    let mut hasher = DefaultHasher::new();
    for file in files {
        file.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

/// Run the ingest the user just confirmed. Taking a token rather than paths
/// means a compromised webview cannot index files the user never picked.
#[tauri::command]
async fn commit_ingest(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    token: String,
    caption: bool,
) -> Result<IngestSummary, String> {
    let files = state
        .pending
        .lock()
        .map_err(|_| "ingest state was poisoned".to_string())?
        .remove(&token)
        .ok_or_else(|| "that selection has expired; choose the documents again".to_string())?;
    let state = state.inner().clone();
    run_ingest(&app, &state, files, Some(caption)).await
}

/// Whether this run should caption, given what the user chose in the dialog and
/// what they configured: an explicit answer always beats the stored default.
fn vision_for(
    paths: &Paths,
    caption: Option<bool>,
) -> Result<Option<VisionConfig>, String> {
    let settings = VisionSettings::load(paths).map_err(|error| format!("{error:#}"))?;
    let wanted = caption.unwrap_or(settings.enabled);
    if !wanted {
        return Ok(None);
    }
    if !render::library_present(paths) {
        return Err(
            "captioning figures needs the PDFium runtime library in the data dir \
             (or set RAG_PDFIUM_PATH); documents can still be indexed as text"
                .to_string(),
        );
    }
    Ok(Some(settings.config()))
}

/// Either an immediate summary, or the cost of captioning so the window can
/// ask before any of it happens.
#[derive(Serialize)]
#[serde(untagged)]
enum AddOutcome {
    Pending(PendingIngest),
    Done(IngestSummary),
}

fn idle_summary(state: &AppState) -> IngestSummary {
    let index = Index::open(&state.paths.index).ok();
    IngestSummary {
        indexed: Vec::new(),
        unchanged: Vec::new(),
        new_chunks: 0,
        total_chunks: index.as_ref().map(|index| index.metadata.chunks).unwrap_or(0),
        total_documents: index.as_ref().map(|index| index.metadata.documents).unwrap_or(0),
        cancelled: true,
        figures: 0,
        captions_made: 0,
        caption_failures: 0,
    }
}

async fn run_ingest(
    app: &AppHandle,
    state: &Arc<AppState>,
    files: Vec<PathBuf>,
    caption: Option<bool>,
) -> Result<IngestSummary, String> {
    let vision = vision_for(&state.paths, caption)?;
    let app = app.clone();
    let state = state.clone();
    tauri::async_runtime::spawn_blocking(move || ingest_choice(&app, &state, files, vision))
        .await
        .map_err(|error| error.to_string())?
}

/// Native picker for files or a folder. Progress arrives as `ingest-progress`
/// events because a corpus ingest runs for minutes.
#[tauri::command]
async fn add_documents(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    scope: Option<String>,
) -> Result<AddOutcome, String> {
    let folder = scope.as_deref() == Some("folder");
    let dialog = app.dialog().file();
    let picked = if folder {
        dialog
            .set_title("Choose a folder of documents to index")
            .blocking_pick_folders()
            .map(|folders| {
                local_paths(folders)
                    .into_iter()
                    .flat_map(|folder| corpus_core::ingest::document_files(&folder).unwrap_or_default())
                    .collect()
            })
    } else {
        dialog
            .set_title("Choose documents to index")
            .add_filter("Documents", &["pdf", "md", "markdown", "txt"])
            .blocking_pick_files()
            .map(local_paths)
    };

    let state = state.inner().clone();
    let Some(files) = picked.filter(|files| !files.is_empty()) else {
        return Ok(AddOutcome::Done(idle_summary(&state)));
    };

    let (figure_count, page_count) = figure_estimate(&app, &files);
    // A document with no drawings has nothing to decide. Thirty of them cost
    // half an hour of GPU time, which is the user's call, not the app's.
    if figure_count == 0 {
        return Ok(AddOutcome::Done(run_ingest(&app, &state, files, Some(false)).await?));
    }

    let token = pending_token(&files);
    let document_count = files.len();
    state
        .pending
        .lock()
        .map_err(|_| "ingest state was poisoned".to_string())?
        .insert(token.clone(), files);
    let settings = VisionSettings::load(&state.paths).map_err(|error| format!("{error:#}"))?;
    Ok(AddOutcome::Pending(PendingIngest {
        token,
        documents: document_count,
        pages: page_count,
        figures: figure_count,
        model: settings.model.clone(),
        caption_minutes_low: round_minutes(figure_count as f64 * 4.0 / 60.0),
        caption_minutes_high: round_minutes(figure_count as f64 * 15.0 / 60.0),
        pdfium: render::library_present(&state.paths),
        caption_default: settings.enabled,
    }))
}

fn round_minutes(minutes: f64) -> f64 {
    (minutes * 10.0).round() / 10.0
}

/// Indexes every document under a folder path, for callers that already know the
/// path (drag-and-drop or a path field) instead of using the native picker.
#[tauri::command]
async fn index_folder(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    folder: String,
    caption: Option<bool>,
) -> Result<IngestSummary, String> {
    let files = corpus_core::ingest::document_files(&PathBuf::from(folder))
        .map_err(|error| error.to_string())?;
    if files.is_empty() {
        return Err("that folder has no PDF or text documents".to_string());
    }
    let state = state.inner().clone();
    run_ingest(&app, &state, files, caption).await
}

/// The MCP server lives inside the shared data folder, so uninstalling is
/// deleting the app and that one folder — nothing else to hunt down.
fn mcp_bin_dir(paths: &Paths) -> PathBuf {
    paths.data.join("bin")
}

/// Copy the MCP server binary into the data folder. Clients point at this path,
/// which survives app moves and updates. A copy that is already as fresh as its
/// source is left alone, so launches stay cheap.
fn place_mcp_binary(source: &Path, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    let name = source
        .file_name()
        .with_context(|| format!("{} has no file name", source.display()))?;
    let dest = dest_dir.join(name);
    let modified = |path: &Path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .with_context(|| format!("{} is not readable", path.display()))
    };
    if let (Ok(dest_time), Ok(source_time)) = (modified(&dest), modified(source)) {
        if dest_time >= source_time {
            return Ok(dest);
        }
    }
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("could not create {}", dest_dir.display()))?;
    fs::copy(source, &dest)
        .with_context(|| format!("could not copy {} to {}", source.display(), dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("could not make {} executable", dest.display()))?;
    }
    Ok(dest)
}

/// Where the MCP server binary ships with this GUI build: inside the bundle as
/// a resource, or beside the raw binary in dev and zip layouts.
fn locate_mcp_source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let name = if cfg!(target_os = "windows") {
        "corpus-mcp.exe"
    } else {
        "corpus-mcp"
    };
    let mut dirs = Vec::new();
    if let Some(contents) = exe_dir.parent() {
        // Bundled: Corpus.app/Contents/MacOS/… → Contents/Resources
        dirs.push(contents.join("Resources").join(name));
        dirs.push(contents.join("Resources/resources").join(name));
    }
    dirs.push(exe_dir.join(name));
    dirs.into_iter().find(|path| path.exists())
}

fn ensure_mcp_server(paths: &Paths) -> anyhow::Result<PathBuf> {
    let source = locate_mcp_source().ok_or_else(|| {
        anyhow::anyhow!(
            "corpus-mcp is not shipped in this build — run scripts/install.sh, or \
             cargo build --release --workspace"
        )
    })?;
    place_mcp_binary(&source, &mcp_bin_dir(paths))
}

/// One JSON block, ready to paste, with this install's real server path.
fn mcp_config_json(command: &Path) -> String {
    let payload = serde_json::json!({
        "mcpServers": {
            "corpus": {
                "transport": "stdio",
                "command": command.display().to_string(),
                "args": [],
                "enabled": true,
                "timeout": 180
            }
        }
    });
    serde_json::to_string_pretty(&payload).expect("the mcpServers payload always serialises")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpStatus {
    command: String,
    json: String,
}

/// The GUI keeps the MCP server inside the data folder; this sheet hands the
/// client config out with that real path, so nothing is fiddled by hand.
#[tauri::command]
async fn mcp_config(state: State<'_, Arc<AppState>>) -> Result<McpStatus, String> {
    let dest = ensure_mcp_server(&state.paths).map_err(|error| format!("{error:#}"))?;
    Ok(McpStatus {
        command: dest.display().to_string(),
        json: mcp_config_json(&dest),
    })
}

pub fn run() {
    let paths = Arc::new(Paths::resolve().expect("no writable data directory"));
    if let Err(error) = ensure_mcp_server(&paths) {
        eprintln!(
            "corpus-mcp could not be placed in {}: {error:#}",
            mcp_bin_dir(&paths).display()
        );
    }
    let state = Arc::new(AppState {
        paths: Arc::clone(&paths),
        models: Models {
            encoder: Mutex::new(None),
            reranker: Mutex::new(None),
            reranker_failed: Mutex::new(false),
        },
        pending: Mutex::new(HashMap::new()),
        chat_seq: Mutex::new(0),
        chat_cancel: AtomicBool::new(false),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            index_status,
            browse,
            remove_document,
            open_source,
            add_documents,
            index_folder,
            vision_settings,
            save_vision_settings,
            test_vision,
            mcp_config,
            commit_ingest,
            chat_completion,
            cancel_chat,
            list_chat_sessions,
            new_chat_session,
            load_chat_session,
            rename_chat_session,
            delete_chat_session,
            restore_chat_session,
        ])
        .setup(|app| {
            app.handle().emit("app-ready", ())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the application");
}

fn main() {
    run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(value: &impl Serialize) -> Vec<String> {
        serde_json::to_value(value)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    fn has(keys: &[String], wanted: &str) -> bool {
        keys.iter().any(|key| key == wanted)
    }

    /// The webview reads these payloads by name, and its tests mock them, so a
    /// renamed field would leave a panel quietly empty while every test stayed
    /// green. This is the boundary check for that.
    #[test]
    fn payloads_carry_the_field_names_the_frontend_reads() {
        for (name, value, wanted) in [
            (
                "VisionStatus",
                keys(&VisionStatus {
                    enabled: true,
                    base_url: "http://127.0.0.1:11234/v1".to_string(),
                    model: "m".to_string(),
                    api_key_set: true,
                    pdfium: true,
                }),
                &["enabled", "baseUrl", "model", "apiKeySet", "pdfium"][..],
            ),
            (
                "IngestSummary",
                keys(&IngestSummary {
                    indexed: vec!["a.pdf".to_string()],
                    unchanged: vec![],
                    new_chunks: 1,
                    total_chunks: 2,
                    total_documents: 3,
                    cancelled: false,
                    figures: 4,
                    captions_made: 5,
                    caption_failures: 6,
                }),
                &[
                    "indexed",
                    "newChunks",
                    "totalChunks",
                    "totalDocuments",
                    "cancelled",
                    "figures",
                    "captionsMade",
                    "captionFailures",
                ][..],
            ),
            (
                "PendingIngest",
                keys(&PendingIngest {
                    token: "t".to_string(),
                    documents: 1,
                    pages: 2,
                    figures: 3,
                    model: "m".to_string(),
                    caption_minutes_low: 1.0,
                    caption_minutes_high: 2.0,
                    pdfium: true,
                    caption_default: false,
                }),
                &[
                    "token",
                    "documents",
                    "pages",
                    "figures",
                    "model",
                    "captionMinutesLow",
                    "captionMinutesHigh",
                    "pdfium",
                    "captionDefault",
                ][..],
            ),
            (
                "ProgressEvent",
                keys(&ProgressEvent {
                    stage: "scanning",
                    done: 16,
                    total: 958,
                    current: "a.pdf".to_string(),
                    phase: Some("pages"),
                }),
                &["stage", "done", "total", "current", "phase"][..],
            ),
        ] {
            for field in wanted {
                assert!(has(&value, field), "{name} is missing the field {field}");
            }
        }
    }

    /// The window tells a pending selection from a finished ingest by looking
    /// for `token`, so only the pending shape may carry it.
    #[test]
    fn the_mcp_server_lands_executable_in_the_data_dir() {
        let root = std::env::temp_dir().join(format!("corpus-mcp-test-{}", std::process::id()));
        let source_dir = root.join("source");
        std::fs::create_dir_all(&source_dir).expect("could not create the test source dir");
        let source = source_dir.join("corpus-mcp");
        std::fs::write(&source, "binary bytes").expect("could not write the test source");
        let dest_dir = root.join("data/bin");

        let dest = place_mcp_binary(&source, &dest_dir).expect("the copy should succeed");
        assert_eq!(dest.file_name().expect("a file name"), "corpus-mcp");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                std::fs::metadata(&dest)
                    .expect("the copy should exist")
                    .permissions()
                    .mode()
                    & 0o111
                    != 0,
                "the copied server should be executable",
            );
        }

        // A copy that is already as fresh as its source is left alone, so a
        // launch never rewrites it.
        let before = std::fs::metadata(&dest)
            .expect("the copy should exist")
            .modified()
            .expect("mtime should read");
        place_mcp_binary(&source, &dest_dir).expect("the second pass should succeed");
        let after = std::fs::metadata(&dest)
            .expect("the copy should exist")
            .modified()
            .expect("mtime should read");
        assert_eq!(before, after, "an unchanged source should not rewrite the copy");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_mcp_config_json_carries_the_installed_path() {
        let json = mcp_config_json(Path::new("/tmp/data/bin/corpus-mcp"));
        assert!(
            json.contains("\"command\": \"/tmp/data/bin/corpus-mcp\""),
            "the real path must appear as the command: {json}"
        );
        assert!(json.contains("\"enabled\": true"), "pi-style schema: {json}");
        assert!(json.contains("\"timeout\": 180"), "pi-style schema: {json}");
        assert!(json.contains("\"transport\": \"stdio\""), "pi-style schema: {json}");
    }

    #[test]
    fn only_a_pending_selection_carries_a_token() {
        let pending = serde_json::to_value(AddOutcome::Pending(PendingIngest {
            token: "t".to_string(),
            documents: 1,
            pages: 2,
            figures: 3,
            model: "m".to_string(),
            caption_minutes_low: 1.0,
            caption_minutes_high: 2.0,
            pdfium: true,
            caption_default: false,
        }))
        .unwrap();
        assert_eq!(pending.get("token").and_then(|v| v.as_str()), Some("t"));

        let done = serde_json::to_value(AddOutcome::Done(IngestSummary {
            indexed: vec![],
            unchanged: vec![],
            new_chunks: 0,
            total_chunks: 0,
            total_documents: 0,
            cancelled: true,
            figures: 0,
            captions_made: 0,
            caption_failures: 0,
        }))
        .unwrap();
        assert!(
            done.get("token").is_none(),
            "a finished ingest must not look like a pending one",
        );
    }
}
