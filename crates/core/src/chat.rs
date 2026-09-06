//! Conversational chat over the indexed corpus, against the same local engine
//! used for figure captions. Retrieval runs invisibly per turn (NotebookLM
//! style: the user talks to the app, the app looks the passages up).
//!
//! No chat-template feature is assumed: the model is asked for JSON and the
//! answer is salvaged from whatever wrapper (a code fence, a prose prefix) it
//! puts around it.
//!
//! The stream protocol is deliberately simple: the whole SSE body is read (the
//! HTTP layer is blocking ureq), split into `data:` lines, and the tokens are
//! emitted from a plain `Vec<ChatEvent>`. There is no incremental JSON or
//! byte-offset parser — a local engine answers in seconds, so the GUI gets the
//! events and reveals the answer in its own time.

use std::path::PathBuf;
use std::pin::Pin;

use anyhow::{Context, Result, bail};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::search::{SearchResult, format_results_numbered};
use crate::vision::{VisionConfig, ensure_local_endpoint};

/// How many turns of the exchange travel back to the model each call. The
/// retrieval happens fresh every turn, so history only has to carry the
/// conversation's thread, not its evidence.
pub const HISTORY_TURNS: usize = 12;

/// A boxed token stream. See the module note: intentionally not `Send`.
pub type ChatStream = Pin<Box<dyn Stream<Item = ChatEvent>>>;

/// One turn as the user typed it and the assistant answered. Assistant content
/// is plain text (the reply with citation markers in it); citation chips are
/// rebuilt from `sources` when the transcript renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// A saved conversation, per user like a ChatGPT/NotebookLM chat. Stored as one
/// JSON file per session beside the index: chats are small, and a file per chat
/// means one corrupt chat cannot take the others with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    /// Shown in the picker. Generated from the first question; renameable.
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    /// Chatting is always grounded in the corpus; this names the model that
    /// answers, remembered per chat so switching engines does not rewrite them.
    pub model: String,
    /// Optional per-chat document scope (NotebookLM source selection).
    pub filename: Option<String>,
    pub passages: usize,
    pub turns: Vec<ChatTurn>,
    /// Citation metadata, one entry per assistant turn (nulls for user turns),
    /// so chips survive a reload without keeping the passage text itself.
    pub sources: Vec<Value>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMeta {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub model: String,
    pub filename: Option<String>,
    pub turns: usize,
}

impl ChatSession {
    pub fn new(id: String, model: &str) -> Self {
        let now = now_iso();
        Self {
            id,
            title: "New chat".to_string(),
            created_at: now.clone(),
            updated_at: now,
            model: model.to_string(),
            filename: None,
            passages: crate::config::DEFAULT_K,
            turns: Vec::new(),
            sources: Vec::new(),
            state: "active".to_string(),
        }
    }

    fn path_for(paths: &crate::config::Paths, id: &str) -> PathBuf {
        paths.data.join("chats").join(format!("{id}.json"))
    }

    pub fn load(paths: &crate::config::Paths, id: &str) -> Result<Self> {
        let file = Self::path_for(paths, id);
        if !file.exists() {
            bail!("chat '{id}' does not exist");
        }
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("cannot read chat '{id}'"))?;
        let session: Self =
            serde_json::from_str(&text).with_context(|| format!("chat '{id}' is corrupt"))?;
        Ok(session)
    }

    pub fn save(&self, paths: &crate::config::Paths) -> Result<()> {
        let file = Self::path_for(paths, &self.id);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write-and-rename so a crash mid-save cannot truncate the chat.
        let temp = file.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&temp, &file)?;
        Ok(())
    }

    pub fn list(paths: &crate::config::Paths) -> Result<Vec<ChatMeta>> {
        let dir = paths.data.join("chats");
        let mut metas: Vec<ChatMeta> = Vec::new();
        if dir.exists() {
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                // A corrupt chat must not hide the healthy ones.
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(session) = serde_json::from_str::<Self>(&text) else {
                    continue;
                };
                if session.state != "active" {
                    continue;
                }
                metas.push(ChatMeta {
                    id: session.id,
                    title: session.title,
                    created_at: session.created_at,
                    updated_at: session.updated_at,
                    model: session.model,
                    filename: session.filename,
                    turns: session.turns.len(),
                });
            }
        }
        metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(metas)
    }

    pub fn delete(paths: &crate::config::Paths, id: &str) -> Result<()> {
        let file = Self::path_for(paths, id);
        if !file.exists() {
            bail!("chat '{id}' does not exist");
        }
        std::fs::remove_file(&file)?;
        Ok(())
    }

    /// Delete into `chats/trash/` instead of removing, so the GUI's Undo toast
    /// can restore the chat even after a reload. `list` skips the subdirectory,
    /// so a trashed chat is invisible exactly like a deleted one.
    pub fn trash(paths: &crate::config::Paths, id: &str) -> Result<()> {
        let file = Self::path_for(paths, id);
        if !file.exists() {
            bail!("chat '{id}' does not exist");
        }
        let dir = paths.data.join("chats").join("trash");
        std::fs::create_dir_all(&dir)?;
        std::fs::rename(&file, dir.join(format!("{id}.json")))?;
        Ok(())
    }

    /// Put a trashed chat back. Restores the file under its original id.
    pub fn restore(paths: &crate::config::Paths, id: &str) -> Result<Self> {
        let trashed = paths
            .data
            .join("chats")
            .join("trash")
            .join(format!("{id}.json"));
        if !trashed.exists() {
            bail!("trashed chat '{id}' does not exist");
        }
        let session: Self = serde_json::from_str(
            &std::fs::read_to_string(&trashed)
                .with_context(|| format!("cannot read trashed chat '{id}'"))?,
        )
        .with_context(|| format!("trashed chat '{id}' is corrupt"))?;
        session.save(paths)?;
        std::fs::remove_file(&trashed)?;
        Ok(session)
    }

    /// Title from the first question (NotebookLM behavior), single line, cut
    /// at a word boundary.
    pub fn title_from(question: &str) -> String {
        let flat: String = question
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        let flat = flat.trim();
        if flat.chars().count() <= 48 {
            return flat.to_string();
        }
        let cut: String = flat.chars().take(48).collect();
        match cut.rfind(' ') {
            Some(space) if space > 16 => format!("{}…", &cut[..space]),
            _ => format!("{}…", cut.trim_end()),
        }
    }

    /// The last question in the transcript, for regenerate.
    pub fn last_question(&self) -> Option<&str> {
        self.turns.iter().rev().find(|t| t.role == "user").map(|t| t.content.as_str())
    }

    /// Drop the trailing assistant turn (and its citations) for regenerate.
    pub fn pop_assistant_turn(&mut self) {
        if let Some(turn) = self.turns.pop() {
            if turn.role == "assistant" {
                self.sources.pop();
            } else {
                self.turns.push(turn);
            }
        }
    }

    /// Push the user's question, generating the title on the first one.
    pub fn push_question(&mut self, question: &str) {
        if self.turns.is_empty() {
            self.title = Self::title_from(question);
        }
        self.turns.push(ChatTurn {
            role: "user".to_string(),
            content: question.to_string(),
        });
    }

    pub fn push_answer(&mut self, answer: &str, sources: Value) {
        self.turns.push(ChatTurn {
            role: "assistant".to_string(),
            content: answer.to_string(),
        });
        self.sources.push(sources);
    }
}

/// One step of a chat answer, streamed to the GUI.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    /// One decoded token of the answer's markdown (already unescaped).
    Token { token: String },
    /// The final, parsed answer with its citations.
    Done { value: Value },
    Error { message: String },
    /// The user pressed Stop; the worker drops the exchange instead of
    /// revealing or saving it.
    Cancelled,
}

/// One chat round: the caller has already grounded the question in the corpus
/// (the GUI does the retrieval with its cached models), so this builds the
/// thread, sends it plus the passages to the model, and returns the stream.
///
/// `question` is what the user typed now; `history` is everything before it.
/// `regenerate` reuses the transcript's last question: the history drops the
/// final assistant turn, the prompt keeps the question.
pub fn answer(
    config: &VisionConfig,
    question: &str,
    history: &[ChatTurn],
    results: &[SearchResult],
    regenerate: bool,
) -> Result<ChatStream> {
    let prompt = chat_prompt(&thread(history, question, regenerate), results);

    let body = json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ],
        "stream": true,
    });
    let url = config.chat_url();
    ensure_local_endpoint(&url)?;

    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let request = ureq::post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .config()
        .timeout_global(Some(timeout))
        .build();
    let mut response = request
        .send(body.to_string())
        .with_context(|| format!("request to {url} failed"))?;

    let status = response.status();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    if status != 200 {
        bail!(
            "model returned {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }

    let events = parse_response(&text, results)?;
    Ok(Box::pin(futures_util::stream::iter(events)))
}

const SYSTEM_PROMPT: &str = "You answer questions about the user's documents using ONLY the numbered passages provided. \
Quote passage numbers as [1], [2] when you use them. \
Format the answer in markdown: headings, **bold**, *italic*, lists, tables and fenced code blocks. \
Draw a flow or process as a fenced mermaid block; draw any other vector graphic as a fenced svg block holding self-contained <svg> markup. \
If the passages do not contain the answer, say so. \
Answer in the language of the question.";

/// What travels to the model: the transcript plus this question, cut to the
/// last `HISTORY_TURNS`. A regenerate drops the answer it is replacing but
/// keeps its question, which is the whole point of pressing the button again.
fn thread(history: &[ChatTurn], question: &str, regenerate: bool) -> Vec<ChatTurn> {
    let mut turns: Vec<ChatTurn> = if regenerate {
        let mut trimmed = history.to_vec();
        if trimmed.last().is_some_and(|turn| turn.role == "assistant") {
            trimmed.pop();
        }
        trimmed
    } else {
        history.to_vec()
    };
    // A regenerate has just left the question it is re-answering as the last turn,
    // so pushing `question` again would put it to the model twice.
    if !turns
        .last()
        .is_some_and(|turn| turn.role == "user" && turn.content == question)
    {
        turns.push(ChatTurn {
            role: "user".to_string(),
            content: question.to_string(),
        });
    }
    turns[turns.len().saturating_sub(HISTORY_TURNS)..].to_vec()
}

/// Build the chat prompt: recent turns, then the fresh retrieval. The thread
/// goes in as plain text (no chat-template feature is assumed), the passages
/// as the grounding block.
fn chat_prompt(turns: &[ChatTurn], results: &[SearchResult]) -> String {
    let mut prompt = String::new();
    for turn in turns {
        let speaker = match turn.role.as_str() {
            "user" => "User",
            _ => "Assistant",
        };
        prompt.push_str(speaker);
        prompt.push_str(": ");
        prompt.push_str(&turn.content);
        prompt.push('\n');
    }
    prompt.push_str("\nPassages:\n");
    prompt.push_str(&format_results_numbered(results));
    prompt.push_str(
        "\nAnswer the last question using only the passages above, citing passage numbers \
         like [2]. Reply with a JSON object: {\"answer\": \"your answer in markdown\"}. \
         No text before or after the JSON.",
    );
    prompt
}

/// Citation metadata for one turn: what the inline [N] citations need, nothing more.
fn sources_json(results: &[SearchResult]) -> Value {
    Value::Array(
        results
            .iter()
            .map(|result| {
                json!({
                    "id": result.id,
                    "filename": result.filename,
                    "page": result.page,
                    "kind": result.score_kind,
                    "score": result.score,
                    "figure": result.figure,
                })
            })
            .collect(),
    )
}

/// Read the SSE body into events: `data:` lines carry `choices[0].delta.content`
/// tokens, and the answer is salvaged from the concatenated text at the end. A
/// server that ignored `stream` sends one JSON body instead — that is handled
/// too, and the same salvage covers whatever wrapper it used.
fn parse_response(text: &str, results: &[SearchResult]) -> Result<Vec<ChatEvent>> {
    let mut tokens: Vec<String> = Vec::new();
    let mut saw_sse = false;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        saw_sse = true;
        if data == "[DONE]" {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue; // not a frame of ours: skip
        };
        if let Some(token) = value["choices"][0]["delta"]["content"].as_str() {
            tokens.push(token.to_string());
        }
    }

    if !saw_sse {
        // One JSON body, no SSE framing.
        if let Ok(value) = serde_json::from_str::<Value>(text)
            && let Some(content) = value["choices"][0]["message"]["content"].as_str()
        {
            tokens.push(content.to_string());
        }
    }

    let mut events = Vec::new();
    for token in &tokens {
        events.push(ChatEvent::Token {
            token: token.clone(),
        });
    }
    let joined = tokens.join("");
    let answer = extract_answer(&joined).unwrap_or(joined);
    events.push(ChatEvent::Done {
        value: json!({"answer": answer, "sources": sources_json(results)}),
    });
    Ok(events)
}

/// Salvage the answer from the model's raw text: prefer the parsed object,
/// then the `"answer"` string value even if the JSON is broken, then a code
/// fence, then everything. The request asks for JSON, but a vision checkpoint
/// may wrap it — the user must see a reply either way.
fn extract_answer(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    // 1. Well-formed JSON.
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Some(answer) = value.get("answer").and_then(Value::as_str)
    {
        return Some(answer.to_string());
    }
    // 2. Salvage the string value of "answer" by hand: unescape until the
    //    closing quote. Works when the rest of the object (or its fences) is
    //    broken.
    if let Some(start) = find_answer_value_start(trimmed) {
        let bytes = trimmed.as_bytes();
        let mut out = String::new();
        let mut i = start;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if i + 1 < bytes.len() => {
                    let escaped = match bytes[i + 1] {
                        b'n' => Some('\n'),
                        b't' => Some('\t'),
                        b'r' => Some('\r'),
                        b'"' => Some('"'),
                        b'\\' => Some('\\'),
                        b'/' => Some('/'),
                        _ => None,
                    };
                    if let Some(ch) = escaped {
                        out.push(ch);
                        i += 2;
                        continue;
                    }
                    // `get` not `[]`: a malformed `\u` can put the slice end inside a
                    // multi-byte char, and the whole point here is not to panic.
                    if bytes[i + 1] == b'u'
                        && let Some(hex) = trimmed.get(i + 2..i + 6)
                        && let Ok(code) = u32::from_str_radix(hex, 16)
                        && let Some(ch) = char::from_u32(code)
                    {
                        out.push(ch);
                        i += 6;
                        continue;
                    }
                    break;
                }
                b'"' => return Some(out),
                _ => {
                    // Copy the whole char at its real width.
                    let ch = trimmed[start..][i - start..]
                        .chars()
                        .next()
                        .expect("non-empty slice has a char");
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    // 3. A code fence around the JSON, or around plain markdown.
    if let Some(start) = trimmed.find("```") {
        let inner = &trimmed[start + 3..];
        let inner = inner.strip_prefix("json").unwrap_or(inner);
        if let Some(end) = inner.rfind("```") {
            let inner = inner[..end].trim();
            if !inner.is_empty() {
                if let Some(answer) = extract_answer(inner) {
                    return Some(answer);
                }
                return Some(inner.to_string());
            }
        }
    }
    // 4. Prose before the JSON: start at the first brace.
    if let Some(brace) = trimmed.find('{')
        && let Ok(value) = serde_json::from_str::<Value>(&trimmed[brace..])
        && let Some(answer) = value.get("answer").and_then(Value::as_str)
    {
        return Some(answer.to_string());
    }
    // 5. The model ignored the JSON request: answer with its text as-is.
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The byte just past the opening quote of the `"answer"` value. The key must
/// sit at an object start (a `,` or `{` precedes its quote), so a prose echo
/// of the prompt's schema line — where the quoted word sits mid-sentence —
/// cannot trigger a half-parsed answer.
fn find_answer_value_start(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let quote = b'\"';
    let needle = b"answer\"";
    let mut previous = None;
    let mut index = 0usize;
    while index + needle.len() <= bytes.len() {
        if previous == Some(quote) && &bytes[index..index + needle.len()] == needle {
            let mut after = index + needle.len();
            while after < bytes.len() && bytes[after].is_ascii_whitespace() {
                after += 1;
            }
            if after < bytes.len() && bytes[after] == b':' {
                after += 1;
                while after < bytes.len() && bytes[after].is_ascii_whitespace() {
                    after += 1;
                }
                if after < bytes.len() && bytes[after] == quote {
                    return Some(after + 1);
                }
            }
        }
        previous = Some(bytes[index]);
        index += 1;
    }
    None
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn collect_stream(stream: ChatStream) -> Vec<ChatEvent> {
    futures_executor::block_on(async { stream.collect().await })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(filename: &str, page: i32, text: &str) -> SearchResult {
        SearchResult {
            id: format!("{filename}:{page}:0"),
            score: 0.9,
            score_kind: "rerank",
            filename: filename.to_string(),
            page,
            text: text.to_string(),
            figure: false,
        }
    }

    fn temp_data(name: &str) -> crate::config::Paths {
        let root = std::env::temp_dir().join(format!("rag-chat-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        crate::config::Paths {
            data: root.clone(),
            models: root.join("models"),
            index: root.join("index"),
        }
    }

    #[test]
    fn the_prompt_carries_the_thread_and_the_passages() {
        let turns = vec![
            ChatTurn {
                role: "user".to_string(),
                content: "first question".to_string(),
            },
            ChatTurn {
                role: "assistant".to_string(),
                content: "first answer".to_string(),
            },
        ];
        let results = vec![result("a.pdf", 2, "because 4.5%")];
        let prompt = chat_prompt(&turns, &results);
        assert!(prompt.contains("User: first question"));
        assert!(prompt.contains("Assistant: first answer"));
        assert!(prompt.contains("[1] [a.pdf p.2 | rerank 0.90]"), "passages carry the number the answer cites");
        assert!(prompt.contains("because 4.5%"));
    }

    #[test]
    fn a_first_question_becomes_a_title() {
        assert_eq!(ChatSession::title_from("What is the gradient?"), "What is the gradient?");
        let long = ChatSession::title_from(&"word ".repeat(40));
        assert!(long.chars().count() <= 49);
        assert!(long.ends_with('…'));
        assert!(!long.ends_with(" …"));
        assert_eq!(ChatSession::title_from("  padded  "), "padded");
    }

    #[test]
    fn a_session_round_trips_through_disk() {
        let paths = temp_data("round-trip");

        let mut session = ChatSession::new("c1".into(), "vision-model");
        session.push_question("first question");
        session.push_answer(
            "an [1] answer",
            json!([{"filename": "a.pdf", "page": 2, "kind": "rerank", "score": 0.9, "figure": false}]),
        );
        session.save(&paths).unwrap();

        let loaded = ChatSession::load(&paths, "c1").unwrap();
        assert_eq!(loaded.title, "first question");
        assert_eq!(loaded.turns.len(), 2);
        assert_eq!(loaded.sources.len(), 1);

        let metas = ChatSession::list(&paths).unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].turns, 2);

        // Regenerate: drop the assistant turn only.
        let mut again = loaded.clone();
        again.pop_assistant_turn();
        assert_eq!(again.turns.len(), 1);
        assert_eq!(again.sources.len(), 0);

        // A corrupt file must not poison the listing.
        std::fs::write(paths.data.join("chats").join("bad.json"), "not json").unwrap();
        assert_eq!(ChatSession::list(&paths).unwrap().len(), 1);

        ChatSession::delete(&paths, "c1").unwrap();
        assert!(ChatSession::load(&paths, "c1").is_err());

        std::fs::remove_dir_all(&paths.data).ok();
    }

    #[test]
    fn a_trashed_chat_hides_from_the_list_and_restores_whole() {
        let paths = temp_data("trash");

        let mut session = ChatSession::new("c1".into(), "vision-model");
        session.push_question("first question");
        session.save(&paths).unwrap();

        ChatSession::trash(&paths, "c1").unwrap();
        assert!(ChatSession::load(&paths, "c1").is_err(), "a trashed chat is gone from view");
        assert_eq!(ChatSession::list(&paths).unwrap().len(), 0, "the trash folder stays out of the picker");

        let restored = ChatSession::restore(&paths, "c1").unwrap();
        assert_eq!(restored.title, "first question");
        assert_eq!(restored.turns.len(), 1);
        assert_eq!(ChatSession::list(&paths).unwrap().len(), 1);

        // Restoring twice fails: the second find has nothing to put back.
        assert!(ChatSession::restore(&paths, "c1").is_err());

        std::fs::remove_dir_all(&paths.data).ok();
    }

    #[test]
    fn answer_salvage_survives_every_wrapper() {
        // Clean JSON.
        assert_eq!(extract_answer("{\"answer\":\"x\"}").as_deref(), Some("x"));
        // Fenced.
        assert_eq!(extract_answer("```json\n{\"answer\":\"x\"}\n```").as_deref(), Some("x"));
        // Prose before the JSON.
        assert_eq!(extract_answer("Here you go: {\"answer\":\"x\"}").as_deref(), Some("x"));
        // Broken after the answer string: salvage by hand.
        assert_eq!(extract_answer("{\"answer\":\"x\\\"ok\"}garbage").as_deref(), Some("x\"ok"));
        // Prose entirely: the text is the answer.
        assert_eq!(extract_answer("just text").as_deref(), Some("just text"));
        // Escapes decode.
        assert_eq!(extract_answer("{\"answer\":\"a\\nb\\u2014c\"}").as_deref(), Some("a\nb—c"));
    }

    #[test]
    fn the_answer_key_is_found_only_when_quoted() {
        assert_eq!(find_answer_value_start("{\"answer\":\"hi\"}"), Some(11));
        assert_eq!(find_answer_value_start("{\"answer\" : \"hi\"}"), Some(13));
        assert_eq!(find_answer_value_start("Here you go: {\"answer\":\"x\"}"), Some(24));
        // A prose echo of the schema must not count as the object's key.
        assert_eq!(find_answer_value_start("say \"answer\": text"), None);
    }

    #[test]
    fn parse_response_collects_sse_tokens_and_salvages_the_answer() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"answer\\\":\\\"hel\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"lo\\\"}\"}}]}\n\n\
                   data: [DONE]\n\n";
        let events = parse_response(sse, &[result("a.pdf", 2, "text")]).unwrap();
        let tokens: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                ChatEvent::Token { token } => Some(token.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tokens, vec!["{\"answer\":\"hel", "lo\"}"]);
        let done = events.iter().find_map(|event| match event {
            ChatEvent::Done { value } => Some(value),
            _ => None,
        });
        let done = done.unwrap();
        assert_eq!(done["answer"], "hello");
        assert_eq!(done["sources"][0]["filename"], "a.pdf");
        assert_eq!(done["sources"][0]["id"], "a.pdf:2:0", "a citation names its exact chunk");
    }

    #[test]
    fn parse_response_handles_a_plain_json_body() {
        let body = "{\"choices\":[{\"message\":{\"content\":\"{\\\"answer\\\":\\\"x\\\"}\"}}]}";
        let events = parse_response(body, &[]).unwrap();
        let done = events.iter().find_map(|event| match event {
            ChatEvent::Done { value } => Some(value),
            _ => None,
        });
        assert_eq!(done.unwrap()["answer"], "x");
    }

    fn turn(role: &str, content: &str) -> ChatTurn {
        ChatTurn {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn regenerate_drops_the_last_answer_but_keeps_its_question() {
        let history = vec![turn("user", "q1"), turn("assistant", "a1")];
        let again = thread(&history, "q1", true);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].content, "q1");
        assert_eq!(again[0].role, "user");

        let next = thread(&history, "q2", false);
        assert_eq!(next.len(), 3, "a new question carries the whole exchange");
        assert_eq!(next.last().unwrap().content, "q2");
    }

    #[test]
    fn only_the_most_recent_turns_travel() {
        let history: Vec<ChatTurn> = (0..20)
            .map(|i| turn(if i % 2 == 0 { "user" } else { "assistant" }, &format!("t{i}")))
            .collect();
        let recent = thread(&history, "now", false);
        assert_eq!(recent.len(), HISTORY_TURNS);
        assert_eq!(recent.last().unwrap().content, "now");
        assert_eq!(recent.first().unwrap().content, "t9", "the oldest turns fall off");
    }

    #[test]
    fn chat_refuses_a_cloud_endpoint_before_anything_is_sent() {
        let config = VisionConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "sk-secret".to_string(),
            model: "some-cloud-model".to_string(),
            timeout_secs: 5,
        };
        let error = match answer(&config, "what is the gradient?", &[], &[], false) {
            Ok(_) => panic!("a cloud endpoint must be refused"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("non-local"),
            "the guard must be the reason: {error:#}"
        );
    }

    #[test]
    fn a_truncated_unicode_escape_is_survived_not_a_panic() {
        // The four hex digits a \u escape needs run into the middle of the "é":
        // slicing the slice would abort the process, and salvage exists precisely
        // because model output arrives broken.
        let salvaged = extract_answer("{\"answer\":\"\\u000é tail").unwrap();
        assert!(salvaged.contains('é'), "{salvaged}");
    }

    /// The whole chat path against a running engine, in the order the GUI runs
    /// it: retrieve with the real index, then ask, then salvage.
    #[test]
    #[ignore = "live: asks the local engine a real question against the real index"]
    fn a_real_question_gets_a_grounded_answer() {
        let paths = crate::config::Paths::resolve().unwrap();
        let settings = crate::vision::VisionSettings::load(&paths).unwrap();
        let index = crate::store::Index::open(&paths.index).expect("index the corpus first");
        let mut encoder = crate::embed::Encoder::load(&paths).unwrap();
        let mut reranker = crate::embed::Reranker::load(&paths).ok();

        let question = "What is the maximum longitudinal slope of a runway?";
        let results = crate::search::search(
            &index,
            &mut encoder,
            reranker.as_mut(),
            question,
            crate::config::DEFAULT_K,
            None,
        )
        .unwrap();
        drop(encoder);
        drop(reranker);
        assert!(!results.is_empty(), "the corpus has to answer something");
        println!("--- retrieved {} passages ---", results.len());
        for result in &results {
            let preview: String = result.text.chars().take(60).collect();
            println!("[{} p.{}] {}", result.filename, result.page, preview);
        }

        let events = collect_stream(
            answer(&settings.config(), question, &[], &results, false).unwrap(),
        );
        let tokens: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                ChatEvent::Token { token } => Some(token.as_str()),
                _ => None,
            })
            .collect();
        let done = events
            .iter()
            .find_map(|event| match event {
                ChatEvent::Done { value } => Some(value.clone()),
                _ => None,
            })
            .expect("the turn must end in done, not an error");
        let answer = done["answer"].as_str().unwrap();
        println!("--- {} sse tokens, {} chars ---", tokens.len(), answer.len());
        println!("{answer}");

        assert!(
            tokens.len() > 1,
            "the engine must stream frames, not answer in one: {}",
            tokens.len()
        );
        assert!(!answer.trim().is_empty(), "an answer must reach the user");
        assert!(
            !answer.contains("\"answer\""),
            "the JSON wrapper must not survive salvage: {answer}"
        );
        assert_eq!(done["sources"].as_array().unwrap().len(), results.len());
    }

    /// Turn two: the earlier exchange travels, retrieval runs fresh for the new
    /// wording, and pressing again re-asks one question rather than two.
    #[test]
    #[ignore = "live: asks the local engine a follow-up against the real index"]
    fn a_follow_up_turn_carries_the_thread() {
        let paths = crate::config::Paths::resolve().unwrap();
        let settings = crate::vision::VisionSettings::load(&paths).unwrap();
        let index = crate::store::Index::open(&paths.index).expect("index the corpus first");
        let mut encoder = crate::embed::Encoder::load(&paths).unwrap();
        let mut reranker = crate::embed::Reranker::load(&paths).ok();

        let history = vec![
            turn(
                "user",
                "What is the maximum longitudinal slope of a runway?",
            ),
            turn(
                "assistant",
                "Code number 4 runways should not exceed 1.25% anywhere, and 0.8% in the \
                 first and last quarter of the runway length [1].",
            ),
        ];
        let follow_up = "And what does it recommend for a code number 3 runway?";
        let results = crate::search::search(
            &index,
            &mut encoder,
            reranker.as_mut(),
            follow_up,
            crate::config::DEFAULT_K,
            None,
        )
        .unwrap();
        drop(encoder);
        drop(reranker);

        let events = collect_stream(
            answer(&settings.config(), follow_up, &history, &results, false).unwrap(),
        );
        let done = events
            .iter()
            .find_map(|event| match event {
                ChatEvent::Done { value } => Some(value.clone()),
                _ => None,
            })
            .expect("the turn must end in done, not an error");
        let answer = done["answer"].as_str().unwrap();
        let tokens = events
            .iter()
            .filter(|event| matches!(event, ChatEvent::Token { .. }))
            .count();
        println!("--- {} sse tokens ---\n{answer}", tokens);

        assert!(tokens > 1, "the engine must stream frames");
        assert!(!answer.trim().is_empty(), "an answer must reach the user");
        assert!(
            !answer.contains("\"answer\""),
            "the JSON wrapper must not survive salvage: {answer}"
        );
        assert!(
            answer.contains('%') || answer.contains("not"),
            "a follow-up that ignores the thread is not an answer: {answer}"
        );

        let sent = thread(&history, follow_up, false);
        assert_eq!(sent.len(), 3, "both earlier turns travel with the new one");
        let answered = vec![turn("user", follow_up), turn("assistant", answer)];
        let again = thread(&answered, follow_up, true);
        assert_eq!(again.len(), 1, "a regenerate asks once, not twice");
        assert_eq!(again[0].content, follow_up);
    }
}
