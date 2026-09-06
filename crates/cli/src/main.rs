use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, anyhow};
use corpus_core::ingest::{Stage, document_files, ingest_files};
use corpus_core::vision::VisionConfig;
use corpus_core::{DEFAULT_K, Encoder, Index, Paths, Reranker, format_documents, format_results, search};

const USAGE: &str = "\
corpus — local hybrid retrieval (RAG) over your documents

  corpus probe                       Embed test texts with the pinned model and print scores
  corpus index <documents dir>       Extract, chunk, embed, and store documents
                                  (text only; add --vision for figures)
  corpus index <documents dir> --vision
                                  Also rasterise figure pages and index a vision
                                  model's caption of each. Slow: seconds per page
                                  on the first run, cached afterwards.
                                  Config: RAG_VISION_URL, RAG_VISION_MODEL.
  corpus search <query> [-k n] [--file <filename>] [--no-rerank]
  corpus list                        List indexed documents with chunk and page counts
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        println!("{USAGE}");
        return Ok(());
    };
    let paths = Paths::resolve()?;

    match command {
        "probe" => probe(&paths),
        "index" => {
            let source = args
                .get(1)
                .map(PathBuf::from)
                .filter(|arg| !arg.starts_with("--"))
                .ok_or_else(|| anyhow!("index needs a documents directory"))?;
            index(&paths, &source, args.iter().any(|arg| arg == "--vision"))
        }
        "search" => {
            let query = args
                .get(1)
                .cloned()
                .filter(|arg| !arg.starts_with('-'))
                .ok_or_else(|| anyhow!("search needs a query"))?;
            let mut k = DEFAULT_K;
            let mut file = None;
            let mut rerank = true;
            let mut index_position = 2;
            while index_position < args.len() {
                match args[index_position].as_str() {
                    "-k" => {
                        index_position += 1;
                        k = args
                            .get(index_position)
                            .and_then(|value| value.parse().ok())
                            .ok_or_else(|| anyhow!("-k expects a number"))?;
                    }
                    "--file" => {
                        index_position += 1;
                        file = args.get(index_position).cloned();
                    }
                    "--no-rerank" => rerank = false,
                    _ => {}
                }
                index_position += 1;
            }
            query_index(&paths, &query, k, file.as_deref(), rerank)
        }
        "list" => list(&paths),
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(anyhow!("unknown command: {other}\n\n{USAGE}")),
    }
}

fn probe(paths: &Paths) -> Result<()> {
    println!("loading {}", corpus_core::config::EMBED_MODEL);
    let mut encoder = Encoder::load(paths)?;
    let texts = [
        "Minimum obstacle clearance for a holding pattern is 300 m inside the holding pattern fix.",
        "Obstacle protection area for holding and the applicable minimum obstacle clearance criteria.",
        "Runway pavement bearing strength is reported using the Pavement Classification Number.",
    ];
    let vectors = encoder.encode(&texts)?;
    println!("vectors: {} x {} dim", vectors.len(), vectors[0].len());
    println!(
        "related pair   cosine {:.4}",
        corpus_core::embed::cosine(&vectors[0], &vectors[1])
    );
    println!(
        "unrelated pair cosine {:.4}",
        corpus_core::embed::cosine(&vectors[0], &vectors[2])
    );
    Ok(())
}

fn index(paths: &Paths, source: &PathBuf, with_vision: bool) -> Result<()> {
    let started = Instant::now();
    println!("indexing {} into {}", source.display(), paths.index.display());
    let mut encoder = Encoder::load(paths)?;
    // Captioning is opt-in per run: it is the slow, model-dependent part of
    // ingest, and the config comes from RAG_VISION_URL / RAG_VISION_MODEL.
    let vision = if with_vision {
        let config = VisionConfig::from_env();
        println!("captioning figures with {} at {}", config.model, config.base_url);
        Some(config)
    } else {
        None
    };
    let report = ingest_files(
        paths,
        &document_files(source)?,
        &mut encoder,
        vision.as_ref(),
        &mut |progress| match progress.stage {
            Stage::Extracting => eprintln!(
                "extracting {}/{} — {}",
                progress.done, progress.total, progress.current
            ),
            Stage::Captioning => eprintln!(
                "captioning {}/{} figures — {}",
                progress.done, progress.total, progress.current
            ),
            Stage::Embedding => eprintln!("embedded {}/{} chunks", progress.done, progress.total),
            Stage::Saving => eprintln!("saving index"),
            Stage::Done => {}
        },
    )?;

    println!(
        "\n{} file(s) indexed, {} unchanged, {} new chunks",
        report.indexed_files.len(),
        report.skipped_files.len(),
        report.new_chunks
    );
    if with_vision {
        println!(
            "{} figure page(s): {} captioned, {} failed",
            report.figures_found, report.captions_made, report.caption_failures
        );
    }
    for filename in &report.indexed_files {
        println!("  + {filename}");
    }
    println!(
        "index now holds {} chunks across {} documents in {:.1}s",
        report.total_chunks,
        report.total_documents,
        started.elapsed().as_secs_f32()
    );
    Ok(())
}

fn query_index(
    paths: &Paths,
    query: &str,
    k: usize,
    file: Option<&str>,
    rerank: bool,
) -> Result<()> {
    let started = Instant::now();
    let index = Index::open(&paths.index)?;
    let mut encoder = Encoder::load(paths)?;

    let mut reranker = match rerank {
        true => Some(Reranker::load(paths)?),
        false => None,
    };

    let scope = file.map(|name| vec![name.to_string()]);
    let results = search(&index, &mut encoder, reranker.as_mut(), query, k, scope.as_deref())?;
    println!("{}", format_results(&results));
    println!("\n{} results in {:.2}s", results.len(), started.elapsed().as_secs_f32());
    Ok(())
}

fn list(paths: &Paths) -> Result<()> {
    let index = Index::open(&paths.index)?;
    println!("{}", format_documents(&index));
    Ok(())
}
