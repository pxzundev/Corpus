# Run commands

Repo: `~/Developer/AI/mcp-servers/rag-mcp-rs`. Requires a Rust toolchain; the Tauri CLI is
at `~/.cargo/bin/cargo-tauri`. No bundler, no dev server — the UI is plain files under
`crates/gui/ui` embedded into the binary at build time.

## Data dir

Everything lives in one directory: `models/`, `index/`, `vision.json`, `captions.jsonl`,
`pdfium/`. macOS `~/Library/Application Support/Corpus`, Windows `%APPDATA%`,
Linux `~/.local/share`. Override with `RAG_DATA_DIR`.

```bash
# Isolated store for testing: never touches the real index, settings or caption cache
mkdir -p /tmp/corpustest
ln -sfn "$HOME/Library/Application Support/Corpus/models"  /tmp/corpustest/models
ln -sfn "$HOME/Library/Application Support/Corpus/pdfium"  /tmp/corpustest/pdfium  # only if testing captioning
RAG_DATA_DIR=/tmp/corpustest ./target/release/corpus-gui
```

```bash
# Index without models: first run downloads ~562 MB, so do it once up front
RAG_DATA_DIR=/tmp/corpustest ./target/release/corpus probe
```

## Dev

```bash
cargo run -p corpus-gui            # debug window
cargo tauri dev                 # same thing, via the Tauri CLI
cargo check -q --workspace      # fast compile check
```

Frontend edits do **not** hot-reload — `tauri::generate_context!` embeds assets, so
rebuild the binary. `tauri.conf.json` has no `devUrl` or `beforeDevCommand`.

## Release

```bash
cargo build --release --workspace
./target/release/corpus-gui        # window
./target/release/corpus --help     # CLI: probe | index | search | list
./target/release/corpus-mcp     # stdio MCP server
cargo tauri build --no-bundle   # build without bundling (bundle.active is still false)
```

## Tests

```bash
cargo test -q --workspace       # 50 core + 2 GUI Rust tests, 7 ignored, no env needed
(cd crates/gui && npm test)     # 48 jsdom UI tests — seconds, fastest loop for UI logic
(cd crates/gui && npm install)  # once, installs jsdom
```

Live tests — these actually call the vision engine or read real PDFs:

```bash
cargo test -p corpus-core --lib -- --ignored --nocapture caption_a_real_page   # render + caption one page
cargo test -p corpus-core --lib -- --ignored --nocapture probe_a_real_endpoint # synthetic image capability probe
cargo test -p corpus-core --lib -- --ignored --nocapture render_a_real_page    # PDFium only, no model
cargo test -p corpus-core --lib -- --ignored --nocapture measure_detection_against_a_real_corpus
# The chat path end to end: real retrieval, then a real answer from the engine.
cargo test -p corpus-core --lib -- --ignored --nocapture a_real_question
# Turn two: the thread travels and retrieval runs again for the new wording.
cargo test -p corpus-core --lib -- --ignored --nocapture a_follow_up_turn
```

All four default to this repo's `./docs`; override with `RAG_VISION_PDF`,
`RAG_RENDER_PDF`, `RAG_FIGURE_CORPUS`.

```bash
python3 scripts/mcp-smoke.py target/release/corpus-mcp "obstacle limitation surface"
```

MCP handshake, tool discovery, schema, live search, filename filter, `list_documents`.
Needs an existing index in the data dir.

## Indexing and search

```bash
./target/release/corpus index docs                     # text only
./target/release/corpus index docs --vision            # plus figure captions: seconds per figure page
./target/release/corpus search "OIS gradient" -k 5     # reranked
./target/release/corpus search "DER" --no-rerank --file "Aeropath - RNP AR Departure and EOSID Manual (v 2.1).pdf"
# --file is an exact filename, not a substring: a partial name answers "no passages found"
# rather than "no such document", so quote it in full
./target/release/corpus list
```

Source PDFs are in `./docs` (gitignored — copyrighted user data, not source).

## Environment variables

| Variable | Purpose |
| --- | --- |
| `RAG_DATA_DIR` | Moves models, index, settings, caption cache and PDFium lookup |
| `RAG_VISION_URL` | Vision endpoint; default `http://127.0.0.1:11234/v1` |
| `RAG_VISION_MODEL` | Vision model id; defaults to the mlx-serve model |
| `RAG_VISION_KEY` | Bearer token, if the endpoint needs one |
| `RAG_VISION_TIMEOUT` | Per-request timeout in seconds |
| `RAG_PDFIUM_PATH` | Where to load PDFium from, instead of `<data_dir>/pdfium` |

Vision endpoints must be loopback, a private range, or a `.local` name. Anything else is
refused before the image leaves the machine. The GUI's chat answers from the same endpoint:
each question retrieves passages first (optional per-document scope), the model answers only
from them, and its `[file p.45]` citations are clickable into the knowledge pane. The same
local-only rule guards that path (`chat_completion` → `vision::ensure_local_endpoint`). The
answer streams as `chat-event` events (token / done / error): the whole SSE body is read up
front over blocking ureq, so per-token latency is bounded by the engine. The frontend holds
`Thinking…` and reveals the answer in one go at `done`, because the tokens are fragments of
the JSON reply and half an answer on screen reads worse than the wait. Raise
`RAG_VISION_TIMEOUT` if the engine thinks slowly.

## Timing to expect

Debug and release are close — indexing 0.91s vs 0.73s, reranked search 0.72s vs 0.54s —
because ONNX Runtime is prebuilt and optimised. Use `--release` when the question is how a
full ingest feels. Text-only corpus index is about two minutes; captioning every figure
page is roughly 11–40 minutes the first time and 2.4s afterwards from `captions.jsonl`.

## Two things before you start

```bash
pgrep -fl corpus-gui                 # a second window reads fine but ingest races on the index files
tail -f /tmp/corpus-gui.log          # stderr of the window launched from a shell
```

There is no installer. `bundle.active` is `false` in `tauri.conf.json`, so nothing produces
a `.app`, `.dmg`, `.msi` or `.deb` — another machine builds from source, downloads 562 MB
of models on first query, and places an arch-matched `libpdfium` by hand or runs
text-only.
