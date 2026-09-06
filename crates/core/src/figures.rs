use std::path::Path;

use anyhow::{Context, Result};
use lopdf::{Document, Object};

/// A page worth sending to a vision model, and why we think so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FigureReason {
    /// A raster large enough to be a photograph or pasted chart, not a logo.
    Raster,
    /// Vector line drawing carrying little text: schematics, diagrams, charts.
    Drawing,
}

#[derive(Debug, Clone, Copy)]
pub struct FigurePage {
    pub page: usize,
    pub reason: FigureReason,
    pub images: usize,
}

/// What indexing a document would cost in vision calls.
#[derive(Debug, Clone, Default)]
pub struct FigureScan {
    pub pages: usize,
    pub figure_pages: Vec<FigurePage>,
}

impl FigureScan {
    pub fn raster_pages(&self) -> usize {
        self.count(FigureReason::Raster)
    }

    pub fn drawing_pages(&self) -> usize {
        self.count(FigureReason::Drawing)
    }

    pub fn needs_vision(&self) -> bool {
        !self.figure_pages.is_empty()
    }

    fn count(&self, reason: FigureReason) -> usize {
        self.figure_pages
            .iter()
            .filter(|page| page.reason == reason)
            .count()
    }
}

/// Where page furniture stops. Height is the reliable axis: this corpus's
/// header bands are up to 868x225, while 396x611 is a real rotated figure and
/// 900x300 screenshots clear a 300 floor. Checking pixels alone would let the
/// header bands through.
pub fn is_substantial(width: u32, height: u32) -> bool {
    width >= 250 && height >= 250 && u64::from(width) * u64::from(height) >= 120_000
}

/// A page of figure geometry paints many paths and says little, whereas a page
/// of prose paints almost none.
fn is_drawing(ops: usize, text_chars: usize) -> bool {
    ops >= 60 && text_chars <= 400
}

const PATH_OPERATORS: [&[u8]; 10] =
    [b"re", b"m", b"l", b"c", b"v", b"y", b"f", b"F", b"S", b"B"];

/// Counts path-painting operators. Splitting on whitespace and string
/// delimiters keeps the `l` inside a literal text string from counting as a
/// line, which a byte-wise search would get wrong on every text page.
fn path_ops(content: &[u8]) -> usize {
    content
        .split(|byte| byte.is_ascii_whitespace() || b"[]<>{}".contains(byte))
        .filter(|token| PATH_OPERATORS.contains(token))
        .count()
}

fn page_dictionary<'a>(document: &'a Document, page: &'a Object) -> Option<&'a lopdf::Dictionary> {
    match page {
        Object::Dictionary(dictionary) => Some(dictionary),
        Object::Reference(id) => document.get_object(*id).ok().and_then(|page| match page {
            Object::Dictionary(dictionary) => Some(dictionary),
            _ => None,
        }),
        _ => None,
    }
}

fn image_size(document: &Document, reference: &Object) -> Option<(u32, u32)> {
    let stream = match reference {
        Object::Reference(id) => document.get_object(*id).ok()?,
        other => other,
    };
    let stream = stream.as_stream().ok()?;
    let dictionary = &stream.dict;
    if dictionary.get(b"Subtype").and_then(Object::as_name).ok() != Some(&b"Image"[..]) {
        return None;
    }
    let side = |key: &[u8]| {
        dictionary
            .get(key)
            .and_then(Object::as_i64)
            .unwrap_or(0)
            .max(0) as u32
    };
    Some((side(b"Width"), side(b"Height")))
}

/// /Resources is inherited down the page tree, and most producers put it on a
/// shared parent node rather than on every page, so a page's own dictionary
/// usually does not contain it.
fn inherited<'a>(document: &'a Document, page: &'a lopdf::Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut current = Some(page);
    for _ in 0..8 {
        let Some(dictionary) = current else {
            return None;
        };
        if let Ok(object) = dictionary.get(key) {
            return Some(match object {
                Object::Reference(id) => document.get_object(*id).ok()?,
                other => other,
            });
        }
        current = dictionary
            .get(b"Parent")
            .ok()
            .and_then(|parent| parent.as_reference().ok())
            .and_then(|id| document.get_object(id).ok())
            .and_then(|parent| parent.as_dict().ok());
    }
    None
}

fn substantial_images<'a>(document: &'a Document, page: &'a Object) -> usize {
    let Some(dictionary) = page_dictionary(document, page) else {
        return 0;
    };
    let Some(Object::Dictionary(resources)) = inherited(document, dictionary, b"Resources") else {
        return 0;
    };
    let Ok(xobjects) = resources.get(b"XObject").and_then(Object::as_dict) else {
        return 0;
    };

    xobjects
        .iter()
        .filter_map(|(_, entry)| image_size(document, entry))
        .filter(|(width, height)| is_substantial(*width, *height))
        .count()
}

/// What counting the figures in one document is doing right now. Text
/// extraction has no per-page callback in pdf-extract, so `Text` reports the
/// page count it is about to read and no position; `Pages` reports both.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub text_phase: bool,
    pub page: usize,
    pub pages: usize,
}

/// Pages between progress reports while classifying. A 958-page document would
/// otherwise emit a thousand events to tell the window what it already knows.
const SCAN_REPORT_EVERY: usize = 16;

pub fn scan_pdf(path: &Path) -> Result<FigureScan> {
    scan_pdf_with(path, &mut |_| {})
}

/// What indexing a document would cost in vision calls, reporting where it is.
/// The two passes below are the whole cost of this function, and on a document
/// of a thousand pages they take tens of seconds: text extraction first, then a
/// per-page walk over image XObjects and content operators.
pub fn scan_pdf_with(path: &Path, on_progress: &mut dyn FnMut(&ScanProgress)) -> Result<FigureScan> {
    let document =
        Document::load(path).with_context(|| format!("could not open {}", path.display()))?;
    let ids: Vec<_> = document.page_iter().collect();

    on_progress(&ScanProgress {
        text_phase: true,
        page: 0,
        pages: ids.len(),
    });
    let page_texts = crate::ingest::pdf_pages(path).unwrap_or_default();

    let mut figure_pages = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        if index % SCAN_REPORT_EVERY == 0 {
            // The page about to be read, so the count never sits at zero while
            // the walk is working.
            on_progress(&ScanProgress {
                text_phase: false,
                page: index + 1,
                pages: ids.len(),
            });
        }
        let Ok(page) = document.get_object(*id) else {
            continue;
        };
        let images = substantial_images(&document, page);
        let text = page_texts.get(index).map(String::as_str).unwrap_or_default();
        let reason = if images > 0 {
            Some(FigureReason::Raster)
        } else {
            let ops = document
                .get_page_content(*id)
                .map(|content| path_ops(&content))
                .unwrap_or(0);
            is_drawing(ops, text.chars().count()).then_some(FigureReason::Drawing)
        };
        if let Some(reason) = reason {
            figure_pages.push(FigurePage {
                page: index + 1,
                reason,
                images,
            });
        }
    }

    if !ids.is_empty() {
        on_progress(&ScanProgress {
            text_phase: false,
            page: ids.len(),
            pages: ids.len(),
        });
    }

    Ok(FigureScan {
        pages: ids.len().max(page_texts.len()),
        figure_pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_logos_are_not_figures() {
        assert!(!is_substantial(180, 60));
        assert!(!is_substantial(199, 900));
        // The Part 173 header band repeats on every page.
        assert!(!is_substantial(868, 225));
    }

    #[test]
    fn pasted_charts_are_figures() {
        assert!(is_substantial(900, 700));
        assert!(is_substantial(500, 500));
        assert!(is_substantial(900, 300));
        // A rotated diagram: narrow but tall.
        assert!(is_substantial(396, 611));
    }

    #[test]
    fn letters_inside_text_strings_are_not_path_operators() {
        // A prose page: the words contain l, m, c, f, S as substrings.
        let content = b"BT /F1 12 Tf 14 TL (Confirmation of compliance) Tj (filler) Tj ET";
        assert_eq!(path_ops(content), 0);
    }

    #[test]
    fn figure_geometry_counts_as_paths() {
        let content = b"1 0 0 1 0 0 cm 4 72 300 200 re S 10 20 m 30 40 l 50 60 c S f";
        assert_eq!(path_ops(content), 7);
        // A real figure page paints far more paths than a hand-written sample.
        assert!(is_drawing(60, 30));
        assert!(is_drawing(400, 400));
        assert!(!is_drawing(path_ops(content), 30));
    }

    #[test]
    fn a_prose_page_is_not_drawing_geometry() {
        assert!(!is_drawing(path_ops(b"BT (Altitude 1500 ft) Tj ET"), 400));
        assert!(!is_drawing(59, 20));
        assert!(!is_drawing(200, 401));
    }

    #[test]
    fn scan_reports_pages_and_figures() {
        let scan = FigureScan {
            pages: 4,
            figure_pages: vec![
                FigurePage { page: 2, reason: FigureReason::Raster, images: 1 },
                FigurePage { page: 3, reason: FigureReason::Drawing, images: 0 },
            ],
        };
        assert_eq!(scan.raster_pages(), 1);
        assert_eq!(scan.drawing_pages(), 1);
        assert!(scan.needs_vision());
        assert!(!FigureScan::default().needs_vision());
    }

    #[test]
    fn unreadable_pdf_is_an_error_not_a_empty_scan() {
        let error = scan_pdf(Path::new("/tmp/definitely-not-a-pdf-rag-mcp-rs.pdf")).unwrap_err();
        assert!(error.to_string().contains("could not open"));
    }

    /// These events are what the window shows between picking documents and the
    /// captioning question, so they have to land while the walk works rather
    /// than when it finishes. Defaults to this repo's `docs/`.
    #[test]
    #[ignore = "needs the real corpus"]
    fn scan_reports_progress_while_it_walks_the_pages() {
        let dir = std::env::var("RAG_FIGURE_CORPUS")
            .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs").to_string());
        let path = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("pdf"))
            .unwrap();

        let mut events = Vec::new();
        let started = std::time::Instant::now();
        let mut text_seconds = -1.0;
        let scan = scan_pdf_with(&path, &mut |progress| {
            if text_seconds < 0.0 && !progress.text_phase {
                // Where the time actually goes: text extraction runs whole
                // document and cannot report a position.
                text_seconds = started.elapsed().as_secs_f64();
            }
            events.push((progress.text_phase, progress.page, progress.pages))
        })
        .unwrap();

        let pages = events[0].2;
        assert!(events[0].0, "text extraction is announced before it starts");
        assert!(pages > 0, "and it says how many pages are ahead");

        let walk: Vec<(usize, usize)> = events
            .iter()
            .filter(|event| !event.0)
            .map(|event| (event.1, event.2))
            .collect();
        assert!(!walk.is_empty(), "the page walk reports as it goes");
        assert!(
            walk.iter().all(|(_, total)| *total == pages),
            "one document's count does not change mid-walk"
        );
        assert!(
            walk.windows(2).all(|pair| pair[1].0 >= pair[0].0),
            "and the count only moves forward"
        );
        assert_eq!(walk.last().unwrap().0, pages, "and it ends on the last page");
        println!(
            "{}: {} pages, text pass {:.1}s, page walk {:.1}s, {} events",
            path.file_name().unwrap().to_string_lossy(),
            scan.pages,
            text_seconds,
            started.elapsed().as_secs_f64() - text_seconds,
            events.len(),
        );
    }

    /// Measures the detection thresholds against a real corpus, defaulting to
    /// this repo's `docs/` (override with RAG_FIGURE_CORPUS). The numbers decide
    /// whether the 60-op gate above is right, so run this before trusting it.
    #[test]
    #[ignore = "writes a lot of output and takes about a minute"]
    fn measure_detection_against_a_real_corpus() {
        let dir = std::env::var("RAG_FIGURE_CORPUS")
            .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs").to_string());
        let mut totals = (0usize, 0usize, 0usize);
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
                continue;
            }
            let scan = scan_pdf(&path).unwrap();
            totals.0 += scan.pages;
            totals.1 += scan.raster_pages();
            totals.2 += scan.drawing_pages();
            println!(
                "{:36} {:4} pages  {:3} raster  {:3} drawing",
                path.file_name().unwrap().to_string_lossy(),
                scan.pages,
                scan.raster_pages(),
                scan.drawing_pages()
            );
        }
        println!("TOTAL {} pages, {} raster pages, {} drawing pages", totals.0, totals.1, totals.2);
    }
}


