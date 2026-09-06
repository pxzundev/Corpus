use crate::config::{CHUNK_MAX_WORDS, CHUNK_OVERLAP_WORDS, CHUNK_WORDS};

/// A sentence boundary needs this many words, so periods inside abbreviations
/// ("300 m.", "vol. 2") and stray page furniture do not chop a clause in half.
const MIN_SENTENCE_WORDS: usize = 6;

/// Splits text into overlapping chunks at sentence boundaries. Sentence
/// boundaries are preferred because document text is organized into numbered clauses and a
/// numbered sub-clause often answers a query on its own; a hard word window
/// would cut mid-clause and produce passages with no subject.
///
/// Blank lines are hard breaks (PDF layout gives them meaning), and sentence
/// ends are detected inside lines too, because extracted PDF text wraps clauses
/// across arbitrary line breaks.
pub fn chunk_text(text: &str) -> Vec<String> {
    let sentences = sentences(text);
    if sentences.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < sentences.len() {
        let mut end = start;
        let mut words = 0usize;
        while end < sentences.len() {
            let length = word_count(&sentences[end]);
            // Stop before blowing past the model's context, unless the chunk is
            // still empty and this sentence is all we can fit.
            if words > 0 && words + length > CHUNK_MAX_WORDS {
                break;
            }
            words += length;
            end += 1;
            if words >= CHUNK_WORDS {
                break;
            }
        }
        let chunk: String = sentences[start..end].concat();
        if !chunk.trim().is_empty() {
            chunks.push(chunk);
        }
        if end >= sentences.len() {
            break;
        }
        start = backtrack(&sentences, start, end);
    }
    chunks
}

fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split("\n\n") {
        let mut current = String::new();
        let mut length = 0usize;
        for token in paragraph.split_whitespace() {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(token);
            length += 1;
            if length >= MIN_SENTENCE_WORDS && ends_sentence(token) {
                current.push(' ');
                push_windowed(&mut out, std::mem::take(&mut current));
                length = 0;
            }
        }
        if !current.trim().is_empty() {
            push_windowed(&mut out, current);
        }
    }
    out
}

/// Text with no sentence-ending punctuation — form fields, extracted tables,
/// line-drawn diagrams — arrives as one unbroken run. The embedding model
/// truncates at 512 tokens, so such a run is cut into overlapping word windows
/// instead of being silently dropped past the limit.
fn push_windowed(out: &mut Vec<String>, sentence: String) {
    let words: Vec<&str> = sentence.split_whitespace().collect();
    if words.len() <= CHUNK_MAX_WORDS {
        out.push(sentence);
        return;
    }
    let mut start = 0usize;
    while start < words.len() {
        let end = (start + CHUNK_MAX_WORDS).min(words.len());
        out.push(words[start..end].join(" "));
        if end >= words.len() {
            break;
        }
        start = end - CHUNK_OVERLAP_WORDS;
    }
}

/// Abbreviations common in technical documents that do not end a
/// sentence; without this, "vol. 2" splits a clause in half.
fn ends_sentence(token: &str) -> bool {
    let token = token.trim_end_matches([')', '"', '\'']);
    if !token.ends_with(['.', '?', '!']) {
        return false;
    }
    let stem = token.trim_end_matches('.').to_ascii_lowercase();
    !matches!(
        stem.as_str(),
        "vol" | "vols" | "no" | "nos" | "fig" | "para" | "subpara" | "ed" | "rev" | "approx" | "sec"
    )
}

fn word_count(sentence: &str) -> usize {
    sentence.split_whitespace().count()
}

/// Walks back from `end` to the sentence where an overlap of about
/// CHUNK_OVERLAP_WORDS words begins. The result is always greater than `start`
/// so the caller makes progress even when a chunk is one oversized sentence.
fn backtrack(sentences: &[String], start: usize, end: usize) -> usize {
    let mut words = 0usize;
    let mut candidate = end;
    while candidate > start {
        let sentence = word_count(&sentences[candidate - 1]);
        if words >= CHUNK_OVERLAP_WORDS || sentence > CHUNK_WORDS {
            break;
        }
        words += sentence;
        candidate -= 1;
    }
    candidate.max(start + 1).min(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `words` filler words ending in a traceable marker sentence.
    fn sentence(words: usize, marker: usize) -> String {
        format!("{} s{marker}.\n", "word ".repeat(words))
    }

    #[test]
    fn short_text_is_one_chunk() {
        let chunks = chunk_text("Holding is a flight maneuver in which aircraft remain over a fix.\n");
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn long_text_reaches_configured_size_and_overlaps() {
        let text: String = (0..40).map(|i| sentence(25, i)).collect();
        let chunks = chunk_text(&text);
        assert!(chunks.len() > 2, "expected several chunks, got {}", chunks.len());
        assert!(
            chunks[0].split_whitespace().count() >= CHUNK_WORDS,
            "first chunk is only {} words",
            chunks[0].split_whitespace().count()
        );

        // A sentence from the tail of chunk 0 must reappear in chunk 1, otherwise
        // a clause spanning the boundary becomes unretrievable.
        let last_marker = chunks[0]
            .split_whitespace()
            .rev()
            .find(|token| token.starts_with('s') && token.ends_with('.'))
            .expect("marker sentence in chunk tail");
        assert!(
            chunks[1].contains(last_marker),
            "chunk 1 does not carry the overlapping sentence {last_marker}"
        );
    }

    #[test]
    fn run_on_text_without_periods_is_windowed() {
        // Form fields and extracted tables produce this: thousands of words and
        // no sentence boundary anywhere.
        let text: String = (0..3000).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        let chunks = chunk_text(&text);
        assert!(chunks.len() > 3, "expected several windows, got {}", chunks.len());
        for chunk in &chunks {
            assert!(
                word_count(chunk) <= CHUNK_MAX_WORDS,
                "chunk of {} words exceeds the model context",
                word_count(chunk)
            );
        }
        // Every window after the first must overlap its predecessor, otherwise
        // the truncation limit hides text between windows.
        let second = chunks[1].split_whitespace().next().unwrap();
        assert!(chunks[0].contains(second), "windows do not overlap");
    }

    #[test]
    fn no_chunk_exceeds_the_model_context() {
        let text: String = (0..200)
            .map(|i| format!("Clause {i} requires obstacle clearance of {i}00 m above the fix.\n"))
            .collect();
        for chunk in chunk_text(&text) {
            assert!(
                word_count(&chunk) <= CHUNK_MAX_WORDS,
                "chunk of {} words exceeds the model context",
                word_count(&chunk)
            );
        }
    }

    #[test]
    fn oversized_single_sentence_terminates() {
        let text: String = (0..3000).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ") + ".";
        assert!(!chunk_text(&text).is_empty());
    }

    #[test]
    fn wrapped_lines_still_split_into_sentences() {
        let text = "Clearance is 300 m inside the fix and applies to Cat A. \nIt rises to 400 m \nfor Cat E aircraft.";
        let found = sentences(text);
        assert_eq!(found.len(), 2, "got {found:?}");
    }

    #[test]
    fn volume_abbreviation_stays_inside_its_sentence() {
        let found = sentences("Applies to Annex vol. 2 of the document. Runway length is 1800 m.");
        assert_eq!(found.len(), 2, "got {found:?}");
        assert!(found[0].trim_end().ends_with("document."), "got {:?}", found[0]);
    }

    #[test]
    fn blank_line_breaks_a_sentence() {
        let found = sentences("First clause about obstacles.\n\nSecond clause about runways.");
        assert_eq!(found.len(), 2, "got {found:?}");
    }

    #[test]
    fn empty_text_has_no_chunks() {
        assert!(chunk_text("   \n\n  ").is_empty());
    }
}
