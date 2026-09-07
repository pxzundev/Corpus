# Corpus — RAG knowledge base over MCP

Local-first retrieval-augmented answer engine in Rust. Index your PDF documents, search
them with fused lexical + dense retrieval and a reranker, chat with them through a desktop
GUI with clickable citations, or query them programmatically through an MCP server over
stdio. Nothing leaves the machine: models run locally, the index lives in one data
directory, and any LLM endpoint the app talks to is restricted to loopback or private
network addresses before an image or question is sent.

Corpus is built for corpora that live on your own disk — regulation manuals, standards,
papers — where answers must cite the page they came from. Embedding, reranking and
figure captioning all run locally under ONNX Runtime: the models, about 562 MB in
total, are downloaded automatically on first run and cached in the data directory, so
after that everything is offline and instant. There is no cloud service, no account, no
telemetry.

## Layout

The workspace has four crates:

| Crate | Binary | Role |
| --- | --- | --- |
| `crates/core` (`corpus-core`) | library | The engine: ingest, chunking, embeddings, BM25, reranking, figure detection, vision captioning, chat, index store |
| `crates/cli` (`corpus`) | `corpus` | CLI: `probe`, `index`, `search`, `list` |
| `crates/mcp` (`corpus-mcp`) | `corpus-mcp` | MCP server over stdio exposing `search_docs` and `list_documents` |
| `crates/gui` (`corpus-gui`) | `corpus-gui` | Tauri 2 desktop app: browse the index, add and remove documents, inspect chunks, chat |

No dev server — the UI is plain files under `crates/gui/ui` embedded into the
binary at build time.

## Installing on macOS

Two ways in. The easy way: download `Corpus_0.1.0_aarch64.dmg` from the
[releases page](https://github.com/pxzundev/Corpus/releases), open it, and drag
`Corpus.app` into Applications. The app is self-contained for MCP: it carries
`corpus-mcp` and places it in the data folder on first launch (see
[Using the MCP server from a client](#using-the-mcp-server-from-a-client) below). The
CLI `corpus` binary ships separately in the same release's zip if you also want
command-line indexing. To build from source instead — a few minutes:

```bash
# 1. Install a recent stable Rust toolchain (the workspace uses edition 2024)
curl --proto '=https' --tlsv1.2 https://sh.rustup.rs -sSf | sh

# 2. Clone and build
git clone https://github.com/pxzundev/Corpus.git
cd Corpus
cargo build --release --workspace   # CLI + MCP server + the GUI binary
cargo tauri build                   # also Corpus.app and a .dmg in target/release/bundle

# 3. Run
./target/release/corpus-gui                   # the desktop window (raw binary)
open target/release/bundle/macos/Corpus.app   # or the bundled app
```

On first run the embedding and reranking models download automatically (~562 MB in
total; ONNX Runtime ships as prebuilt binaries). Warm them before opening the window
with `./target/release/corpus probe`. After that the models are cached in the data
directory and everything runs offline.

For figure captioning you also need PDFium: grab an arch-matched binary from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) —
`pdfium-mac-arm64.tgz` on Apple silicon, `pdfium-mac-x64.tgz` on Intel — and put
`libpdfium.dylib` in `~/Library/Application Support/Corpus/pdfium/`, or point
`RAG_PDFIUM_PATH` at it. Without PDFium the app still works text-only.

Optional: `cargo install tauri-cli` for `cargo tauri dev` / `cargo tauri build`, and
Node + npm for the UI tests (`npm install` once, then `npm test`).

To pick up changes later: `git pull && cargo build --release --workspace`.

## Installing on Windows

Build from source, or grab the release zip: `Corpus-v0.1.0-windows-x64.zip` on the
[releases page](https://github.com/pxzundev/Corpus/releases) contains `corpus-gui.exe`,
`corpus.exe` and `corpus-mcp.exe` plus `install.ps1`, and needs no build. These
steps assume PowerShell. (The zip's `install.ps1` can place `corpus` and `corpus-mcp`
for you without any build — see
[Using the MCP server from a client](#using-the-mcp-server-from-a-client).)

```powershell
# 1. Install a recent stable Rust toolchain (the workspace uses edition 2024)
winget install Rustlang.Rustup     # or download rustup-init.exe from https://rustup.rs

# 2. Clone and build
git clone https://github.com/pxzundev/Corpus.git
cd Corpus
cargo build --release --workspace

# 3. Run
.\target\release\corpus-gui.exe     # the desktop window
.\target\release\corpus.exe --help  # the CLI: probe | index | search | list
```

Windows has no bundled `.app`, but the GUI still self-connects MCP: it looks for
`corpus-mcp.exe` beside itself and places it in `%APPDATA%\Corpus\bin` on first launch,
so any folder holding all the exes works, and the MCP sheet shows the JSON to paste.

On first run the embedding and reranking models download automatically (~562 MB in
total). Warm them before opening the window with `.\target\release\corpus.exe probe`.
The window needs the WebView2 runtime, preinstalled on Windows 11 (on Windows 10,
install it from Microsoft if it is missing).

For figure captioning you also need PDFium: download `pdfium-windows-x64.tgz` from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) and put
`libpdfium.dll` in `%APPDATA%\Corpus\pdfium\`, or point `RAG_PDFIUM_PATH` at it.
Without PDFium the app still works text-only. The data directory lives at
`%APPDATA%\Corpus`.

To pick up changes later: `git pull && cargo build --release --workspace`.

## Installing and running, plain English

The short version, for anyone who just wants the app working:

### macOS

1. **Install.** Download `Corpus_0.1.0_aarch64.dmg` from the
   [releases page](https://github.com/pxzundev/Corpus/releases), open it, and drag
   `Corpus.app` into Applications. On first open, macOS asks to confirm the app
   (right-click → Open, once) because it is not signed with a developer certificate.
   That is the whole install — no scripts, no Terminal, nothing else.
2. **Run the app.** Double-click Corpus. The embedding models (~562 MB) download
   automatically on this first launch. Drag PDFs or folders into the window to index
   them. The app also quietly copies its built-in `corpus-mcp` server into its data
   folder (`~/Library/Application Support/Corpus/bin/corpus-mcp`) on this same first
   launch — it just happens; there is nothing to see or do.
3. **Connect the harness.** Click the **MCP** button in the Chat header. A sheet
   shows the exact `mcpServers` JSON block for your machine, with the real server
   path already filled in. Click **Copy JSON**, paste the block into your client's
   `mcpServers` config (pi, Claude Desktop, and similar clients), and restart the
   client. Its `search_docs` and `list_documents` tools now answer from the
   documents you indexed in step 2 — no engine, API key or model is needed on this
   side.
4. **Optional: chat and vision.** These need a model, so run your own local engine
   (LM Studio or similar) and point the app at it once in the Vision sheet.
5. **Uninstall, someday.** Drag `Corpus.app` to the Trash, delete
   `~/Library/Application Support/Corpus`, and remove the pasted block from your
   client's config. Everything the app ever created lives in those two places.

### Windows

1. **Get the binaries.** Download the release zip (`Corpus-v0.1.0-windows-x64.zip`)
   from the [releases page](https://github.com/pxzundev/Corpus/releases) and unzip it,
   or build from source following [Installing on Windows](#installing-on-windows)
   above. Either way you end up with a folder that contains `corpus-gui.exe`,
   `corpus.exe` and `corpus-mcp.exe`.
2. **Put the exe files somewhere.** Any folder works — say `Documents\Corpus` — or
   run `install.ps1` for the CLI tools. No installation step is required for the
   GUI: a folder with the exes in it is enough.
3. **Run the app.** Double-click `corpus-gui.exe`. The WebView2 runtime it needs is
   preinstalled on Windows 11 (on Windows 10, install it once from Microsoft). The
   ~562 MB of models download on first launch, and you drag PDFs into the window to
   index them.
4. **The MCP connection happens automatically.** The app looks for
   `corpus-mcp.exe` beside itself, copies it into `%APPDATA%\Corpus\bin`, and that
   is where it stays — the same "you do nothing" behavior as macOS.
5. **Connect the harness.** Click the **MCP** button in the Chat header — the sheet
   shows the exact JSON with the real path. **Copy JSON**, paste it into your
   client's `mcpServers` config, restart the client.
6. **Uninstall.** Delete the exe folder, delete `%APPDATA%\Corpus` (models, index,
   the copied MCP server — everything lives there), and remove the pasted block
   from your client's config.

The full technical detail for both platforms is in the sections above and in
[Using the MCP server from a client](#using-the-mcp-server-from-a-client) below.

## Data dir

Everything lives in one directory: `models/`, `index/`, `vision.json`, `captions.jsonl`,
`pdfium/`, and `bin/` (the MCP server, copied here by the GUI on first launch). macOS
`~/Library/Application Support/Corpus`, Windows `%APPDATA%`, Linux `~/.local/share`.
Override with `RAG_DATA_DIR`.

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
cargo tauri dev                    # same thing, via the Tauri CLI
cargo check -q --workspace         # fast compile check
```

Frontend edits do **not** hot-reload — `tauri::generate_context!` embeds assets, so
rebuild the binary. `tauri.conf.json` has no `devUrl` or `beforeDevCommand`.

## Release

```bash
cargo build --release --workspace
./target/release/corpus-gui        # window
./target/release/corpus --help     # CLI: probe | index | search | list
./target/release/corpus-mcp        # stdio MCP server
cargo tauri build --no-bundle      # build the GUI binary without bundling
```

## Tests

```bash
cargo test -q --workspace          # 50 core + 2 GUI Rust tests, 7 ignored, no env needed
(cd crates/gui && npm test)        # 48 jsdom UI tests — seconds, fastest loop for UI logic
(cd crates/gui && npm install)     # once, installs jsdom
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

Source PDFs live wherever you keep them; pass the directory to `index`.

## Chat in the GUI

The window's chat answers from the same vision endpoint: each question retrieves passages
first (optional per-document scope), the model answers only from them, and its
`[file p.45]` citations are clickable into the knowledge pane. The same local-only rule
guards that path (`chat_completion` → `vision::ensure_local_endpoint`). The answer streams
as `chat-event` events (token / done / error): the whole SSE body is read up front over
blocking ureq, so per-token latency is bounded by the engine. The frontend holds
`Thinking…` and reveals the answer in one go at `done`, because the tokens are fragments
of the JSON reply and half an answer on screen reads worse than the wait. Raise
`RAG_VISION_TIMEOUT` if the engine thinks slowly.

## Connecting an inference engine (LM Studio and friends)

Corpus ships no model server. Chat in the GUI and figure captioning both talk to an
OpenAI-compatible HTTP server that you already run locally — LM Studio, mlx-serve,
llama.cpp's `llama-server`, Ollama's OpenAI-compatible endpoint — anything that answers
`/v1/chat/completions` (and, for captioning, accepts base64 images in the OpenAI
vision format).

Point Corpus at your engine in the GUI's vision settings (the Endpoint / Model / API key
sheet); the values persist in `<data_dir>/vision.json`. With LM Studio, load a
vision-language model, start the server on its default port, and fill in:

- **Endpoint** `http://127.0.0.1:1234/v1` (LM Studio's default; mlx-serve uses
  `:11234/v1`, llama-server `:8080/v1`)
- **Model** the exact model id your engine reports
- **API key** only if your server requires one (LM Studio does not by default)

Then click **Test the model**: Corpus sends a real synthetic image and shows the reply,
so you know the endpoint genuinely accepts images instead of trusting what it
advertises. Captioning every figure page in an index takes minutes to hours the first
time; afterwards captions are cached in `<data_dir>/captions.jsonl`.

Chat uses the same endpoint and settings, so one engine serves both — a text-only model
is enough for chat, a vision-language model for captioning. The environment variables
below override the saved settings when a command needs different values.

One rule guards both paths: the endpoint must be loopback, a private range, or a
`.local` name. Anything else — including the public internet — is refused before the
image or question leaves the machine. An engine on another machine in your home network
works; a hosted API does not.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `RAG_DATA_DIR` | Moves models, index, settings, caption cache and PDFium lookup |
| `RAG_VISION_URL` | Vision and chat endpoint (OpenAI-compatible); default `http://127.0.0.1:11234/v1` |
| `RAG_VISION_MODEL` | Model id, as your engine reports it; set in the GUI's vision settings or here |
| `RAG_VISION_KEY` | Bearer token, if the endpoint needs one |
| `RAG_VISION_TIMEOUT` | Per-request timeout in seconds |
| `RAG_PDFIUM_PATH` | Where to load PDFium from, instead of `<data_dir>/pdfium` |

Vision endpoints must be loopback, a private range, or a `.local` name. Anything else is
refused before the image leaves the machine.

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

## Using the MCP server from a client

`corpus-mcp` speaks MCP over stdio and exposes two tools: `search_docs` (fused lexical +
dense search, optional rerank and exact-filename filter) and `list_documents`. It needs
no inference engine — both tools run entirely against the local index, so any MCP client
connects to it as-is, with no model, API key or engine setup on Corpus's side.

### From the GUI app (nothing to do)

The `.app` bundle carries `corpus-mcp` inside it. On first launch the GUI places the
server in the data folder (`~/Library/Application Support/Corpus/bin/corpus-mcp`), so
clients can point at a stable path and uninstalling stays "delete the app, delete the
data folder". Click the **MCP** button in the Chat header: the sheet shows exactly this
JSON for your machine, with a **Copy JSON** button:

```json
{
  "mcpServers": {
    "corpus": {
      "transport": "stdio",
      "command": "/Users/you/Library/Application Support/Corpus/bin/corpus-mcp",
      "args": [],
      "enabled": true,
      "timeout": 180
    }
  }
}
```

On Windows the command is `%APPDATA%\\Corpus\\bin\\corpus-mcp.exe`; on Linux,
`~/.local/share/Corpus/bin/corpus-mcp` (the sheet always shows the absolute form). If
you set `RAG_DATA_DIR`, the sheet shows the server inside that directory instead.

### CLI-only installs (source build, or the release zip)

The folder-picking installer still exists for users who want just the CLI binaries:

```bash
# macOS and Linux: asks where to put them, copies, prints the JSON below
curl -fsSL https://raw.githubusercontent.com/pxzundev/Corpus/main/scripts/install.sh | bash
# or, from a repo checkout or the release zip:
bash scripts/install.sh
```

```powershell
# Windows: same, folder prompt included
.\scripts\install.ps1
```

Then add the printed block to your client's `mcpServers` config (pi, Claude Desktop,
and other clients that follow the convention) — here with one unrelated server to show
the shape clients ignore extra entries by:

```json
{
  "mcpServers": {
    "local-web-search": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-puppeteer"]
    },
    "corpus": {
      "transport": "stdio",
      "command": "/usr/local/bin/corpus-mcp",
      "args": [],
      "enabled": true,
      "timeout": 180
    }
  }
}
```

The installer fills in `command` with the folder you chose; clients that only want
`command` and `args` ignore the extra fields. The client's `search_docs` calls answer
from whatever documents you indexed; add documents through the GUI (or the CLI's
`corpus index`) first.

## Uninstalling

Corpus creates nothing outside two places, so removal is two deletions:

- The app: drag `Corpus.app` to the Trash (macOS). On Windows or Linux, delete the
  folder you installed the binaries into.
- The data folder: `~/Library/Application Support/Corpus` (macOS), `%APPDATA%\Corpus`
  (Windows), `~/.local/share/Corpus` (Linux). The embedding models, the index, the
  caption cache, the vision settings, the PDFium build, and the MCP server the GUI
  placed in `bin/` all live inside it.

Then remove the `corpus` entry from your client's `mcpServers` config. No caches,
launch agents or shell rc edits exist anywhere else.

## Notes on distribution

`bundle.active` is `true` with targets `app` and `dmg`, so on macOS `cargo tauri build`
produces `Corpus.app` and `Corpus_0.1.0_aarch64.dmg` under `target/release/bundle/`.
`corpus-mcp` rides inside the bundle as a resource — the build script copies the freshly
built binary into `crates/gui/resources/` (gitignored) and the GUI places it in the data
folder on first launch, so the app is self-contained: no separate CLI install for MCP
connectivity. The bundle is ad-hoc signed (no developer certificate), so Gatekeeper
asks for a confirm on first open — right-click → Open, once. Windows ships as a
folder-of-exes zip from the release (see above); Linux has no installer yet —
build from source as above.

## License

MIT — see `Cargo.toml`.
