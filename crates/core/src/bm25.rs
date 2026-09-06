use std::collections::HashMap;

const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Okapi BM25 over the chunk table — the lexical leg that keeps exact codes,
/// section numbers, and abbreviations distinguishable when a dense model
/// embeds them close together.
pub struct Bm25 {
    doc_lengths: Vec<f32>,
    average_length: f32,
    document_count: f32,
    vocabulary: HashMap<String, u32>,
    /// term id -> (document index, term frequency)
    postings: HashMap<u32, Vec<(u32, u32)>>,
}

impl Bm25 {
    pub fn new(texts: &[String]) -> Self {
        let mut doc_lengths = Vec::with_capacity(texts.len());
        let mut vocabulary: HashMap<String, u32> = HashMap::new();
        let mut postings: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();

        for (doc, text) in texts.iter().enumerate() {
            let tokens = tokenize(text);
            doc_lengths.push(tokens.len() as f32);
            let mut counts: HashMap<u32, u32> = HashMap::new();
            for token in tokens {
                let id = match vocabulary.get(&token) {
                    Some(id) => *id,
                    None => {
                        let id = vocabulary.len() as u32;
                        vocabulary.insert(token, id);
                        id
                    }
                };
                *counts.entry(id).or_insert(0) += 1;
            }
            for (id, count) in counts {
                postings.entry(id).or_default().push((doc as u32, count));
            }
        }

        let total: f32 = doc_lengths.iter().sum();
        let document_count = texts.len().max(1) as f32;
        Self {
            doc_lengths,
            average_length: total / document_count,
            document_count,
            vocabulary,
            postings,
        }
    }

    /// One score per document, in corpus order. Only documents that contain a
    /// query term are touched, so the rest stay at zero.
    pub fn scores(&self, query: &str) -> Vec<f32> {
        let mut scores = vec![0.0f32; self.doc_lengths.len()];
        for term in tokenize(query) {
            let Some(&id) = self.vocabulary.get(&term) else {
                continue;
            };
            let list = &self.postings[&id];
            let idf =
                (1.0 + (self.document_count - list.len() as f32 + 0.5) / (list.len() as f32 + 0.5)).ln();
            for &(doc, term_frequency) in list {
                let tf = term_frequency as f32;
                let length = self.doc_lengths[doc as usize];
                let denominator =
                    tf + K1 * (1.0 - B + B * length / self.average_length.max(1.0));
                scores[doc as usize] += idf * tf * (K1 + 1.0) / denominator;
            }
        }
        scores
    }
}

pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Vec<String> {
        vec![
            "Minimum obstacle clearance inside the holding pattern is 300 m.".to_string(),
            "The holding pattern protection area depends on obstacle height.".to_string(),
            "Runway pavement bearing strength uses the Pavement Classification Number.".to_string(),
        ]
    }

    #[test]
    fn matching_documents_outscore_unrelated() {
        let scores = Bm25::new(&corpus()).scores("obstacle clearance");
        assert!(scores[0] > scores[2], "scores: {scores:?}");
        assert_eq!(scores[2], 0.0);
    }

    #[test]
    fn rare_term_dominates_a_common_one() {
        let scores = Bm25::new(&corpus()).scores("pavement classification holding");
        assert!(scores[2] > scores[0], "scores: {scores:?}");
    }

    #[test]
    fn longer_document_is_penalized_for_the_same_hit() {
        // Term frequency is deliberately identical; only length differs, which is
        // the case BM25 length normalization is supposed to penalize.
        let short = "obstacle clearance.";
        let long = "obstacle clearance is defined separately from the runway strip, \
                   the holding protective area, and the missed ascent gradient.";
        let scores = Bm25::new(&[short.to_string(), long.to_string()]).scores("obstacle clearance");
        assert!(scores[0] > scores[1], "scores: {scores:?}");
    }

    #[test]
    fn tokenize_splits_punctuation_and_case() {
        assert_eq!(tokenize("OCH-300 m."), vec!["och", "300", "m"]);
    }

    #[test]
    fn empty_corpus_scores_nothing() {
        assert!(Bm25::new(&[]).scores("obstacle").is_empty());
    }
}
