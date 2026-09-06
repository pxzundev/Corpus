use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};

use crate::caption_cache::CaptionCache;
use crate::chunk::chunk_text;
use crate::config::{EMBED_BATCH, Paths};
use crate::embed::Encoder;
use crate::figures;
use crate::render;
use crate::store::{Chunk, DocumentRecord, Index};
use crate::vision::{self, VisionConfig};

const TEXT_EXTENSIONS: [&str; 3] = ["md", "markdown", "txt"];

/// Rasterisation density for figure pages. 150 dpi keeps small printed
/// gradients legible to a vision model without producing a 5 MB image.
const FIGURE_DPI: u32 = 150;

/// Stop captioning a document after this many failures in a row. A vision
/// engine that is down fails every page the same way, and paying the full
/// timeout for each one would turn a two-minute ingest into an hour.
const MAX_CONSECUTIVE_CAPTION_FAILURES: usize = 3;

#[derive(Debug, Default)]
pub struct IngestReport {
    pub indexed_files: Vec<String>,
    pub skipped_files: Vec<String>,
    pub new_chunks: usize,
    pub total_chunks: usize,
    pub total_documents: usize,
    /// Figure pages found in the PDFs that were actually indexed.
    pub figures_found: usize,
    /// Captions produced by the vision model on this run.
    pub captions_made: usize,
    /// Figures the vision model could not caption. Their page text is still
    /// indexed, so the document is searchable but its drawings are not.
    pub caption_failures: usize,
}

/// Progress of an ingest run. Extraction and embedding are reported separately
/// because their units differ (files versus chunks) and both take long enough
/// that the caller has to show something.
#[derive(Debug, Clone)]
pub struct IngestProgress {
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
    pub current: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Extracting,
    Captioning,
    Embedding,
    Saving,
    Done,
}

/// Indexes a directory tree: every PDF and text file under `source`, reporting
/// progress to stderr.
pub fn ingest(paths: &Paths, source: &Path, encoder: &mut Encoder) -> Result<IngestReport> {
    ingest_files(
        paths,
        &document_files(source)?,
        encoder,
        None,
        &mut |progress| match progress.stage {
            Stage::Extracting => eprintln!(
                "extracting {}/{} files — {}",
                progress.done, progress.total, progress.current
            ),
            Stage::Captioning => eprintln!(
                "captioning {}/{} figures — {}",
                progress.done, progress.total, progress.current
            ),
            Stage::Embedding => {
                eprintln!("embedded {}/{} new chunks", progress.done, progress.total);
            }
            Stage::Saving => eprintln!("saving index"),
            Stage::Done => {}
        },
    )
}

/// Adds new and changed documents to the index. A file whose content hash is
/// already indexed is skipped, so re-running only pays for what actually
/// changed. Accepting an explicit file list is what lets the GUI's file picker
/// add a selection rather than a whole folder.
///
/// With `vision` set, figure pages of each PDF are rasterised and captioned by
/// that model, and each caption is indexed as its own chunk. Without it, only
/// printed text is indexed.
pub fn ingest_files(
    paths: &Paths,
    files: &[PathBuf],
    encoder: &mut Encoder,
    vision: Option<&VisionConfig>,
    on_progress: &mut dyn FnMut(&IngestProgress),
) -> Result<IngestReport> {
    paths.ensure()?;
    let existing = Index::open(&paths.index).unwrap_or_else(|_| Index::empty());
    let known_hashes = existing.files();
    let mut documents = Index::read_documents(&paths.index)?;
    let mut captions = CaptionCache::load(paths)?;

    let mut report = IngestReport::default();
    let mut changed: HashSet<String> = HashSet::new();
    // filename, source path, fhash, page, text, is_figure_caption
    let mut pending: Vec<(String, String, String, i32, String, bool)> = Vec::new();

    for (position, path) in files.iter().enumerate() {
        on_progress(&IngestProgress {
            stage: Stage::Extracting,
            done: position + 1,
            total: files.len(),
            current: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
        let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        let fhash = content_hash(&bytes);

        if known_hashes.get(&filename).is_some_and(|known| *known == fhash) {
            report.skipped_files.push(filename);
            continue;
        }

        let is_pdf = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
        let pages = if is_pdf {
            pdf_pages(path)?
        } else {
            vec![fs::read_to_string(path)
                .with_context(|| format!("could not read {} as UTF-8 text", path.display()))?]
        };

        for (position, page_text) in pages.iter().enumerate() {
            let page = if is_pdf { position as i32 + 1 } else { -1 };
            for text in chunk_text(page_text) {
                pending.push((
                    filename.clone(),
                    path.display().to_string(),
                    fhash.clone(),
                    page,
                    text,
                    false,
                ));
            }
        }

        if let (Some(config), true) = (vision, is_pdf) {
            caption_figures(
                paths,
                path,
                &filename,
                &fhash,
                &pages,
                config,
                &mut captions,
                &mut report,
                &mut pending,
                on_progress,
            )?;
        }

        changed.insert(filename.clone());
        report.indexed_files.push(filename);
    }

    captions.save()?;

    report.new_chunks = pending.len();

    // Chunks and vectors travel together so a dropped file cannot leave an
    // orphan vector behind.
    let mut pairs: Vec<(Chunk, Vec<f32>)> = existing
        .chunks
        .into_iter()
        .zip(existing.vectors)
        .filter(|(chunk, _)| !changed.contains(&chunk.filename))
        .collect();

    let mut sequence: HashMap<(String, i32), u32> = HashMap::new();
    for batch_start in (0..pending.len()).step_by(EMBED_BATCH) {
        let batch = &pending[batch_start..(batch_start + EMBED_BATCH).min(pending.len())];
        let texts: Vec<&str> = batch.iter().map(|(_, _, _, _, text, _)| text.as_str()).collect();
        let embedded = encoder.encode(&texts)?;

        for ((filename, _, fhash, page, text, figure), vector) in batch.iter().zip(embedded) {
            let seq = sequence
                .entry((filename.clone(), *page))
                .and_modify(|n| *n += 1)
                .or_insert(0);
            pairs.push((
                Chunk {
                    id: format!("{fhash}:{page}:{seq}"),
                    text: text.clone(),
                    filename: filename.clone(),
                    page: *page,
                    fhash: fhash.clone(),
                    figure: *figure,
                },
                vector,
            ));
        }
        on_progress(&IngestProgress {
            stage: Stage::Embedding,
            done: (batch_start + batch.len()).min(pending.len()),
            total: pending.len(),
            current: batch
                .last()
                .map(|(filename, ..)| filename.clone())
                .unwrap_or_default(),
        });
    }

    if report.new_chunks == 0 {
        on_progress(&IngestProgress {
            stage: Stage::Done,
            done: 0,
            total: 0,
            current: String::new(),
        });
        report.total_chunks = existing.metadata.chunks;
        report.total_documents = existing.metadata.documents;
        return Ok(report);
    }

    on_progress(&IngestProgress {
        stage: Stage::Saving,
        done: report.new_chunks,
        total: report.new_chunks,
        current: String::new(),
    });

    let (chunks, vectors): (Vec<Chunk>, Vec<Vec<f32>>) = pairs.into_iter().unzip();
    let index = Index::save(&paths.index, chunks, vectors)?;

    // Where each indexed document came from, so the GUI can open the original.
    for (filename, path, fhash) in pending
        .iter()
        .map(|(filename, path, fhash, ..)| (filename, path, fhash))
        .collect::<HashSet<_>>()
    {
        let (chunks, pages) = index
            .document_stats()
            .get(filename)
            .cloned()
            .unwrap_or((0, 0));
        documents.insert(
            filename.clone(),
            DocumentRecord {
                filename: filename.clone(),
                path: path.clone(),
                fhash: fhash.clone(),
                chunks,
                pages,
            },
        );
    }
    Index::write_documents(&paths.index, &documents)?;

    report.total_chunks = index.metadata.chunks;
    report.total_documents = index.metadata.documents;
    Ok(report)
}

/// Drops a document's chunks and vectors from the index. Returns how many
/// chunks were removed, or None if no document had that filename.
/// Rasterise and caption the figure pages of one PDF, queueing each caption as
/// a chunk. A page that cannot be captioned is counted and skipped: its printed
/// text is already indexed, so the document stays searchable even when the
/// vision engine is unavailable.
#[allow(clippy::too_many_arguments)]
fn caption_figures(
    paths: &Paths,
    path: &Path,
    filename: &str,
    fhash: &str,
    pages: &[String],
    config: &VisionConfig,
    captions: &mut CaptionCache,
    report: &mut IngestReport,
    pending: &mut Vec<(String, String, String, i32, String, bool)>,
    on_progress: &mut dyn FnMut(&IngestProgress),
) -> Result<()> {
    let figure_pages: Vec<usize> = figures::scan_pdf(path)?
        .figure_pages
        .into_iter()
        .map(|figure| figure.page - 1) // scan reports 1-based pages
        .collect();
    if figure_pages.is_empty() {
        return Ok(());
    }
    report.figures_found += figure_pages.len();

    let images = render::render_pages(paths, path, &figure_pages, FIGURE_DPI)?;
    let total = images.len();
    let mut consecutive_failures = 0;

    for (done, (page_index, png)) in images.into_iter().enumerate() {
        on_progress(&IngestProgress {
            stage: Stage::Captioning,
            done: done + 1,
            total,
            current: filename.to_string(),
        });

        let caption = match captions.get(&png, &config.model, vision::PROMPT_VERSION) {
            Some(cached) => cached.to_string(),
            None => match vision::caption_image(config, &png, &page_hint(pages, page_index)) {
                Ok(caption) => {
                    captions.put(&png, &config.model, vision::PROMPT_VERSION, &caption);
                    report.captions_made += 1;
                    consecutive_failures = 0;
                    caption
                }
                Err(error) => {
                    report.caption_failures += 1;
                    consecutive_failures += 1;
                    eprintln!(
                        "could not caption {} page {}: {error:#}",
                        filename,
                        page_index + 1
                    );
                    if consecutive_failures >= MAX_CONSECUTIVE_CAPTION_FAILURES {
                        report.caption_failures += figure_pages.len() - done - 1;
                        eprintln!(
                            "giving up on figures in {filename} after {consecutive_failures} \
                             failures in a row"
                        );
                        break;
                    }
                    continue;
                }
            },
        };

        pending.push((
            filename.to_string(),
            path.display().to_string(),
            fhash.to_string(),
            page_index as i32 + 1,
            caption,
            true,
        ));
    }
    Ok(())
}

/// A few words of the page's own text, so the model knows which document and
/// subject the drawing belongs to.
fn page_hint(pages: &[String], page_index: usize) -> String {
    let text = pages
        .get(page_index)
        .map(String::as_str)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut hint: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        hint.push('…');
    }
    hint
}

pub fn remove_document(paths: &Paths, filename: &str) -> Result<usize> {
    let index = Index::open(&paths.index)?;
    let removed = index
        .chunks
        .iter()
        .filter(|chunk| chunk.filename == filename)
        .count();
    if removed == 0 {
        return Ok(0);
    }

    let pairs: Vec<(Chunk, Vec<f32>)> = index
        .chunks
        .into_iter()
        .zip(index.vectors)
        .filter(|(chunk, _)| chunk.filename != filename)
        .collect();
    let (chunks, vectors): (Vec<Chunk>, Vec<Vec<f32>>) = pairs.into_iter().unzip();
    Index::save(&paths.index, chunks, vectors)?;

    let mut documents = Index::read_documents(&paths.index)?;
    documents.remove(filename);
    Index::write_documents(&paths.index, &documents)?;
    Ok(removed)
}

/// Recursively collects PDFs and plain-text documents.
pub fn document_files(source: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).with_context(|| format!("could not list {}", source.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let known = path
                .extension()
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("pdf")
                        || TEXT_EXTENSIONS.iter().any(|name| ext.eq_ignore_ascii_case(name))
                });
            if known {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

pub fn pdf_pages(path: &Path) -> Result<Vec<String>> {
    pdf_extract::extract_text_by_pages(path)
        .map_err(|error| anyhow!("could not extract text from {}: {error}", path.display()))
}

/// 16 hex characters of SHA-256, matching the id scheme the Python index used.
fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_16_hex_chars_and_stable() {
        let hash = content_hash(b"Doc 8168 Vol 2");
        assert_eq!(hash.len(), 16);
        assert_eq!(hash, content_hash(b"Doc 8168 Vol 2"));
        assert_ne!(hash, content_hash(b"different"));
    }

    #[test]
    fn document_files_finds_pdfs_and_text_but_not_other_extensions() {
        let dir = std::env::temp_dir().join(format!("rag-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        for name in ["a.pdf", "b.md", "c.txt", "d.png"] {
            fs::write(dir.join(name), "x").unwrap();
        }
        fs::write(dir.join("sub/e.pdf"), "x").unwrap();

        let names: Vec<String> = document_files(&dir)
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(names, vec!["a.pdf", "b.md", "c.txt", "e.pdf"]);
    }
}
