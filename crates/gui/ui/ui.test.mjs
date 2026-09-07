/* Render crates/gui/ui in jsdom with the Rust commands mocked, and drive the
 * paths that matter: initial load, document selection, removal confirmation,
 * ingest progress, and the guarantee that file text never becomes markup.
 * Run: node --test crates/gui/ui/ui.test.mjs   (needs jsdom on NODE_PATH)
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { JSDOM } from "jsdom";

const here = dirname(fileURLToPath(import.meta.url));
const html = readFileSync(join(here, "index.html"), "utf8");
const script = readFileSync(join(here, "app.js"), "utf8");
const css = readFileSync(join(here, "styles.css"), "utf8");

const STATUS = {
  chunks: 2619,
  documents: 2,
  model: "bge-base-en-v1.5",
  dimension: 768,
  chunkerVersion: 1,
  indexPath: "/tmp/index",
  modelsPath: "/tmp/models",
  indexPresent: true,
};

const DOCUMENTS = [
  {
    filename: "Doc-8168-Vol1.pdf",
    path: "/docs/Doc-8168-Vol1.pdf",
    chunks: 1155,
    pages: 228,
    figures: 12,
    sourceAvailable: true,
  },
  {
    filename: "Annex-14.pdf",
    path: "/docs/Annex-14.pdf",
    chunks: 1464,
    pages: 228,
    figures: 0,
    sourceAvailable: false,
  },
];

// What the Rust side returns when picked documents contain drawings: a token
// and a cost estimate, never the paths themselves.
const PENDING = {
  token: "9f3a1c",
  documents: 2,
  pages: 268,
  figures: 34,
  model: "qwen3-vl",
  captionMinutesLow: 2.3,
  captionMinutesHigh: 8.5,
  pdfium: true,
  captionDefault: false,
};

const VISION = {
  enabled: true,
  baseUrl: "http://127.0.0.1:11234/v1",
  model: "qwen3-vl",
  apiKeySet: true,
  pdfium: true,
};

// Malicious markup on purpose: if any of this reaches innerHTML the test fails.
const MARKUP = `<img src=x onerror="window.__pwned=1">before`;

function page(calls, options = {}) {
  const dom = new JSDOM(html, { runScripts: "outside-only", pretendToBeVisual: true });
  // jsdom has no scrollIntoView; a citation click calls it to centre the card.
  dom.window.Element.prototype.scrollIntoView = () => {};
  // jsdom's HTMLInputElement.select() may not move the DOM selection; the
  // harness mirrors the browser's semantics so the pre-select is testable.
  dom.window.HTMLInputElement.prototype.select = function () {
    this.selectionStart = 0;
    this.selectionEnd = this.value.length;
  };
  const emitted = [];
  // The chats the backend still has on disk: new adds one, delete drops one, so
  // the picker cannot keep offering a chat that was just removed.
  const saved = new Set((options.sessions ?? []).map((session) => session.id));
  // Delete is a soft delete here, mirroring the backend's trash folder:
  // restore moves a chat out of this set and back into saved.
  const trashed = new Set();
  // Chats the GUI created during the run, newest first — the backend lists
  // them again on every loadSessions, with the model-chosen title and turn
  // count once an exchange has been persisted.
  const created = [];
  // A command that answers in the same tick can never be caught with its busy
  // indicator up, so the slow paths are delayed per command name.
  const later = (value, ms) =>
    ms ? new Promise((resolve) => setTimeout(() => resolve(value), ms)) : Promise.resolve(value);
  dom.window.__TAURI__ = {
    core: {
      invoke(name, args) {
        calls.push({ name, args });
        if (name === "browse") {
          return later(
            {
              document: args.document ?? null,
              total: args.document ? 3 : 0,
              offset: 0,
              items: args.document
                ? [
                    { id: "a1b2c3d4e5f60718", page: 45, words: 300, text: MARKUP, figure: true },
                    { id: "ff00", page: -1, words: 12, words_: 0, text: "plain text", figure: false },
                  ]
                : [],
              documents: options.documents ?? DOCUMENTS,
              status: STATUS,
            },
            options.slow?.browse,
          );
        }
        if (name === "remove_document") return Promise.resolve(1155);
        if (name === "add_documents")
          return later(
            options.addOutcome ?? {
              indexed: ["a.pdf", "b.pdf"],
              unchanged: [],
              newChunks: 120,
              totalChunks: 2739,
              totalDocuments: 4,
              cancelled: false,
              figures: 0,
              captionsMade: 0,
              captionFailures: 0,
            },
            options.slow?.add,
          );
        if (name === "commit_ingest")
          return Promise.resolve({
            indexed: ["a.pdf"],
            unchanged: [],
            newChunks: 60,
            totalChunks: 2679,
            totalDocuments: 3,
            cancelled: false,
            figures: 34,
            captionsMade: args.caption ? 34 : 0,
            captionFailures: options.captionFailures ?? 0,
          });
        if (name === "vision_settings") return Promise.resolve(options.vision ?? VISION);
        if (name === "vision_models") {
          return options.visionModelsError
            ? Promise.reject(new Error(options.visionModelsError))
            : Promise.resolve(options.visionModels ?? ["glm-5.2", "qwen3-vl"]);
        }
        if (name === "save_vision_settings")
          return Promise.resolve({
            enabled: args.enabled ?? false,
            baseUrl: args.baseUrl ?? "",
            model: args.model ?? "",
            // Mirrors the backend: an empty key leaves the stored one in place.
            apiKeySet: Boolean(args.apiKey) || Boolean(options.vision?.apiKeySet),
            pdfium: options.vision?.pdfium ?? true,
          });
        if (name === "mcp_config")
          return Promise.resolve({
            command: "/tmp/data/bin/corpus-mcp",
            json: JSON.stringify(
              {
                mcpServers: {
                  corpus: {
                    transport: "stdio",
                    command: "/tmp/data/bin/corpus-mcp",
                    args: [],
                    enabled: true,
                    timeout: 180,
                  },
                },
              },
              null,
              2,
            ),
          });
        if (name === "test_vision")
          return options.testVisionError
            ? Promise.reject(new Error(options.testVisionError))
            : later(
                options.testVision ?? "A climb profile with a 4.5% gradient.",
                options.slow?.vision,
              );
        if (name === "open_source") return Promise.resolve(null);
        if (name === "chat_completion") {
          const question = args.question;
          // The backend persists the exchange to the session it names, and a
          // first turn names the chat from its topic.
          const target =
            created.find((session) => session.id === args.sessionId) ??
            (options.sessions ?? []).find((session) => session.id === args.sessionId);
          if (target) {
            if (target.turns === 0) target.title = options.inferredTitle ?? target.title;
            target.turns = (target.turns ?? 0) + 2;
          }
          if (options.chatError) {
            return Promise.reject(new Error(options.chatError));
          }
          const answer =
            options.chat?.answer ??
            "The gradient is **4.5%** [Doc-8168-Vol1.pdf p.45].\n\n- one\n- two\n\n```txt\ncode\n```";
          const sources = options.chat?.sources ?? [
            {
              id: "a1b2c3d4e5f60718",
              filename: "Doc-8168-Vol1.pdf",
              page: 45,
              figure: true,
              score: 0.81,
              kind: "rerank",
            },
          ];
          const handler = emitted.find((entry) => entry.event === "chat-event")?.handler;
          const tokens = options.chat?.tokens ?? ["The gradient is ", "**4.5%**"];
          const finish = () =>
            handler?.({ payload: { kind: "done", value: { answer, sources } } });
          const ms = options.slow?.chat;
          if (ms) {
            // Tokens land spread across the wait, so a test can look between the
            // first token and the answer the app finally commits.
            tokens.forEach((token, i) =>
              setTimeout(
                () => handler?.({ payload: { kind: "token", token } }),
                Math.round((ms * (i + 1)) / (tokens.length + 1)),
              ),
            );
            setTimeout(finish, ms);
            return later(null, ms);
          }
          for (const token of tokens) handler?.({ payload: { kind: "token", token } });
          finish();
          return Promise.resolve(null);
        }
        if (name === "list_chat_sessions")
          return Promise.resolve([
            ...created,
            ...(options.sessions ?? []).filter((session) => saved.has(session.id)),
          ]);
        if (name === "new_chat_session") {
          const session = options.newSession ?? {
            id: "new-1",
            title: "New chat",
            created_at: "",
            updated_at: "",
            model: "qwen3-vl",
            filename: null,
            passages: 6,
            turns: [],
            sources: [],
            state: "active",
          };
          saved.add(session.id);
          created.unshift({ id: session.id, title: session.title, turns: 0 });
          return Promise.resolve(session);
        }
        if (name === "load_chat_session")
          return Promise.resolve(
            options.loadedSession ?? {
              id: args.id,
              title: options.sessions?.find((s) => s.id === args.id)?.title ?? "Saved chat",
              created_at: "",
              updated_at: "",
              model: "qwen3-vl",
              filename: null,
              passages: 6,
              turns: options.sessionTurns?.[args.id] ?? [],
              sources: options.sessionSources?.[args.id] ?? [],
              state: "active",
            },
          );
        if (name === "rename_chat_session") {
          const session = options.sessions?.find((item) => item.id === args.id);
          if (session) session.title = args.title;
          const fresh = created.find((item) => item.id === args.id);
          if (fresh) fresh.title = args.title;
          return Promise.resolve(null);
        }
        if (name === "delete_chat_session") {
          saved.delete(args.id);
          trashed.add(args.id);
          const fresh = created.find((item) => item.id === args.id);
          if (fresh) created.splice(created.indexOf(fresh), 1);
          return Promise.resolve(null);
        }
        if (name === "restore_chat_session") {
          if (!trashed.has(args.id)) return Promise.reject(new Error(`trashed chat '${args.id}' does not exist`));
          trashed.delete(args.id);
          saved.add(args.id);
          return Promise.resolve({
            id: args.id,
            title: options.sessions?.find((s) => s.id === args.id)?.title ?? "Saved chat",
            created_at: "",
            updated_at: "",
            model: "qwen3-vl",
            filename: null,
            passages: 6,
            turns: options.sessionTurns?.[args.id] ?? [],
            sources: options.sessionSources?.[args.id] ?? [],
            state: "active",
          });
        }
        return Promise.resolve(null);
      },
    },
    event: {
      listen(event, handler) {
        emitted.push({ event, handler });
        return Promise.resolve(() => {});
      },
    },
  };
  dom.window.eval(script);
  return { dom, emitted };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 30));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const doc = (dom) => dom.window.document;
const text = (dom, selector) => doc(dom).querySelector(selector)?.textContent?.trim();

test("status line reports counts, model, and dimensions", async () => {
  const { dom } = page([]);
  await tick();
  assert.match(text(dom, "#stats"), /2,619 chunks · 2 documents/);
  assert.match(text(dom, "#stats"), /bge-base-en-v1\.5 768-dim/);
});

test("documents render with chunk and page counts, and a remove control", async () => {
  const { dom } = page([]);
  await tick();
  const rows = doc(dom).querySelectorAll(".doc");
  assert.equal(rows.length, 2);
  assert.match(rows[0].textContent, /1,155 chunks · 228 pages/);
  assert.equal(
    rows[0].querySelectorAll(".icon-btn").length,
    2,
    "open + remove when the source file is present",
  );
  assert.equal(rows[1].querySelectorAll(".icon-btn").length, 1, "remove only, no open");
  assert.equal(rows[1].querySelector(".doc-missing").textContent, "original not found");
  // The document whose source file disappeared says so instead of offering open.
  assert.match(rows[1].textContent, /original not found/);
});

test("document text is never interpreted as HTML", async () => {
  const { dom, calls } = page([]);
  await tick();
  doc(dom)
    .querySelector(".doc-main")
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(dom.window.__pwned, undefined, "onerror handler must not run");
  const images = doc(dom).querySelectorAll(".chunk-text img");
  assert.equal(images.length, 0, "no injected <img> in chunk text");
  assert.equal(doc(dom).querySelector(".chunk-text").textContent, MARKUP);
  void calls;
});

test("selecting a document opens its passages in the knowledge modal", async () => {
  const { dom } = page([]);
  await tick();
  doc(dom)
    .querySelector(".doc-main")
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(doc(dom).querySelector("#knowledge-dialog").open, true, "the modal opens");
  assert.equal(text(dom, "#chunks-heading"), "Doc-8168-Vol1.pdf", "the title is just the file");
  assert.match(text(dom, "#chunk-count"), /2 of 3 passages/);
  assert.match(doc(dom).querySelector(".chunk-page").textContent, /page 45/);

  click(dom, "#knowledge-close");
  await tick();
  assert.equal(doc(dom).querySelector("#knowledge-dialog").open, false, "the close button exits");
});

test("filtering re-queries with the term and reports no matches", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  doc(dom)
    .querySelector(".doc-main")
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();

  const input = doc(dom).querySelector("#chunk-filter");
  input.value = "visual";
  input.dispatchEvent(new dom.window.Event("input", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 400));

  const browse = calls.filter((call) => call.name === "browse").at(-1);
  assert.equal(browse.args.filter, "visual");
});

test("removal asks for confirmation before touching the index", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();

  const remove = [...doc(dom).querySelectorAll(".doc")[0].querySelectorAll(".icon-btn")].at(-1);
  remove.dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();

  assert.equal(doc(dom).querySelector("#confirm-remove").open, true, "dialog opens first");
  assert.equal(calls.some((call) => call.name === "remove_document"), false, "not yet invoked");
  assert.match(text(dom, "#confirm-body"), /1,155 chunks across 228 pages/);

  doc(dom)
    .querySelector("#confirm-ok")
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(calls.some((call) => call.name === "remove_document"), true);
  assert.equal(doc(dom).querySelector("#confirm-remove").open, false);
});

test("removal message names the document being removed", async () => {
  const { dom } = page([]);
  await tick();
  const remove = [...doc(dom).querySelectorAll(".doc")[0].querySelectorAll(".icon-btn")].at(-1);
  remove.dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.match(text(dom, "#confirm-body"), /Doc-8168-Vol1\.pdf/);
  assert.match(text(dom, "#confirm-remove"), /original file on disk is not touched/);
});

test("cancelling removal does not call the command", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  const remove = [...doc(dom).querySelectorAll(".doc")[0].querySelectorAll(".icon-btn")].at(-1);
  remove.dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  doc(dom)
    .querySelector("#confirm-cancel")
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(calls.some((call) => call.name === "remove_document"), false);
  assert.equal(doc(dom).querySelector("#confirm-remove").open, false);
});

test("ingest progress updates the activity row and re-reads the index", async () => {
  const calls = [];
  const { dom, emitted } = page(calls);
  await tick();
  const progress = emitted.find((entry) => entry.event === "ingest-progress").handler;

  progress({
    payload: { stage: "extracting", done: 1, total: 2, current: "a.pdf" },
  });
  await tick();
  const row = doc(dom).querySelector("#activity");
  assert.equal(row.hidden, false);
  assert.equal(doc(dom).querySelector("#activity-meter").getAttribute("aria-valuenow"), "50");
  assert.match(text(dom, "#activity-text"), /Reading 1 of 2 files — a\.pdf/);
  assert.equal(doc(dom).querySelector("#add-files").disabled, true, "no concurrent ingest");

  progress({
    payload: { stage: "scanning", phase: "document", done: 2, total: 4, current: "Annex-14.pdf" },
  });
  await tick();
  assert.match(
    text(dom, "#activity-text"),
    /Counting figures in 2 of 4 documents — Annex-14\.pdf/,
    "the wait before the caption question is named",
  );

  // Text extraction cannot report where it is, so it says what it is about to
  // read and leaves the meter alone rather than running it backwards.
  progress({
    payload: { stage: "scanning", phase: "text", done: 0, total: 958, current: "Annex-14.pdf" },
  });
  await tick();
  assert.match(text(dom, "#activity-text"), /Reading the text of Annex-14\.pdf — 958 pages/);
  assert.equal(doc(dom).querySelector("#activity-meter").getAttribute("aria-valuenow"), "50");

  progress({
    payload: { stage: "scanning", phase: "pages", done: 320, total: 958, current: "Annex-14.pdf" },
  });
  await tick();
  assert.match(text(dom, "#activity-text"), /page 320 of 958/);
  assert.equal(doc(dom).querySelector("#activity-meter").getAttribute("aria-valuenow"), "33");

  progress({ payload: { stage: "embedding", done: 600, total: 1200, current: "b.pdf" } });
  await tick();
  assert.match(text(dom, "#activity-text"), /Embedding 600 of 1,200 chunks/);

  progress({
    payload: { stage: "preparing", done: 0, total: 0, current: "embedding model", phase: null },
  });
  await tick();
  assert.match(text(dom, "#activity-text"), /Loading the embedding model/);

  progress({ payload: { stage: "saving", done: 0, total: 0, current: "" } });
  await tick();
  assert.equal(doc(dom).querySelector("#activity-meter").getAttribute("aria-valuenow"), "100");

  await new Promise((resolve) => setTimeout(resolve, 900));
  assert.ok(calls.filter((call) => call.name === "browse").length > 1, "index re-read");
  assert.equal(doc(dom).querySelector("#add-files").disabled, false);
});

test("the activity row is the only busy signal and is not in the top bar", async () => {
  const calls = [];
  const { dom } = page(calls);
  assert.equal(doc(dom).querySelector("#progress"), null, "no strip under the header");
  const row = doc(dom).querySelector("#activity");
  assert.equal(doc(dom).querySelector(".topbar #activity"), null);
  assert.equal(doc(dom).querySelector(".pane-docs #activity"), row);
  await tick();
  assert.equal(row.hidden, true, "quiet once the index is read");

  doc(dom)
    .querySelector("#add-files")
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  assert.equal(row.hidden, false, "the picker delay is announced immediately");
  assert.match(text(dom, "#activity-text"), /Choose documents to index/);
  assert.equal(doc(dom).querySelector("#activity-meter").style.width, "0%", "no invented count");
  await tick();
});

test("a wait the app cannot measure counts seconds instead of looking stalled", async () => {
  const { dom, emitted } = page([], { slow: { add: 4000 } });
  await tick();
  const progress = emitted.find((entry) => entry.event === "ingest-progress").handler;

  // While a native picker is open the user is the slow part, and a timer on
  // their name reads as a fault.
  click(dom, "#add-files");
  await sleep(1200);
  assert.equal(text(dom, "#activity-text"), "Choose documents to index");

  progress({
    payload: { stage: "scanning", phase: "text", done: 0, total: 958, current: "Annex-14.pdf" },
  });
  await sleep(1200);
  assert.match(
    text(dom, "#activity-text"),
    /Reading the text of Annex-14\.pdf — 958 pages \(1s\)/,
    "text extraction reports no position, so it reports elapsed time",
  );
});

test("reading passages spins while the read is in flight", async () => {
  const calls = [];
  const { dom } = page(calls, { slow: { browse: 700 } });
  // The startup read is the same wait, so it gets the indicator too.
  await sleep(60);
  assert.equal(doc(dom).querySelector("#activity").hidden, false, "startup read");
  assert.match(text(dom, "#activity-text"), /Reading the index/);
  await sleep(900);

  click(dom, ".doc-main");
  await sleep(320);
  const row = doc(dom).querySelector("#chunks-activity");
  assert.equal(row.hidden, false, "the pane that is filling says so");
  assert.match(text(dom, "#chunks-activity-text"), /Loading passages/);
  await sleep(700);
  assert.equal(row.hidden, true, "quiet once the passages are on screen");
});

test("a quick passage read never flashes the indicator", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  click(dom, ".doc-main");
  await sleep(60);
  assert.equal(
    doc(dom).querySelector("#chunks-activity").hidden,
    true,
    "no flash for a read that lands in a frame",
  );
  assert.ok(doc(dom).querySelectorAll("#chunk-list .chunk").length, "passages did arrive");
});

test("testing the vision model spins while the image is in flight", async () => {
  const calls = [];
  const { dom } = page(calls, { slow: { vision: 700 } });
  await tick();
  click(dom, "#vision-open");
  click(dom, "#vision-test");
  assert.equal(doc(dom).querySelector("#vision-spinner").hidden, false, "a seconds-long wait");
  await sleep(900);
  assert.equal(doc(dom).querySelector("#vision-spinner").hidden, true);
  assert.match(text(dom, "#vision-note"), /It saw: A climb profile/);
});

test("add documents reports what was indexed", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  doc(dom)
    .querySelector("#add-files")
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  const add = calls.find((call) => call.name === "add_documents");
  assert.equal(add.args.scope, "files");
  assert.match(doc(dom).querySelector("#toast").getAttribute("role"), /status|alert/);
});

/* ---------- chat ---------- */

const submitChat = (dom, question) => {
  doc(dom).querySelector("#chat-input").value = question;
  doc(dom)
    .querySelector("#chat-form")
    .dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
};

test("chatting with no sessions starts one, and the model names it from the topic", async () => {
  const calls = [];
  const { dom } = page(calls, { inferredTitle: "Provisional tax instalments" });
  await tick();
  submitChat(dom, "what is provisional tax instalment?");
  await tick();

  const order = calls.map((call) => call.name);
  const made = order.indexOf("new_chat_session");
  const answered = order.indexOf("chat_completion");
  assert.ok(made !== -1 && answered !== -1 && made < answered, "the session exists before the turn is answered");
  assert.equal(calls.find((call) => call.name === "chat_completion").args.sessionId, "new-1");

  // The picker repaints from the backend: the model's title, the real turns.
  assert.equal(text(dom, ".session .session-title"), "Provisional tax instalments");
  assert.match(text(dom, ".session .session-turns"), /2 turns/);
});

test("chat answers with rendered markdown and clickable citations", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  submitChat(dom, "what is the climb gradient?");
  await tick();

  const chat = calls.find((call) => call.name === "chat_completion");
  assert.equal(chat.args.question, "what is the climb gradient?");
  assert.equal(chat.args.documents, null, "the whole index grounds an unticked question");
  assert.equal(chat.args.sessionId, "new-1", "no saved sessions, so the turn lands in a fresh one");
  assert.equal(
    chat.args.history,
    null,
    "the first turn has no thread to carry (null, not [], so serde yields an empty history)",
  );

  const assistant = [...doc(dom).querySelectorAll(".chat-msg")].at(-1);
  assert.equal(assistant.classList.contains("chat-assistant"), true);
  assert.match(assistant.querySelector("strong").textContent, /4\.5%/);
  assert.equal(assistant.querySelectorAll(".chat-body li").length, 2, "the list rendered");
  assert.equal(assistant.querySelector(".chat-code code").textContent, "code");
  const cite = assistant.querySelector(".chat-cite");
  assert.equal(cite.textContent, "[Doc-8168-Vol1.pdf p.45]");
  assert.equal(
    assistant.querySelector(".chat-foot"),
    null,
    "retrieved passages live behind the inline citations, not in a footer strip",
  );
});

test("a model answer is never treated as HTML", async () => {
  const { dom } = page([], {
    chat: { answer: `<img src=x onerror="window.__pwned=1">done`, citations: [], sources: [] },
  });
  await tick();
  submitChat(dom, "malicious reply please");
  await tick();
  assert.equal(dom.window.__pwned, undefined, "onerror handler must not run");
  assert.equal(doc(dom).querySelectorAll(".chat-body img").length, 0);
});

test("an empty question does not hit the backend", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  submitChat(dom, "   ");
  await tick();
  assert.equal(calls.some((call) => call.name === "chat_completion"), false);
});

test("answers format as markdown", async () => {
  const answer = [
    "### Heading",
    "**bold** and *italic* and ~~struck~~ and `code`.",
    "| wing | pos |",
    "| :-- | --: |",
    "| A | 3 |",
    "> a quoted note",
    "see [the manual](https://example.com/manual)",
    "- top",
    "  - nested",
    "---",
  ].join("\n");
  const { dom } = page([], { chat: { answer, sources: [] } });
  await tick();
  submitChat(dom, "format this");
  await tick();
  const body = [...doc(dom).querySelectorAll(".chat-msg")].at(-1).querySelector(".chat-body");
  assert.equal(body.querySelector("h5").textContent, "Heading");
  assert.match(body.querySelector("strong").textContent, /bold/);
  assert.match(body.querySelector("em").textContent, /italic/);
  assert.match(body.querySelector("del").textContent, /struck/);
  assert.equal(body.querySelector("code.chat-inline-code").textContent, "code");
  const table = body.querySelector("table.chat-table");
  assert.equal(table.querySelectorAll("tbody tr").length, 1);
  assert.equal(table.querySelector("td:nth-child(2)").style.textAlign, "right");
  assert.match(body.querySelector("blockquote.chat-quote").textContent, /quoted note/);
  const link = body.querySelector("a");
  assert.equal(link.getAttribute("href"), "https://example.com/manual");
  assert.equal(link.getAttribute("rel"), "noopener noreferrer");
  assert.ok(body.querySelector("ul li ul li"), "an indented item nests");
  assert.ok(body.querySelector("hr.chat-rule"), "a bare --- is a rule");
});

test("svg drawings render, but scripts and handlers are stripped", async () => {
  const answer = [
    "```svg",
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12" onload="window.__pwned=1">',
    "<script>window.__pwned = 1</script>",
    '<rect width="6" height="6" fill="green" onclick="window.__pwned=1"/>',
    '<a href="javascript:window.__pwned=1">go</a>',
    "</svg>",
    "```",
  ].join("\n");
  const { dom } = page([], { chat: { answer, sources: [] } });
  await tick();
  submitChat(dom, "draw a square");
  await tick();
  const svg = doc(dom).querySelector(".chat-svg");
  assert.ok(svg, "the fenced svg became a real drawing");
  assert.equal(svg.getAttribute("onload"), null, "handler on the root is gone");
  assert.equal(svg.querySelector("script"), null, "no scripts survive");
  assert.equal(svg.querySelector("a"), null, "no links survive");
  const rect = svg.querySelector("rect");
  assert.equal(rect.getAttribute("fill"), "green", "the drawing itself survives");
  assert.equal(rect.hasAttribute("onclick"), false);
  assert.equal(
    doc(dom).querySelector(".chat-body").textContent.includes("pwned"),
    false,
    "not even the inert payload text is left behind",
  );
});

test("a raw svg paragraph without a fence is sanitized too", async () => {
  const answer = '<svg viewBox="0 0 4 4"><circle cx="2" cy="2" r="2"/></svg>';
  const { dom } = page([], { chat: { answer, sources: [] } });
  await tick();
  submitChat(dom, "just a dot");
  await tick();
  assert.ok(doc(dom).querySelector(".chat-svg circle"), "the drawing lands");
});

test("mermaid blocks draw, or fall back to their source", async () => {
  const answer = "```mermaid\ngraph TD; A[Runway]-->B[Stopway];\n```";
  const { dom } = page([], { chat: { answer, sources: [] } });
  dom.window.MERMAID_LOAD_TIMEOUT_MS = 150;
  await tick();
  submitChat(dom, "draw the layout");
  await tick();
  const host = doc(dom).querySelector(".chat-mermaid");
  assert.ok(host, "the mermaid block reserves a drawing host");
  await sleep(500);
  assert.match(host.textContent, /graph TD/, "the source stays readable while the engine waits");
  assert.match(host.textContent, /diagram engine/, "and says so when the engine cannot run");
});

test("a failed chat shows the error in the transcript", async () => {
  const { dom } = page([], { chatError: "model returned 500: engine busy" });
  await tick();
  submitChat(dom, "will it blend?");
  await tick();
  assert.match(doc(dom).querySelector(".chat-error").textContent, /engine busy/);
  assert.equal(doc(dom).querySelector("#chat-send").disabled, false, "the box unlocks after a failure");
  assert.equal(doc(dom).querySelector("#chat-send").textContent, "Send", "the button is Send again");
});

test("the chat waits visibly while the model thinks", async () => {
  const { dom } = page([], { slow: { chat: 700 } });
  await tick();
  submitChat(dom, "visual dva holding");
  await sleep(60);
  assert.equal(doc(dom).querySelector("#chat-send").textContent, "Stop", "one question at a time");
  const pending = [...doc(dom).querySelectorAll(".chat-msg")].at(-1);
  assert.match(pending.textContent, /Thinking/);
  await sleep(900);
  assert.equal(doc(dom).querySelector("#chat-send").textContent, "Send");
  assert.match(doc(dom).querySelector("strong").textContent, /4\.5%/);
});

test("stop cancels the answer in flight and drops its reply", async () => {
  const calls = [];
  const { dom } = page(calls, { slow: { chat: 700 } });
  await tick();
  submitChat(dom, "visual dva holding");
  await tick();
  assert.equal(doc(dom).querySelector("#chat-send").textContent, "Stop", "send turns into stop");
  click(dom, "#chat-send");
  await tick();
  assert.ok(
    calls.some((call) => call.name === "cancel_chat"),
    "stop asks the backend to cancel",
  );
  assert.match(
    [...doc(dom).querySelectorAll(".chat-msg")].at(-1).textContent,
    /Stopped/,
    "the transcript says the answer was stopped",
  );
  await sleep(900);
  assert.equal(doc(dom).querySelector("#chat-send").textContent, "Send", "the button returns to send");
  assert.equal(doc(dom).querySelectorAll(".chat-body strong").length, 0, "the late reply is dropped");
});

test("history carries earlier turns and clear empties them", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  submitChat(dom, "first question");
  await tick();
  submitChat(dom, "and the second?");
  await tick();
  const second = calls.filter((call) => call.name === "chat_completion").at(-1);
  assert.equal(second.args.history.length, 2, "the first exchange travels along");
  assert.equal(second.args.history[0].role, "user");
  assert.equal(second.args.history[1].role, "assistant");
  assert.equal(
    typeof second.args.history[1].content,
    "string",
    "the assistant turn goes as its answer text, not the { answer, sources } object",
  );
  assert.match(second.args.history[1].content, /4\.5%/, "and the answer text is intact");

  click(dom, "#chat-new");
  await tick();
  assert.equal(doc(dom).querySelectorAll(".chat-msg").length, 0);
  assert.equal(doc(dom).querySelector("#chat-empty").hidden, false, "the pane says why it is empty");
});

test("a citation click lands the knowledge pane on the passage", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  submitChat(dom, "what is the climb gradient?");
  await tick();
  click(dom, ".chat-cite");
  await tick();
  assert.match(doc(dom).querySelector("#chunks-heading").textContent, /Doc-8168-Vol1\.pdf/);
  assert.match(doc(dom).querySelector("#toast").textContent, /Showing Doc-8168-Vol1\.pdf/);
  assert.equal(
    doc(dom).querySelector("#knowledge-dialog").open,
    true,
    "a citation click opens the knowledge modal on the spot",
  );
  // The pane's last load filters by the cited page, not by a text guess.
  const hint = calls.filter((call) => call.name === "browse").at(-1);
  assert.equal(hint.args.document, "Doc-8168-Vol1.pdf");
  assert.equal(hint.args.page, 45, "page is a metadata filter, not the text box");
  assert.equal(hint.args.filter, null, "no text filter is faked for the jump");
  assert.match(text(dom, "#chunk-count"), /on page 45/, "the count names the lock");
  const cards = [...doc(dom).querySelectorAll(".chunk")];
  assert.ok(cards[0].classList.contains("is-cited"), "the cited chunk itself is marked");
  assert.equal(
    cards[1].classList.contains("is-cited"),
    false,
    "its page neighbour stays unmarked",
  );
});

test("a numbered citation opens the passage at its index", async () => {
  const calls = [];
  const { dom } = page(calls, {
    chat: {
      answer: "The gradient is **4.5%** [1], the weight is 60,000 kg [2], and [9] is out of range.",
      sources: [
        { filename: "Annex-14.pdf", page: 34, kind: "rerank", score: 0.9, figure: false },
        { filename: "Doc-8168-Vol1.pdf", page: 45, kind: "hybrid", score: 0.8, figure: false },
      ],
    },
  });
  await tick();
  submitChat(dom, "what is the climb gradient?");
  await tick();

  const assistant = [...doc(dom).querySelectorAll(".chat-msg")].at(-1);
  assert.deepEqual(
    [...assistant.querySelectorAll(".chat-cite")].map((cite) => cite.textContent),
    ["[1]", "[2]"],
    "passage numbers are links",
  );
  assert.match(
    assistant.querySelector(".chat-plain").textContent,
    /\[9\]/,
    "a number with no passage behind it stays plain",
  );

  click(dom, ".chat-cite");
  await tick();
  assert.match(
    doc(dom).querySelector("#chunks-heading").textContent,
    /Annex-14\.pdf/,
    "[1] lands on the first retrieved passage",
  );
});

test("the document tick controls which documents ground the chat", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();

  const groupCheck = doc(dom).querySelector(".doc-group-check");
  assert.ok(groupCheck.checked, "everything grounds by default");

  groupCheck.checked = false;
  groupCheck.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  await tick();
  assert.equal(
    doc(dom).querySelectorAll(".doc.off").length, 2,
    "the whole unticked group reads as off",
  );

  submitChat(dom, "what is the climb gradient?");
  await tick();
  const chat = calls.find((call) => call.name === "chat_completion");
  assert.deepEqual(
    chat.args.documents,
    [],
    "an unticked group grounds nothing on purpose",
  );

  groupCheck.checked = true;
  groupCheck.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  submitChat(dom, "and now?");
  await tick();
  const again = calls.filter((call) => call.name === "chat_completion").at(-1);
  assert.equal(again.args.documents, null, "all ticked means the whole index again");
});

/* ---------- vision settings and the caption pre-flight ---------- */

const click = (dom, selector) =>
  doc(dom)
    .querySelector(selector)
    .dispatchEvent(new dom.window.Event("click", { bubbles: true }));
const change = (dom, selector) =>
  doc(dom)
    .querySelector(selector)
    .dispatchEvent(new dom.window.Event("change", { bubbles: true }));

/* ---------- the chats panel ---------- */

test("the index note lives in a popover behind an info chip", async () => {
  const { dom } = page([]);
  await tick();
  assert.equal(doc(dom).querySelector("#stats-popover").hidden, true, "the note starts folded");
  click(dom, "#stats-info");
  assert.equal(doc(dom).querySelector("#stats-popover").hidden, false, "the chip opens the note");
  assert.equal(doc(dom).querySelector("#stats-info").getAttribute("aria-expanded"), "true");
  assert.match(
    doc(dom).querySelector("#stats-popover").textContent,
    /2,619 chunks · 2 documents/,
    "the popover carries the index note",
  );
  click(dom, "#chat-new");
  assert.equal(doc(dom).querySelector("#stats-popover").hidden, true, "clicking away folds it");
});

test("documents group under collapsible folder headers", async () => {
  const documents = [
    { filename: "a.pdf", path: "/Users/me/Manuals/a.pdf", chunks: 10, pages: 5, figures: 0, sourceAvailable: true },
    { filename: "b.pdf", path: "/Users/me/Manuals/b.pdf", chunks: 12, pages: 6, figures: 0, sourceAvailable: true },
    { filename: "c.pdf", path: "/Users/me/Loose/c.pdf", chunks: 7, pages: 2, figures: 0, sourceAvailable: true },
  ];
  const { dom } = page([], { documents });
  await tick();
  const headers = [...doc(dom).querySelectorAll(".doc-group-toggle")];
  assert.equal(headers.length, 2, "one header per folder");
  assert.match(headers[0].textContent, /Loose/);
  assert.match(headers[1].textContent, /Manuals/);
  assert.match(headers[1].textContent, /2 docs/, "the header counts its cards");
  assert.equal(doc(dom).querySelectorAll(".doc").length, 3);

  headers[1].dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(doc(dom).querySelectorAll(".doc").length, 1, "folding Manuals hides its cards");
  assert.equal(
    [...doc(dom).querySelectorAll(".doc-group-toggle")][1].getAttribute("aria-expanded"),
    "false",
  );

  [...doc(dom).querySelectorAll(".doc-group-toggle")][1].dispatchEvent(
    new dom.window.Event("click", { bubbles: true }),
  );
  await tick();
  assert.equal(doc(dom).querySelectorAll(".doc").length, 3, "unfold brings them back");
});

test("the group tick empties or fills the whole folder", async () => {
  const documents = [
    { filename: "a.pdf", path: "/Users/me/Manuals/a.pdf", chunks: 10, pages: 5, figures: 0, sourceAvailable: true },
    { filename: "b.pdf", path: "/Users/me/Manuals/b.pdf", chunks: 12, pages: 6, figures: 0, sourceAvailable: true },
    { filename: "c.pdf", path: "/Users/me/Loose/c.pdf", chunks: 7, pages: 2, figures: 0, sourceAvailable: true },
  ];
  const { dom } = page([], { documents });
  await tick();
  const manuals = [...doc(dom).querySelectorAll(".doc-group-check")][1];
  assert.equal(manuals.checked, true, "everything starts grounded");
  assert.equal(manuals.indeterminate, false);

  manuals.checked = false;
  manuals.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  await tick();
  const rows = [...doc(dom).querySelectorAll(".doc")];
  const manualsRows = rows.filter((li) => li.dataset.folder === "/Users/me/Manuals");
  assert.ok(manualsRows.length === 2 && manualsRows.every((li) => li.classList.contains("off")),
    "unticking the folder dims every row in it");
  const loose = [...doc(dom).querySelectorAll(".doc-group-check")][0];
  assert.equal(loose.checked, true, "other folders keep their tick");
  assert.equal(
    rows.filter((li) => li.dataset.folder === "/Users/me/Loose").every((li) => !li.classList.contains("off")),
    true,
    "and their rows stay plain",
  );

  manuals.checked = true;
  manuals.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  assert.ok(manualsRows.every((li) => !li.classList.contains("off")), "ticking the folder fills it");
  assert.equal(manuals.indeterminate, false, "full again");
});

test("folder names can be renamed without touching the real path", async () => {
  const documents = [
    { filename: "a.pdf", path: "/Users/me/Manuals/a.pdf", chunks: 10, pages: 5, figures: 0, sourceAvailable: true },
    { filename: "c.pdf", path: "/Users/me/Loose/c.pdf", chunks: 7, pages: 2, figures: 0, sourceAvailable: true },
  ];
  const { dom } = page([], { documents });
  await tick();
  const renamePencil = [...doc(dom).querySelectorAll(".doc-group-rename")][1];
  renamePencil.dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  assert.equal(doc(dom).querySelector("#chat-rename-dialog").open, true);
  assert.match(doc(dom).querySelector("#chat-rename-title").textContent, /folder/i);
  assert.equal(doc(dom).querySelector("#chat-rename-input").value, "Manuals");
  doc(dom).querySelector("#chat-rename-input").value = "  Flight manuals  ";
  click(dom, "#chat-rename-ok");
  await tick();
  const names = [...doc(dom).querySelectorAll(".doc-group-name")].map((n) => n.textContent);
  assert.ok(names.includes("Flight manuals"), "the header carries the custom name");
  const stillPathTooltip = [...doc(dom).querySelectorAll(".doc-group-toggle")]
    .find((b) => b.title === "/Users/me/Manuals");
  assert.ok(stillPathTooltip, "the tooltip keeps the real path");
});

test("a new group takes the next added documents, folders name themselves", async () => {
  const documents = [
    { filename: "new.pdf", path: "/Users/me/Downloads/new.pdf", chunks: 4, pages: 1, figures: 0, sourceAvailable: true },
  ];
  const addOutcome = {
    indexed: ["new.pdf"],
    unchanged: [],
    newChunks: 4,
    totalChunks: 4,
    totalDocuments: 1,
    cancelled: false,
    figures: 0,
    captionsMade: 0,
    captionFailures: 0,
  };
  const { dom } = page([], { documents, addOutcome });
  await tick();
  assert.match(doc(dom).querySelector(".doc").dataset.folder, /Downloads/);

  click(dom, "#group-new");
  doc(dom).querySelector("#chat-rename-input").value = "Cabin safety";
  click(dom, "#chat-rename-ok");
  await tick();
  const names = [...doc(dom).querySelectorAll(".doc-group-name")].map((n) => n.textContent);
  assert.ok(names.includes("Cabin safety"), "the group shows while still empty");
  assert.ok(doc(dom).querySelector(".doc-group-blank"), "the empty group says what fills it");

  // A folder indexes under its own name; it must not consume the group.
  click(dom, "#add-folder");
  await tick();
  assert.match(doc(dom).querySelector(".doc").dataset.folder, /Downloads/);

  click(dom, "#add-files");
  await tick();
  assert.equal(
    doc(dom).querySelector(".doc").dataset.folder,
    "custom:Cabin safety",
    "the hand-added file joined the named group",
  );

  const pencil = doc(dom).querySelector(".doc-group-rename");
  pencil.dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  doc(dom).querySelector("#chat-rename-input").value = "Oxygen equipment";
  click(dom, "#chat-rename-ok");
  await tick();
  assert.equal(doc(dom).querySelector(".doc").dataset.folder, "custom:Oxygen equipment");
  assert.match(doc(dom).querySelector(".doc-group-name").textContent, /Oxygen equipment/);
});

test("deleting a group removes its documents only after a confirmation", async () => {
  const calls = [];
  const documents = [
    { filename: "a.pdf", path: "/Users/me/Manuals/a.pdf", chunks: 10, pages: 5, figures: 0, sourceAvailable: true },
    { filename: "b.pdf", path: "/Users/me/Manuals/b.pdf", chunks: 12, pages: 6, figures: 0, sourceAvailable: true },
    { filename: "c.pdf", path: "/Users/me/Loose/c.pdf", chunks: 7, pages: 2, figures: 0, sourceAvailable: true },
  ];
  const { dom } = page(calls, { documents });
  await tick();
  const trash = [...doc(dom).querySelectorAll(".doc-group-remove")][1];
  trash.dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(doc(dom).querySelector("#confirm-remove").open, true, "the dialog asks first");
  assert.match(text(dom, "#confirm-body"), /Manuals/);
  assert.match(text(dom, "#confirm-body"), /2 document\(s\)/);
  assert.equal(calls.some((call) => call.name === "remove_document"), false, "nothing removed yet");

  click(dom, "#confirm-ok");
  await tick();
  assert.deepEqual(
    calls.filter((call) => call.name === "remove_document").map((call) => call.args.filename).sort(),
    ["a.pdf", "b.pdf"],
    "the whole folder went, and only the folder's documents",
  );
  assert.equal(doc(dom).querySelector("#confirm-remove").open, false);
});

test("an empty named group deletes itself without touching the index", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  click(dom, "#group-new");
  doc(dom).querySelector("#chat-rename-input").value = "Oxygen";
  click(dom, "#chat-rename-ok");
  await tick();
  const header = [...doc(dom).querySelectorAll(".doc-group")]
    .find((li) => li.querySelector(".doc-group-name")?.textContent === "Oxygen");
  assert.ok(header, "the empty group exists");
  header.querySelector(".doc-group-remove").dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  assert.match(text(dom, "#confirm-body"), /empty group “Oxygen”/);
  click(dom, "#confirm-ok");
  await tick();
  assert.equal(calls.some((call) => call.name === "remove_document"), false, "nothing to remove");
  const names = [...doc(dom).querySelectorAll(".doc-group-name")].map((n) => n.textContent);
  assert.equal(names.includes("Oxygen"), false, "the group is gone");
});

test("the chats panel lists saved chats and picking one loads its transcript", async () => {
  const { dom } = page([], {
    sessions: [
      { id: "c-1", title: "Gradient questions", turns: 2 },
      { id: "c-2", title: "Weight and balance", turns: 5 },
    ],
    sessionTurns: {
      "c-1": [{ role: "user", content: "the first saved question" }],
      "c-2": [
        { role: "user", content: "max ramp weight?" },
        {
          role: "assistant",
          content: {
            answer: "**136,000 kg** [Doc-8168-Vol1.pdf p.12].",
            sources: [
              { filename: "Doc-8168-Vol1.pdf", page: 12, kind: "hybrid", score: 0.7, figure: false },
            ],
          },
        },
      ],
    },
  });
  await tick();

  const rows = [...doc(dom).querySelectorAll(".session")];
  assert.deepEqual(
    rows.map((row) => row.querySelector(".session-title").textContent),
    ["Gradient questions", "Weight and balance"],
  );
  assert.deepEqual(
    rows.map((row) => row.querySelector(".session-turns").textContent),
    ["2 turns", "5 turns"],
    "turn counts read as English next to the titles",
  );
  assert.equal(rows[0].getAttribute("aria-current"), "true", "the first saved chat opens");
  assert.match(
    doc(dom).querySelector("#chat-session-list [aria-current] .session-title").textContent,
    /Gradient questions/,
    "the open chat is marked in the chats panel",
  );

  rows[1].querySelector(".session-open").dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();

  const messages = [...doc(dom).querySelectorAll(".chat-msg")];
  assert.equal(messages.length, 2, "the saved exchange renders as a transcript");
  assert.match(messages[1].querySelector("strong").textContent, /136,000 kg/);
  assert.match(messages[1].querySelector(".chat-cite").textContent, /\[Doc-8168-Vol1\.pdf p\.12\]/);
  assert.match(
    doc(dom).querySelector("#chat-session-list [aria-current] .session-title").textContent,
    /Weight and balance/,
    "the mark moves with the pick",
  );
});

test("a new chat starts blank", async () => {
  const calls = [];
  const { dom } = page(calls, {
    sessions: [{ id: "c-1", title: "Gradient questions", turns: 2 }],
    sessionTurns: { "c-1": [{ role: "user", content: "the first saved question" }] },
  });
  await tick();
  assert.equal(doc(dom).querySelectorAll(".chat-msg").length, 1, "a saved chat opened");

  click(dom, "#chat-new");
  await tick();
  assert.equal(calls.some((call) => call.name === "new_chat_session"), true);
  assert.equal(doc(dom).querySelectorAll(".chat-msg").length, 0, "a new chat starts blank");
  assert.match(
    doc(dom).querySelector("#chat-session-list [aria-current] .session-title").textContent,
    /New chat/,
    "the new chat announces itself in the panel",
  );
});

test("renaming asks first, then renames only the chat that is open", async () => {
  const calls = [];
  const { dom } = page(calls, { sessions: [{ id: "c-1", title: "Gradient questions", turns: 2 }] });
  await tick();

  click(dom, ".session-edit");
  assert.equal(doc(dom).querySelector("#chat-rename-dialog").open, true, "rename is a dialog");
  assert.equal(
    doc(dom).querySelector("#chat-rename-input").value,
    "Gradient questions",
    "prefilled with the title on screen",
  );

  click(dom, "#chat-rename-cancel");
  await tick();
  assert.equal(doc(dom).querySelector("#chat-rename-dialog").open, false);
  assert.equal(
    calls.some((call) => call.name === "rename_chat_session"),
    false,
    "cancelling touches nothing",
  );

  click(dom, ".session-edit");
  doc(dom).querySelector("#chat-rename-input").value = "  Performance chapter  ";
  click(dom, "#chat-rename-ok");
  await tick();
  const rename = calls.find((call) => call.name === "rename_chat_session");
  assert.equal(rename.args.id, "c-1");
  assert.equal(rename.args.title, "Performance chapter", "padded input is trimmed before it is stored");
  assert.equal(
    doc(dom).querySelector("#chat-rename-dialog").open,
    false,
    "saving closes the dialog, not only cancelling",
  );
  assert.deepEqual(
    [...doc(dom).querySelectorAll(".session .session-title")].map((title) => title.textContent),
    ["Performance chapter"],
    "the chats panel repaints with the new title",
  );
  assert.match(text(dom, "#toast"), /renamed/i);
});

test("the rename dialog starts with the title fully selected", async () => {
  const { dom } = page([], { sessions: [{ id: "c-1", title: "Gradient questions", turns: 2 }] });
  await tick();
  click(dom, ".session-edit");
  const input = doc(dom).querySelector("#chat-rename-input");
  assert.equal(input.selectionStart, 0, "the caret is at the start, not mid-text");
  assert.equal(input.selectionEnd, input.value.length, "typing replaces the whole title");
});

test("deleting a chat drops it from the picker and shows what is left", async () => {
  const calls = [];
  const { dom } = page(calls, {
    sessions: [
      { id: "c-1", title: "Gradient questions", turns: 2 },
      { id: "c-2", title: "Weight and balance", turns: 5 },
    ],
    sessionTurns: {
      "c-1": [{ role: "user", content: "the first saved question" }],
      "c-2": [{ role: "user", content: "max ramp weight?" }],
    },
  });
  await tick();
  assert.equal(doc(dom).querySelectorAll(".chat-msg").length, 1, "c-1 opened on boot");

  click(dom, ".session-remove");
  await tick();
  const removed = calls.find((call) => call.name === "delete_chat_session");
  assert.equal(removed.args.id, "c-1");
  assert.deepEqual(
    [...doc(dom).querySelectorAll(".session .session-title")].map((title) => title.textContent),
    ["Weight and balance"],
    "the deleted chat is gone from the list",
  );
  assert.match(
    doc(dom).querySelector("#chat-session-list [aria-current] .session-title").textContent,
    /Weight and balance/,
    "the next saved chat opens instead",
  );
  assert.match(text(dom, "#toast"), /deleted/i);

  click(dom, ".session-remove");
  await tick();
  assert.equal(doc(dom).querySelectorAll(".chat-msg").length, 0, "nothing left to show");
  assert.equal(doc(dom).querySelector("#chat-empty").hidden, false, "and the pane says why");
  assert.equal(doc(dom).querySelector(".session"), null, "and the chats panel is empty");
});

test("undoing a chat delete brings it back with its transcript", async () => {
  const calls = [];
  const { dom } = page(calls, {
    sessions: [
      { id: "c-1", title: "Gradient questions", turns: 2 },
      { id: "c-2", title: "Weight and balance", turns: 5 },
    ],
    sessionTurns: {
      "c-1": [{ role: "user", content: "the first saved question" }],
      "c-2": [{ role: "user", content: "max ramp weight?" }],
    },
  });
  await tick();

  click(dom, ".session-remove");
  await tick();
  const undo = doc(dom).querySelector("#toast .toast-action");
  assert.ok(undo, "the delete toast offers Undo");
  assert.match(undo.textContent, /undo/i);

  undo.dispatchEvent(new dom.window.Event("click", { bubbles: true }));
  await tick();
  const restored = calls.find((call) => call.name === "restore_chat_session");
  assert.equal(restored.args.id, "c-1", "undo restores the chat that was deleted");
  assert.deepEqual(
    [...doc(dom).querySelectorAll(".session .session-title")].map((title) => title.textContent),
    ["Gradient questions", "Weight and balance"],
    "the picker has both chats again",
  );
  assert.match(text(dom, "#toast"), /restored/i);
  assert.match(
    doc(dom).querySelector("#chat-session-list [aria-current] .session-title").textContent,
    /Gradient questions/,
    "the restored chat is the one on screen",
  );
});

test("the chat log follows answers only while the reader is at the bottom", async () => {
  const calls = [];
  const { dom } = page(calls, { slow: { chat: 500 } });
  await tick();
  submitChat(dom, "what is the climb gradient?");

  const log = doc(dom).querySelector("#chat-log");
  // jsdom lays nothing out: fake a tall log so "bottom" means something.
  Object.defineProperty(log, "scrollHeight", { configurable: true, value: 4000 });
  Object.defineProperty(log, "clientHeight", { configurable: true, value: 600 });

  // Mid-stream, the reader scrolls up to re-read an earlier answer.
  await sleep(100);
  log.scrollTop = 0;
  log.dispatchEvent(new dom.window.Event("scroll", { bubbles: true }));
  assert.equal(
    doc(dom).querySelector("#chat-jump").hidden,
    false,
    "scrolling up shows the jump-to-latest pill",
  );

  // The answer lands while the reader is away. The view stays put.
  await sleep(600);
  assert.match(
    [...doc(dom).querySelectorAll(".chat-msg")].at(-1).textContent,
    /4\.5%/,
    "the answer did land in the log",
  );
  assert.ok(log.scrollTop < 3400, "a finished answer does not force the view to the bottom");

  click(dom, "#chat-jump");
  await tick();
  assert.equal(log.scrollTop, 4000, "the pill jumps to the bottom");
  assert.equal(doc(dom).querySelector("#chat-jump").hidden, true, "and hides itself");
});

test("Enter mid-IME-composition belongs to the input method, not the send", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  const input = doc(dom).querySelector("#chat-input");
  input.value = "スー";
  const event = new dom.window.KeyboardEvent("keydown", {
    key: "Enter",
    bubbles: true,
    cancelable: true,
  });
  // jsdom's KeyboardEvent cannot set isComposing; define it like a live IME would.
  Object.defineProperty(event, "isComposing", { value: true });
  input.dispatchEvent(event);
  await tick();
  assert.equal(
    calls.some((call) => call.name === "chat_completion"),
    false,
    "composing Enter never reaches the backend",
  );

  const plain = new dom.window.KeyboardEvent("keydown", {
    key: "Enter",
    bubbles: true,
    cancelable: true,
  });
  input.dispatchEvent(plain);
  await tick();
  assert.equal(
    calls.some((call) => call.name === "chat_completion"),
    true,
    "a plain Enter still sends",
  );
});

test("saved answers keep their text and citations when reopened", async () => {
  const { dom } = page([], {
    sessions: [{ id: "c-9", title: "Stopway again", turns: 2 }],
    sessionTurns: {
      "c-9": [
        { role: "user", content: "what is a stopway?" },
        { role: "assistant", content: "A **stopway** is defined [1]." },
      ],
    },
    sessionSources: {
      "c-9": [[
        { id: "x:34:1", filename: "Annex-14.pdf", page: 34, kind: "rerank", score: 0.8, figure: false },
      ]],
    },
  });
  await tick();

  const messages = [...doc(dom).querySelectorAll(".chat-msg")];
  assert.equal(messages.length, 2, "the saved answer is part of the transcript");
  assert.match(messages[1].querySelector("strong").textContent, /stopway/);
  assert.match(
    messages[1].querySelector(".chat-cite").textContent,
    /\[1\]/,
    "citations resolve against the sources stored with the chat",
  );
});

test("the chats panel collapses and expands without losing its list", async () => {
  const { dom } = page([], { sessions: [{ id: "c-1", title: "Gradient questions", turns: 2 }] });
  await tick();

  click(dom, "#sessions-toggle");
  await tick();
  assert.equal(doc(dom).querySelector("#panes").classList.contains("sessions-collapsed"), true);
  assert.equal(doc(dom).querySelector("#sessions-toggle").getAttribute("aria-expanded"), "false");

  click(dom, "#sessions-toggle");
  await tick();
  assert.equal(
    doc(dom).querySelector("#panes").classList.contains("sessions-collapsed"),
    false,
    "expanding brings the panel back",
  );
  assert.equal(doc(dom).querySelector("#sessions-toggle").getAttribute("aria-expanded"), "true");
  assert.equal(doc(dom).querySelectorAll(".session").length, 1, "the list survived the collapse");
});

test("raw model tokens stay hidden until the answer is whole", async () => {
  const { dom } = page([], {
    slow: { chat: 700 },
    chat: {
      // The model streams the JSON wrapper first: revealing tokens as they land
      // would show braces and escapes to the user.
      tokens: ['{"answer": "', "The gradient is ", "**4.5%**", '"}'],
      answer: "The gradient is **4.5%** [Doc-8168-Vol1.pdf p.45].",
      sources: [
        { filename: "Doc-8168-Vol1.pdf", page: 45, kind: "hybrid", score: 0.8, figure: false },
      ],
    },
  });
  await tick();
  submitChat(dom, "visual dva holding");
  await sleep(400);

  const pending = [...doc(dom).querySelectorAll(".chat-msg")].at(-1);
  assert.match(pending.textContent, /Thinking/, "tokens have landed, the answer has not");
  assert.equal(pending.textContent.includes("{"), false, "no JSON fragment reaches the transcript");
  assert.equal(pending.querySelector(".chat-cite"), null, "no citations before the answer");

  await sleep(500);
  assert.match(pending.querySelector("strong").textContent, /4\.5%/);
  assert.match(pending.querySelector(".chat-cite").textContent, /\[Doc-8168-Vol1\.pdf p\.45\]/);
});

test("vision settings load into the panel", async () => {
  const { dom } = page([]);
  await tick();
  assert.equal(doc(dom).querySelector("#vision-enabled").checked, true);
  assert.equal(doc(dom).querySelector("#vision-url").value, "http://127.0.0.1:11234/v1");
  // A stored key is never echoed back into a readable field.
  assert.equal(doc(dom).querySelector("#vision-key").value, "");
  assert.match(doc(dom).querySelector("#vision-key").placeholder, /saved/);
});

test("the vision model is a dropdown fed by the endpoint's own list", async () => {
  const { dom } = page([]);
  await tick();
  const select = doc(dom).querySelector("#vision-model");
  assert.equal(select.hidden, false, "the listing arrived, so the dropdown shows");
  assert.equal(doc(dom).querySelector("#vision-model-manual").hidden, true);
  assert.deepEqual(
    [...select.options].map((option) => option.value),
    ["glm-5.2", "qwen3-vl"],
  );
  assert.equal(select.value, "qwen3-vl", "the saved model is what the dropdown shows");
});

test("an unreachable endpoint keeps the model box typeable", async () => {
  const { dom } = page([], { visionModelsError: "connection refused" });
  await tick();
  const manual = doc(dom).querySelector("#vision-model-manual");
  assert.equal(manual.hidden, false, "the typed box is the fallback");
  assert.equal(manual.value, "qwen3-vl", "the saved model survives a failed listing");
  assert.equal(doc(dom).querySelector("#vision-model").hidden, true);
});

test("editing the endpoint saves it and refetches the model list", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  const url = doc(dom).querySelector("#vision-url");
  url.value = "http://192.168.0.9:8000/v1";
  change(dom, "#vision-url");
  await tick();
  const saved = calls.find((call) => call.name === "save_vision_settings");
  assert.equal(saved.args.baseUrl, "http://192.168.0.9:8000/v1");
  assert.equal(saved.args.enabled, true);
  assert.equal(saved.args.model, "qwen3-vl", "the dropdown's selection is what saves");
  const lists = calls.filter((call) => call.name === "vision_models");
  assert.equal(lists.length, 2, "once at open, once after the endpoint changed");
});

test("missing PDFium is stated with the settings, not after an ingest", async () => {
  const { dom } = page([], { vision: { ...VISION, pdfium: false } });
  await tick();
  assert.match(
    doc(dom).querySelector("#vision-note").textContent,
    /PDFium is not installed/,
  );
});

test("the vision test reports what the model actually saw", async () => {
  const { dom } = page([]);
  await tick();
  click(dom, "#vision-test");
  await tick();
  assert.match(
    doc(dom).querySelector("#vision-note").textContent,
    /It saw: A climb profile with a 4\.5% gradient\./,
  );
});

test("a failed vision test is shown rather than swallowed", async () => {
  const { dom } = page([], { testVisionError: "192.168.0.9 is not a local address" });
  await tick();
  click(dom, "#vision-test");
  await tick();
  const note = doc(dom).querySelector("#vision-note");
  assert.match(note.textContent, /not a local address/);
  assert.ok(note.classList.contains("is-error"));
  assert.equal(
    doc(dom).querySelector("#vision-test").disabled,
    false,
    "the button unlocks after a failure",
  );
});

test("documents with figures are priced before any captioning is paid for", async () => {
  const calls = [];
  const { dom } = page(calls, { addOutcome: PENDING });
  await tick();
  click(dom, "#add-files");
  await tick();
  const dialog = doc(dom).querySelector("#caption-prompt");
  assert.equal(dialog.open, true, "the question has to come before the cost");
  assert.match(dialog.textContent, /34 figure page\(s\)/);
  assert.match(dialog.textContent, /qwen3-vl/);
  assert.match(dialog.textContent, /2\.3–8\.5 minutes/);
  assert.equal(calls.some((call) => call.name === "commit_ingest"), false);
});

test("committing sends only the token, never file paths", async () => {
  const calls = [];
  const { dom } = page(calls, { addOutcome: PENDING });
  await tick();
  click(dom, "#add-files");
  await tick();
  click(dom, "#caption-prompt-text");
  await tick();
  const commit = calls.find((call) => call.name === "commit_ingest");
  assert.deepEqual(Object.keys(commit.args).sort(), ["caption", "token"]);
  assert.equal(commit.args.token, "9f3a1c");
  assert.equal(commit.args.caption, false);
});

test("captioning is committed as an explicit yes", async () => {
  const calls = [];
  const { dom } = page(calls, { addOutcome: PENDING });
  await tick();
  click(dom, "#add-files");
  await tick();
  click(dom, "#caption-prompt-yes");
  await tick();
  const commit = calls.find((call) => call.name === "commit_ingest");
  assert.equal(commit.args.caption, true);
  assert.match(text(dom, "#toast"), /34 figure caption\(s\) made/);
});

test("cancelling the caption question commits nothing and unlocks the buttons", async () => {
  const calls = [];
  const { dom } = page(calls, { addOutcome: PENDING });
  await tick();
  click(dom, "#add-files");
  await tick();
  click(dom, "#caption-prompt-cancel");
  await tick();
  assert.equal(calls.some((call) => call.name === "commit_ingest"), false);
  assert.equal(doc(dom).querySelector("#caption-prompt").open, false);
  assert.equal(doc(dom).querySelector("#add-files").disabled, false);
});

test("without PDFium the caption button is disabled but text indexing is not", async () => {
  const { dom } = page([], { addOutcome: { ...PENDING, pdfium: false } });
  await tick();
  click(dom, "#add-files");
  await tick();
  const warn = doc(dom).querySelector("#caption-prompt-warn");
  assert.equal(warn.hidden, false);
  assert.match(warn.textContent, /PDFium is not installed/);
  assert.equal(doc(dom).querySelector("#caption-prompt-yes").disabled, true);
  assert.equal(doc(dom).querySelector("#caption-prompt-text").disabled, false);
});

test("caption failures are reported instead of passing as success", async () => {
  const { dom } = page([], { addOutcome: PENDING, captionFailures: 9 });
  await tick();
  click(dom, "#add-files");
  await tick();
  click(dom, "#caption-prompt-yes");
  await tick();
  const toast = doc(dom).querySelector("#toast");
  assert.match(toast.textContent, /9 caption failure\(s\)/);
  assert.equal(toast.getAttribute("data-kind"), "error");
});

test("captions are marked as captions in the document list and passages", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  void calls;
  assert.match(doc(dom).querySelector(".doc-meta").textContent, /12 figure captions/);
  // A document with no captions says nothing about figures.
  assert.equal(doc(dom).querySelectorAll(".doc")[1].textContent.includes("figure"), false);

  click(dom, ".doc-main");
  await tick();
  const badges = doc(dom).querySelectorAll(".chunk-figure");
  assert.equal(badges.length, 1, "only the caption chunk carries the badge");
  assert.match(badges[0].textContent, /figure caption/);
  assert.match(badges[0].title, /not text printed on it/);
});

test("vision settings open in a sheet, never inside the header", async () => {
  const { dom } = page([]);
  await tick();
  // The header must contain no controls of its own: as a flex item, a form
  // there grows the bar and pushes the workspace below the fold.
  assert.equal(doc(dom).querySelectorAll(".topbar input, .topbar textarea").length, 0);
  assert.equal(doc(dom).querySelector("#vision-dialog").open, false);

  click(dom, "#vision-open");
  assert.equal(doc(dom).querySelector("#vision-dialog").open, true);
  assert.equal(doc(dom).querySelector("#vision-url").value, "http://127.0.0.1:11234/v1");
});

test("the header states whether captioning will run", async () => {
  const on = page([]);
  await tick();
  assert.equal(text(on.dom, "#vision-state"), "on");
  assert.equal(doc(on.dom).querySelector("#vision-state").dataset.state, "on");

  const off = page([], { vision: { ...VISION, enabled: false } });
  await tick();
  assert.equal(text(off.dom, "#vision-state"), "off");

  const broken = page([], { vision: { ...VISION, enabled: true, pdfium: false } });
  await tick();
  assert.equal(text(broken.dom, "#vision-state"), "no PDFium");
  assert.equal(doc(broken.dom).querySelector("#vision-state").dataset.state, "unavailable");
});

test("closing the sheet stores the fields, Escape discards them", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  click(dom, "#vision-open");

  const url = doc(dom).querySelector("#vision-url");
  url.value = "http://127.0.0.1:8080/v1";
  click(dom, "#vision-done");
  await tick();
  const saved = calls.filter((call) => call.name === "save_vision_settings").pop();
  assert.equal(saved.args.baseUrl, "http://127.0.0.1:8080/v1");
  assert.equal(doc(dom).querySelector("#vision-dialog").open, false);

  // Escape re-reads the stored settings rather than keeping half-typed values.
  click(dom, "#vision-open");
  doc(dom).querySelector("#vision-url").value = "http://typo";
  doc(dom)
    .querySelector("#vision-dialog")
    .dispatchEvent(new dom.window.Event("cancel", { bubbles: true, cancelable: true }));
  await tick();
  assert.equal(doc(dom).querySelector("#vision-url").value, "http://127.0.0.1:11234/v1");
});

test("the knowledge modal explains itself instead of showing a bare button", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  const empty = doc(dom).querySelector("#chunk-empty");
  assert.equal(empty.hasAttribute("hidden"), false, "an empty modal needs its message");
  assert.match(empty.textContent, /indexed chunk/);
  assert.equal(doc(dom).querySelector("#load-more").hasAttribute("hidden"), true);

  click(dom, ".doc-main");
  await tick();
  assert.equal(doc(dom).querySelector("#chunk-empty").hasAttribute("hidden"), true);
  // Two of three passages are loaded, so more to load is a real offer here.
  assert.equal(doc(dom).querySelector("#load-more").hasAttribute("hidden"), false);
});

test("the knowledge modal's CSS hides it until it is open", async () => {
  // jsdom never renders stylesheets, so the one bug it cannot catch by
  // behaviour is guarded here: an author `display` in the base rule of a
  // dialog beats the UA's dialog:not([open]) { display: none } and leaves
  // the modal painted on screen forever, immune to close().
  const bare = css.replace(/\/\*[^*]*\*+(?:[^/*][^*]*\*+)*\//g, "");
  const baseRule = bare.match(/\.knowledge\s*\{([^}]*)\}/);
  assert.ok(baseRule, "the base rule exists");
  assert.equal(/display:/.test(baseRule[1]), false, "no display in the base rule");
  assert.match(bare, /\.knowledge\[open\]\s*\{[^}]*display:\s*flex/);
  // The general case: any component display rule can out-shout the UA's
  // [hidden] { display: none }, so the stylesheet must normalize [hidden].
  assert.match(bare, /\[hidden\]\s*\{[^}]*display:\s*none\s*!important/);
});

test("counts read as English", async () => {
  const { dom } = page([]);
  await tick();
  const rows = doc(dom).querySelectorAll(".doc-meta");
  assert.match(rows[0].textContent, /1,155 chunks · 228 pages · 12 figure captions/);
  // The second document has no captions, so it says nothing about figures.
  assert.match(rows[1].textContent, /1,464 chunks · 228 pages/);
});

test("a single count is not pluralised", async () => {
  const { dom } = page([], {
    documents: [
      {
        filename: "one-page.pdf",
        path: "/docs/one-page.pdf",
        chunks: 1,
        pages: 1,
        figures: 1,
        sourceAvailable: true,
      },
    ],
  });
  await tick();
  assert.match(
    doc(dom).querySelector(".doc-meta").textContent,
    /1 chunk · 1 page · 1 figure caption\b/,
  );
});

test("the selected document is marked the way the stylesheet keys it", async () => {
  const { dom } = page([]);
  await tick();
  const rows = doc(dom).querySelectorAll(".doc");
  assert.equal(rows[0].hasAttribute("aria-current"), false);
  click(dom, ".doc-main");
  await tick();
  // CSS highlights .doc[aria-current="true"]; a different attribute name here
  // means no selection is ever visible, and no test would notice.
  assert.equal(doc(dom).querySelectorAll(".doc")[0].getAttribute("aria-current"), "true");
  assert.equal(doc(dom).querySelectorAll(".doc")[1].hasAttribute("aria-current"), false);
});

test("the MCP sheet shows the real command and closes cleanly", async () => {
  const calls = [];
  const { dom } = page(calls);
  await tick();
  click(dom, "#mcp-open");
  await tick();
  const pre = text(dom, "#mcp-json");
  assert.match(pre, /corpus-mcp/);
  assert.match(pre, /"enabled": true/);
  assert.match(pre, /"timeout": 180/);
  assert.equal(calls.filter((call) => call.name === "mcp_config").length, 1);
  click(dom, "#mcp-done");
  await tick();
  assert.equal(doc(dom).querySelector("#mcp-dialog").open, false, "the Done button exits");
});

test("copying the MCP config selects it when the clipboard is unavailable", async () => {
  const { dom } = page([]);
  await tick();
  click(dom, "#mcp-open");
  await tick();
  click(dom, "#mcp-copy");
  await tick();
  const selection = doc(dom).getSelection();
  assert.equal(selection.rangeCount, 1);
  assert.match(String(selection), /corpus-mcp/);
  assert.match(text(dom, "#mcp-note"), /copy/i);
});
