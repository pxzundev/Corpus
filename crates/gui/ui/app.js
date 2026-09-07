/* Front end for the knowledge base browser.
 *
 * Two rules hold throughout:
 * - Document text comes from PDFs, so it is untrusted. Every string reaches the
 *   DOM through textContent; nothing built from file content is assigned to
 *   innerHTML, so a PDF containing markup cannot execute.
 * - The document list is re-read after every mutation. A browser showing
 *   passages that were just removed is worse than one that reloads.
 */
const {
  core: { invoke },
  event: { listen },
} = window.__TAURI__;

const PAGE_SIZE = 40;
const view = {
  document: null,
  filter: "",
  // Page filter from a citation click; metadata, not text, so the pane can
  // land on a page that has no distinctive opening words.
  page: null,
  offset: 0,
  total: 0,
  documents: [],
  status: null,
  busy: false,
};

/* Folders whose card list is folded in the documents pane. */
const collapsedFolders = new Set();

/* Custom display names for folders, keyed by path. Only the label changes;
 * grouping, tooltips and the grounding set keep using the real path.
 * Both directions are guarded: a webview without storage just forgets the
 * names between runs. */
function readFolderLabels() {
  try {
    return new Map(JSON.parse(window.localStorage.getItem("corpus.folderLabels") ?? "[]"));
  } catch {
    return new Map();
  }
}

const folderLabels = readFolderLabels();

function saveFolderLabels() {
  try {
    window.localStorage.setItem("corpus.folderLabels", JSON.stringify([...folderLabels]));
  } catch {
    /* in-memory labels only */
  }
}

/* Hand-made groups. A group is a name the user chose; documents are assigned
 * to it by filename and keep the group even if the file moves on disk.
 * nextAddGroup is the one-shot destination for the next Add documents. */
const CUSTOM_GROUP_PREFIX = "custom:";

function loadJson(key, fallback) {
  try {
    return JSON.parse(window.localStorage.getItem(key) ?? fallback);
  } catch {
    return JSON.parse(fallback);
  }
}

function persistJson(key, value) {
  try {
    window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* in-memory only */
  }
}

/* One-time migration from the app's former name: copy "rag.*" values into
 * "corpus.*" keys, then drop the old ones. */
function migrateStorage() {
  const pairs = [
    ["corpus.docGroups", "rag.docGroups"],
    ["corpus.customGroups", "rag.customGroups"],
    ["corpus.folderLabels", "rag.folderLabels"],
  ];
  for (const [key, oldKey] of pairs) {
    try {
      const old = window.localStorage.getItem(oldKey);
      if (old !== null && window.localStorage.getItem(key) === null) {
        window.localStorage.setItem(key, old);
        window.localStorage.removeItem(oldKey);
      }
    } catch {
      /* no storage: nothing to migrate */
    }
  }
}

migrateStorage();

const docGroups = new Map(loadJson("corpus.docGroups", "[]"));
const customGroups = new Set(loadJson("corpus.customGroups", "[]"));
let nextAddGroup = null;
let pendingAddTarget = null;

function saveDocGroups() {
  persistJson("corpus.docGroups", [...docGroups]);
}

function saveCustomGroups() {
  persistJson("corpus.customGroups", [...customGroups]);
}

const el = (id) => document.getElementById(id);
const nodes = {
  stats: el("stats"),
  statsInfo: el("stats-info"),
  statsPopover: el("stats-popover"),
  docList: el("doc-list"),
  chunksHeading: el("chunks-heading"),
  chunkFilter: el("chunk-filter"),
  chunkCount: el("chunk-count"),
  chunkScroll: el("chunk-scroll"),
  chunkList: el("chunk-list"),
  loadMore: el("load-more"),
  knowledgeDialog: el("knowledge-dialog"),
  knowledgeClose: el("knowledge-close"),
  chatLog: el("chat-log"),
  chatJump: el("chat-jump"),
  chatEmpty: el("chat-empty"),
  chatForm: el("chat-form"),
  chatInput: el("chat-input"),
  chatSend: el("chat-send"),
  chatSessionList: el("chat-session-list"),
  sessionsToggle: el("sessions-toggle"),
  panes: el("panes"),
  chatNew: el("chat-new"),
  chatRenameDialog: el("chat-rename-dialog"),
  chatRenameTitle: el("chat-rename-title"),
  chatRenameInput: el("chat-rename-input"),
  chatRenameOk: el("chat-rename-ok"),
  chatRenameCancel: el("chat-rename-cancel"),
  activity: el("activity"),
  chunkActivity: el("chunks-activity"),
  visionSpinner: el("vision-spinner"),
  toast: el("toast"),
  confirm: el("confirm-remove"),
  confirmTitle: el("confirm-title"),
  confirmBody: el("confirm-body"),
  confirmOk: el("confirm-ok"),
  confirmCancel: el("confirm-cancel"),
  addFiles: el("add-files"),
  addFolder: el("add-folder"),
  groupNew: el("group-new"),
  chunkEmpty: el("chunk-empty"),
  visionOpen: el("vision-open"),
  visionState: el("vision-state"),
  mcpOpen: el("mcp-open"),
  mcpDialog: el("mcp-dialog"),
  mcpClose: el("mcp-close"),
  mcpDone: el("mcp-done"),
  mcpCopy: el("mcp-copy"),
  mcpJson: el("mcp-json"),
  mcpNote: el("mcp-note"),
  visionDialog: el("vision-dialog"),
  visionClose: el("vision-close"),
  visionDone: el("vision-done"),
  visionEnabled: el("vision-enabled"),
  visionUrl: el("vision-url"),
  visionModelSelect: el("vision-model"),
  visionModelManual: el("vision-model-manual"),
  visionKey: el("vision-key"),
  visionTest: el("vision-test"),
  visionNote: el("vision-note"),
  captionPrompt: el("caption-prompt"),
  captionBody: el("caption-prompt-body"),
  captionWarn: el("caption-prompt-warn"),
  captionYes: el("caption-prompt-yes"),
  captionText: el("caption-prompt-text"),
  captionCancel: el("caption-prompt-cancel"),
};

const ICONS = {
  open:
    '<path d="M14 4h6v6"/><path d="M20 4 11 13"/><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
  remove:
    '<path d="M4 7h16"/><path d="M9 7V5h6v2"/><path d="M6 7l1 13h10l1-13"/><path d="M10 11v6M14 11v6"/>',
  search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
  edit: '<path d="M4 20h4L20 8l-4-4L4 16z"/>',
  chevron: '<path d="M9 6l6 6-6 6"/>',
};

const number = (value) => (value ?? 0).toLocaleString();
const plural = (count, word) => (count === 1 ? word : `${word}s`);

/* ---------- feedback ---------- */

let toastTimer;
let refreshTimer;
function toast(message, kind = "ok", action = null) {
  clearTimeout(toastTimer);
  nodes.toast.dataset.kind = kind;
  nodes.toast.dataset.visible = "true";
  // Errors interrupt; confirmations only announce.
  nodes.toast.setAttribute("role", kind === "error" ? "alert" : "status");
  nodes.toast.replaceChildren();
  nodes.toast.append(message);
  nodes.toast.classList.toggle("has-action", Boolean(action));
  if (action) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "toast-action";
    button.textContent = action.label;
    button.addEventListener("click", () => action.onClick());
    nodes.toast.append(button);
  }
  // An action outlives a plain notice: the window to undo is long enough to
  // read the toast, glance at what it cost, and press it.
  toastTimer = setTimeout(() => {
    nodes.toast.dataset.visible = "false";
  }, action ? 8000 : 3200);
}

const setBusy = (pane, busy) => pane.setAttribute("aria-busy", String(busy));

/* One busy signal, reused by every wait: spinner, words, a meter when the work
 * reports a fraction, and an elapsed count when it cannot. `delay` keeps a
 * command that usually lands in a frame from flashing an indicator that would
 * mean nothing. */
function activity(row) {
  const text = row.querySelector(".activity-text");
  const meter = row.querySelector(".activity-meter");
  let showTimer;
  let elapsedTimer;

  return {
    show(label, { fraction, delay = 0, tick = false } = {}) {
      clearTimeout(showTimer);
      if (delay) {
        showTimer = setTimeout(() => paint(label, { fraction, tick }), delay);
        return;
      }
      paint(label, { fraction, tick });
    },
    hide() {
      clearTimeout(showTimer);
      clearInterval(elapsedTimer);
      elapsedTimer = undefined;
      row.hidden = true;
      text.textContent = "";
      if (meter) meter.style.width = "0%";
    },
  };

  function paint(label, { fraction, tick }) {
    row.hidden = false;
    clearInterval(elapsedTimer);
    elapsedTimer = undefined;

    if (meter && Number.isFinite(fraction)) {
      const percent = Math.round(fraction * 100);
      meter.style.width = `${percent}%`;
      meter.setAttribute("aria-valuenow", String(percent));
      text.textContent = label;
      return;
    }
    if (!tick) {
      text.textContent = label;
      return;
    }
    // Counting figures spends most of its time extracting text, which reports
    // the page count up front and no position afterwards. The seconds at least
    // prove the wait is moving.
    const since = Date.now();
    draw(label, since);
    elapsedTimer = setInterval(() => draw(label, since), 1000);
  }

  function draw(label, since) {
    const seconds = Math.floor((Date.now() - since) / 1000);
    text.textContent = seconds > 0 ? `${label} (${seconds}s)` : label;
  }
}

const ingestActivity = activity(nodes.activity);
const chunkActivity = activity(nodes.chunkActivity);

/* Passage reads and searches are local, so their indicator waits 200ms. */
const LOCAL_DELAY = 200;

function hideActivity() {
  clearTimeout(refreshTimer);
  ingestActivity.hide();
  setAddEnabled(true);
}

function setAddEnabled(enabled) {
  nodes.addFiles.disabled = !enabled;
  nodes.addFolder.disabled = !enabled;
}

function svgIcon(name) {
  const element = document.createElement("span");
  element.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[name]}</svg>`;
  return element.firstElementChild;
}

function iconButton(name, label, handler, variant = "") {
  const button = document.createElement("button");
  button.type = "button";
  button.className = `icon-btn ${variant}`.trim();
  button.title = label;
  button.setAttribute("aria-label", label);
  button.append(svgIcon(name));
  button.addEventListener("click", handler);
  return button;
}

/* ---------- index status and document list ---------- */

function renderStatus(status) {
  if (!status || !status.indexPresent) {
    nodes.stats.textContent = `No index yet · ${status?.model ?? "unknown model"} will download on first add`;
    return;
  }
  nodes.stats.textContent =
    `${number(status.chunks)} chunks · ${number(status.documents)} documents · ` +
    `${status.model} ${status.dimension}-dim · chunker v${status.chunkerVersion}`;
  nodes.stats.title = `Index: ${status.indexPath}\nModels: ${status.modelsPath}`;
}

function renderDocuments() {
  nodes.docList.replaceChildren();
  nodes.docList.removeAttribute("aria-busy");
  const present = new Set(view.documents.map((doc) => doc.filename));
  for (const name of excludedDocs) {
    if (!present.has(name)) excludedDocs.delete(name);
  }

  if (!view.documents.length) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent =
      "No documents indexed. Add documents picks individual files; Add folder indexes a whole tree.";
    nodes.docList.append(empty);
    return;
  }

  const groups = new Map();
  for (const doc of view.documents) {
    const dir = folderOf(doc);
    if (!groups.has(dir)) groups.set(dir, []);
    groups.get(dir).push(doc);
  }
  // Hand-made groups stay on the shelf even while empty.
  for (const name of customGroups) {
    const key = `${CUSTOM_GROUP_PREFIX}${name}`;
    if (!groups.has(key)) groups.set(key, []);
  }
  const folders = [...groups.keys()].sort(
    (a, b) => (a === "") - (b === "") || folderLabel(a).localeCompare(folderLabel(b)),
  );
  for (const folder of folders) {
    const docs = groups.get(folder);
    const card = folderHeader(folder, docs);
    if (collapsedFolders.has(folder)) {
      card.classList.add("is-collapsed");
    } else if (docs.length) {
      const items = document.createElement("ul");
      items.className = "doc-items";
      for (const doc of docs) items.append(docRow(doc));
      card.append(items);
    } else {
      const blank = document.createElement("p");
      blank.className = "doc-group-blank";
      blank.textContent = "Nothing in this group yet — Add documents sends its next files here.";
      card.append(blank);
    }
    nodes.docList.append(card);
  }
}

/* The folder a document was indexed from; loose files share a label. A
 * hand-made group wins over the path once a file has been assigned one. */
function folderOf(doc) {
  const named = docGroups.get(doc.filename);
  if (named) return `${CUSTOM_GROUP_PREFIX}${named}`;
  const path = String(doc.path || "").replace(/\\/g, "/");
  const cut = path.lastIndexOf("/");
  return cut > 0 ? path.slice(0, cut) : "";
}

function folderLabel(dir) {
  if (dir.startsWith(CUSTOM_GROUP_PREFIX)) return dir.slice(CUSTOM_GROUP_PREFIX.length);
  const custom = folderLabels.get(dir);
  if (custom) return custom;
  if (!dir) return "Added one at a time";
  const parts = dir.split("/").filter(Boolean);
  return parts.at(-1) || dir;
}

function folderHeader(dir, docs) {
  const collapsed = collapsedFolders.has(dir);
  const header = document.createElement("li");
  header.className = "doc-group";

  const check = document.createElement("input");
  check.type = "checkbox";
  check.className = "doc-group-check";
  check.title = "Ticked groups ground the chat answers; unticked ones do not";
  check.dataset.folder = dir;
  check.setAttribute("aria-label", `Use every document in ${folderLabel(dir)} to ground chat answers`);
  check.addEventListener("change", () => {
    if (!docs.length) {
      updateFolderCheck(check, docs);
      return;
    }
    const on = check.checked; // an indeterminate box clicks to fully on
    for (const doc of docs) {
      if (on) excludedDocs.delete(doc.filename);
      else excludedDocs.add(doc.filename);
    }
    // The rows follow the group: dim while unticked, plain while ticked.
    for (const row of nodes.docList.querySelectorAll(".doc")) {
      if (row.dataset.folder === dir) row.classList.toggle("off", !on);
    }
    toast(
      on
        ? `${folderLabel(dir)}: every document grounds answers`
        : `${folderLabel(dir)}: no document grounds answers`,
    );
  });

  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = "doc-group-toggle";
  toggle.setAttribute("aria-expanded", String(!collapsed));
  toggle.title = dir.startsWith(CUSTOM_GROUP_PREFIX)
    ? `A group you made${
        nextAddGroup === folderLabel(dir) ? " — your next Add documents lands here" : ""
      }`
    : dir || "Documents added one at a time, without a folder";

  const name = document.createElement("span");
  name.className = "doc-group-name";
  name.textContent = folderLabel(dir);

  const count = document.createElement("span");
  count.className = "doc-group-count";
  count.textContent = `${docs.length} ${plural(docs.length, "doc")}`;

  toggle.append(svgIcon("chevron"), name, count);
  toggle.addEventListener("click", () => {
    if (collapsedFolders.has(dir)) collapsedFolders.delete(dir);
    else collapsedFolders.add(dir);
    renderDocuments();
  });

  const rename = iconButton(
    "edit",
    `Rename the ${folderLabel(dir)} folder`,
    () => openFolderRename(dir, folderLabel(dir)),
  );
  rename.classList.add("doc-group-rename");

  const drop = iconButton(
    "remove",
    dir.startsWith(CUSTOM_GROUP_PREFIX)
      ? `Delete the ${folderLabel(dir)} group and its documents`
      : `Remove every document indexed from ${folderLabel(dir)}`,
    () => askRemoveGroup(dir, docs),
    "danger",
  );
  drop.classList.add("doc-group-remove");

  const tools = document.createElement("div");
  tools.className = "doc-group-tools";
  tools.append(rename, drop);

  const bar = document.createElement("div");
  bar.className = "doc-group-bar";
  bar.append(check, toggle, tools);
  header.append(bar);
  updateFolderCheck(check, docs);
  return header;
}

/* The header tick mirrors its cards: fully on, fully off, or a partial
 * dash when only some of the folder's documents ground answers. */
function updateFolderCheck(check, docs) {
  const included = docs.filter((doc) => !excludedDocs.has(doc.filename)).length;
  check.checked = docs.length > 0 && included === docs.length;
  check.indeterminate = included > 0 && included < docs.length;
}

function updateFolderChecks() {
  for (const check of nodes.docList.querySelectorAll(".doc-group-check")) {
    updateFolderCheck(
      check,
      view.documents.filter((doc) => folderOf(doc) === check.dataset.folder),
    );
  }
}

function docRow(doc) {
    const item = document.createElement("li");
    item.className = "doc";
    item.dataset.folder = folderOf(doc);
    if (doc.filename === view.document) item.setAttribute("aria-current", "true");
    const ticked = !excludedDocs.has(doc.filename);
    if (!ticked) item.classList.add("off");

    const main = document.createElement("button");
    main.type = "button";
    main.className = "doc-main";
    main.addEventListener("click", () => selectDocument(doc.filename));

    const name = document.createElement("span");
    name.className = "doc-name";
    name.textContent = doc.filename;
    name.title = doc.path || doc.filename;

    const meta = document.createElement("span");
    meta.className = "doc-meta";
    meta.textContent =
      `${number(doc.chunks)} ${plural(doc.chunks, "chunk")} · ${number(doc.pages)} ${plural(doc.pages, "page")}` +
      (doc.figures ? ` · ${number(doc.figures)} ${plural(doc.figures, "figure caption")}` : "");

    main.append(name, meta);

    const tools = document.createElement("div");
    tools.className = "doc-tools";
    if (doc.sourceAvailable) {
      tools.append(
        iconButton("open", `Open ${doc.filename}`, () =>
          invoke("open_source", { filename: doc.filename }).catch((error) =>
            toast(String(error), "error"),
          ),
        ),
      );
    } else {
      // Inline under the counts: on the tools row it read as part of the buttons.
      const missing = document.createElement("span");
      missing.className = "doc-missing";
      missing.textContent = "original not found";
      missing.title = doc.path;
      main.append(missing);
    }
    tools.append(iconButton("remove", `Remove ${doc.filename} from the index`, () => askRemove(doc), "danger"));

    item.append(main, tools);
    return item;
}

/* ---------- chunk browser ---------- */

async function loadChunks({ append = false } = {}) {
  if (!view.document || view.busy) return;
  view.busy = true;
  setBusy(nodes.chunkScroll, true);
  chunkActivity.show(
    view.filter
      ? "Filtering passages…"
      : view.offset
        ? "Loading more passages…"
        : "Loading passages…",
    { delay: LOCAL_DELAY },
  );

  try {
    const offset = append ? view.offset : 0;
    const page = await invoke("browse", {
      document: view.document,
      filter: view.filter || null,
      page: view.page,
      offset,
      limit: PAGE_SIZE,
    });

    view.total = page.total;
    view.offset = offset + page.items.length;
    if (!append) nodes.chunkList.replaceChildren();
    for (const chunk of page.items) nodes.chunkList.append(chunkCard(chunk));

    setHeadingFor(page.document);
    nodes.chunkCount.textContent = page.total
      ? `${number(view.offset)} of ${number(page.total)} passages` +
        (view.page ? ` on page ${view.page}` : "") +
        (view.filter ? ` matching “${view.filter}”` : "")
      : view.page
        ? `Nothing is indexed on page ${view.page}.`
        : view.filter
          ? `No passage matches “${view.filter}”`
          : "This document has no indexed text.";
    // An empty pane with a "Load more" button in it explains nothing.
    nodes.chunkEmpty.hidden = page.items.length > 0;
    nodes.chunkEmpty.textContent = page.items.length
      ? ""
      : view.page
        ? `The citation named page ${view.page} of ${page.document}, but no chunk carries that page.`
        : view.filter
          ? `Nothing in ${page.document} matches “${view.filter}”.`
          : "This document has no indexed text — its pages produced no chunks.";
    nodes.loadMore.hidden = view.offset >= page.total;
  } catch (error) {
    toast(String(error), "error");
  } finally {
    view.busy = false;
    setBusy(nodes.chunkScroll, false);
    chunkActivity.hide();
  }
}

function setHeadingFor(filename) {
  nodes.chunksHeading.textContent = filename ?? "Knowledge";
  nodes.chunksHeading.title = filename ?? "";
}

/// A caption is a model's reading of a drawing, so the viewer has to be able to
/// tell it apart from printed body text at a glance.
function figureBadge() {
  const badge = document.createElement("span");
  badge.className = "chunk-figure";
  badge.textContent = "figure caption";
  badge.title =
    "A vision model's description of a drawing on this page, not text printed on it.";
  return badge;
}

function chunkCard(chunk) {
  const item = document.createElement("li");
  item.className = "chunk";
  item.dataset.chunkId = chunk.id;

  const head = document.createElement("div");
  head.className = "chunk-head";

  const page = document.createElement("span");
  page.className = "chunk-page";
  page.textContent = chunk.page === -1 ? "whole document" : `page ${chunk.page}`;

  const words = document.createElement("span");
  words.textContent = `${number(chunk.words)} words`;

  const id = document.createElement("span");
  id.className = "chunk-id";
  id.textContent = chunk.id;
  id.title = chunk.id;

  head.append(page, words, id);
  if (chunk.figure) head.insertBefore(figureBadge(), words);

  const text = document.createElement("p");
  text.className = "chunk-text";
  text.textContent = chunk.text;

  item.append(head, text);
  if (chunk.text.length > 600) item.append(expandButton(item, text, "4 lines"));
  return item;
}

/// Collapsed passages need an explicit control; changing the line clamp alone
/// gives no indication of what happened.
function expandButton(item, text, _hint) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "chunk-expand";
  button.textContent = "Expand";
  button.setAttribute("aria-expanded", "false");
  button.addEventListener("click", () => {
    const open = item.getAttribute("aria-expanded") === "true";
    item.setAttribute("aria-expanded", String(!open));
    button.setAttribute("aria-expanded", String(!open));
    button.textContent = open ? "Expand" : "Collapse";
  });
  return button;
}

async function selectDocument(filename) {
  if (view.busy) return;
  // The passage list is a modal now: opening a document is opening the modal.
  openDialog(nodes.knowledgeDialog);
  view.document = filename;
  view.filter = "";
  view.page = null;
  view.offset = 0;
  nodes.chunkFilter.value = "";
  setHeadingFor(filename);
  renderDocuments();
  nodes.chunkList.replaceChildren();
  await loadChunks();
}

/* ---------- mutations ---------- */

let pendingRemoval = null; // {kind: "doc", doc} or {kind: "group", dir, docs, named}

/// jsdom implements HTMLDialogElement without showModal/close, so feature-detect
/// and fall back to the open attribute; the webview always takes the first branch.
function openDialog(dialog) {
  if (typeof dialog.showModal === "function") {
    dialog.showModal();
  } else {
    dialog.open = true;
  }
}

function closeDialog(dialog) {
  if (typeof dialog.close === "function" && dialog.open) {
    dialog.close();
  } else {
    dialog.open = false;
  }
}

function askRemove(doc) {
  pendingRemoval = { kind: "doc", doc };
  nodes.confirmTitle.textContent = "Remove from the index?";
  nodes.confirmBody.textContent =
    `${doc.filename} — ${number(doc.chunks)} chunks across ${number(doc.pages)} pages ` +
    "will stop being searchable.";
  openDialog(nodes.confirm);
  nodes.confirmCancel.focus();
}

function askRemoveGroup(dir, docs) {
  const named = dir.startsWith(CUSTOM_GROUP_PREFIX);
  pendingRemoval = { kind: "group", dir, docs, named };
  const label = folderLabel(dir);
  const chunks = docs.reduce((total, doc) => total + (doc.chunks || 0), 0);
  nodes.confirmTitle.textContent = "Remove from the index?";
  nodes.confirmBody.textContent = !docs.length
    ? `The empty group “${label}” will be deleted.`
    : named
      ? `Group “${label}” — ${number(docs.length)} document(s), ${number(chunks)} chunks — ` +
        "will stop being searchable, and the group will be deleted."
      : `All ${number(docs.length)} document(s) indexed from ${label} — ${number(chunks)} chunks — ` +
        "will stop being searchable.";
  openDialog(nodes.confirm);
  nodes.confirmCancel.focus();
}

function closeConfirm() {
  closeDialog(nodes.confirm);
}

async function confirmRemove() {
  const request = pendingRemoval;
  closeConfirm();
  if (!request) return;
  try {
    if (request.kind === "doc") await removeDocument(request.doc);
    else await removeGroup(request);
  } finally {
    pendingRemoval = null;
    hideActivity();
  }
}

/* The open reading pane has to let go of a document that left the index. */
function forgetDocument(filename) {
  if (view.document !== filename) return;
  view.document = null;
  nodes.chunkList.replaceChildren();
  nodes.chunkCount.textContent = "";
  setHeadingFor(null);
}

async function removeDocument(doc) {
  ingestActivity.show(`Removing ${doc.filename}…`, { fraction: 0.5 });
  try {
    const removed = await invoke("remove_document", { filename: doc.filename });
    forgetDocument(doc.filename);
    await refresh();
    toast(`Removed ${number(removed)} chunks from the index`);
  } catch (error) {
    toast(String(error), "error");
  }
}

async function removeGroup({ dir, docs, named }) {
  const label = folderLabel(dir);
  const deleted = [];
  const failures = [];
  let chunks = 0;
  if (docs.length) {
    ingestActivity.show(`Removing ${label}…`, { fraction: 0 });
    for (const doc of docs) {
      try {
        chunks += (await invoke("remove_document", { filename: doc.filename })) ?? 0;
        deleted.push(doc.filename);
      } catch {
        failures.push(doc.filename);
      }
    }
  }
  if (named) {
    const name = dir.slice(CUSTOM_GROUP_PREFIX.length);
    if (!failures.length) {
      customGroups.delete(name);
      if (nextAddGroup === name) nextAddGroup = null;
    }
    for (const filename of deleted) docGroups.delete(filename);
    saveCustomGroups();
    saveDocGroups();
  }
  for (const filename of deleted) forgetDocument(filename);
  await refresh();
  if (failures.length) {
    toast(
      `${label}: removed ${deleted.length} of ${docs.length}; ${failures.join(", ")} failed`,
      "error",
    );
  } else if (!docs.length) {
    toast(`Group “${label}” deleted`);
  } else if (named) {
    toast(`Removed ${number(chunks)} chunks and deleted the ${label} group`);
  } else {
    toast(`Removed ${number(chunks)} chunks from ${label}`);
  }
}

/* ---------- vision settings ---------- */

let pendingCaption = null;

function visionNote(message, ok = true) {
  nodes.visionNote.textContent = message;
  nodes.visionNote.classList.toggle("is-error", !ok);
  nodes.visionNote.classList.toggle("is-ok", ok && Boolean(message));
}

/// The header has room for state, not for a form: whether captioning will run
/// on the next ingest, and whether it is even possible here.
function renderVisionState(settings) {
  const state = !settings.pdfium ? "unavailable" : settings.enabled ? "on" : "off";
  nodes.visionState.dataset.state = state;
  nodes.visionState.textContent = state === "unavailable" ? "no PDFium" : state;
  nodes.visionOpen.title =
    state === "on"
      ? `Captions figures with ${settings.model}`
      : state === "unavailable"
        ? "PDFium is not installed, so figures cannot be read"
        : "Figures are indexed as printed text only";
}

async function loadVision() {
  try {
    const settings = await invoke("vision_settings");
    nodes.visionEnabled.checked = Boolean(settings.enabled);
    nodes.visionUrl.value = settings.baseUrl ?? "";
    // The model box is a dropdown fed by the endpoint's own listing; until that
    // listing answers, the typed box is what shows, so nothing is lost.
    nodes.visionModelManual.value = settings.model ?? "";
    nodes.visionModelManual.hidden = false;
    nodes.visionModelSelect.hidden = true;
    nodes.visionModelSelect.replaceChildren();
    nodes.visionKey.value = "";
    nodes.visionKey.placeholder = settings.apiKeySet
      ? "saved — type a new one to replace it"
      : "only if the endpoint needs one";
    renderVisionState(settings);
    await refreshVisionModels();
    if (!settings.pdfium) {
      visionNote(
        "PDFium is not installed, so figures cannot be rasterised or captioned. " +
          "Put libpdfium in the data dir or set RAG_PDFIUM_PATH.",
        false,
      );
    } else {
      visionNote("");
    }
  } catch (error) {
    visionNote(String(error), false);
  }
}

/// Fill the dropdown from `GET {endpoint}/models`. The saved model always has
/// an entry even if the server stopped advertising it, so switching engines
/// does not silently rewrite the setting. Any failure — engine down, no such
/// path — puts the typed box back instead.
async function refreshVisionModels() {
  try {
    const models = await invoke("vision_models");
    if (!models.length) throw new Error("the endpoint lists no models");
    const select = nodes.visionModelSelect;
    select.replaceChildren();
    const saved = nodes.visionModelManual.value.trim();
    for (const id of models) {
      const option = document.createElement("option");
      option.value = id;
      option.textContent = id;
      select.append(option);
    }
    if (saved && !models.includes(saved)) {
      const option = document.createElement("option");
      option.value = saved;
      option.textContent = saved;
      select.append(option);
    }
    select.value = saved && models.includes(saved) ? saved : (models[0] ?? "");
    select.hidden = false;
    nodes.visionModelManual.hidden = true;
  } catch (error) {
    nodes.visionModelManual.hidden = false;
    nodes.visionModelSelect.hidden = true;
  }
}

async function saveVision() {
  try {
    const settings = await invoke("save_vision_settings", {
      enabled: nodes.visionEnabled.checked,
      baseUrl: nodes.visionUrl.value,
      model:
        nodes.visionModelSelect.hidden
          ? nodes.visionModelManual.value
          : nodes.visionModelSelect.value,
      // An empty box means "keep what is stored", which the backend enforces.
      apiKey: nodes.visionKey.value,
    });
    nodes.visionKey.value = "";
    nodes.visionKey.placeholder = settings.apiKeySet
      ? "saved — type a new one to replace it"
      : "only if the endpoint needs one";
    visionNote("Saved.", true);
    renderVisionState(settings);
  } catch (error) {
    visionNote(String(error), false);
  }
}

/// The sheet is read-only, so every open re-reads where the server ended up.
function openMcp() {
  openDialog(nodes.mcpDialog);
  loadMcp();
}

async function loadMcp() {
  nodes.mcpNote.textContent = "";
  nodes.mcpNote.classList.remove("is-error", "is-ok");
  nodes.mcpJson.textContent = "Loading…";
  try {
    const info = await invoke("mcp_config");
    nodes.mcpJson.textContent = info.json;
  } catch (error) {
    nodes.mcpJson.textContent = "";
    mcpNote(String(error), false);
  }
}

function mcpNote(message, ok = true) {
  nodes.mcpNote.textContent = message;
  nodes.mcpNote.classList.toggle("is-error", !ok);
  nodes.mcpNote.classList.toggle("is-ok", ok && Boolean(message));
}

/// WKWebView (and jsdom) may deny clipboard writes, so the fallback selects the
/// JSON — a manual copy is then one keystroke.
async function copyMcp() {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(nodes.mcpJson.textContent);
      mcpNote("Copied to the clipboard.", true);
      return;
    }
  } catch (error) {
    // fall through to the manual fallback
  }
  const range = document.createRange();
  range.selectNodeContents(nodes.mcpJson);
  const selection = document.getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
  mcpNote("Press Cmd/Ctrl+C to copy the selected JSON.", true);
}

/// Closing the sheet stores what is in the fields; Escape is the discard path.
async function commitVision() {
  await saveVision();
  closeDialog(nodes.visionDialog);
}

function openVision() {
  openDialog(nodes.visionDialog);
  nodes.visionEnabled.focus();
}

/// Advertised capabilities have already proved unreliable, so the only test is
/// sending a real image and reading back what the model said about it.
async function testVision() {
  nodes.visionTest.disabled = true;
  nodes.visionSpinner.hidden = false;
  visionNote("Sending a test image…");
  try {
    await saveVision();
    const answer = await invoke("test_vision");
    visionNote(`It saw: ${answer}`, true);
  } catch (error) {
    visionNote(String(error), false);
  } finally {
    nodes.visionSpinner.hidden = true;
    nodes.visionTest.disabled = false;
  }
}

/* ---------- adding documents ---------- */

async function finishIngest(summary) {
  if (summary.cancelled) {
    toast("Cancelled — nothing was added");
    return;
  }
  await refresh();
  const parts = [
    `Indexed ${number(summary.indexed.length)} file(s)`,
    `${number(summary.newChunks)} new chunks`,
    `${number(summary.totalChunks)} chunks in ${number(summary.totalDocuments)} documents`,
  ];
  if (summary.figures) {
    parts.push(
      summary.captionsMade
        ? `${number(summary.captionsMade)} figure caption(s) made`
        : `${number(summary.figures)} figure page(s) left uncaptioned`,
    );
  }
  if (summary.captionFailures) {
    parts.push(`${number(summary.captionFailures)} caption failure(s)`);
  }
  toast(parts.join(" · "), summary.captionFailures ? "error" : "ok");

  // Files chosen by hand land in the group the user just made; folders
  // group themselves by their own name instead.
  if (pendingAddTarget && summary.indexed.length) {
    const group = pendingAddTarget;
    pendingAddTarget = null;
    nextAddGroup = null;
    for (const file of summary.indexed) {
      docGroups.set(String(file).split("/").pop(), group);
    }
    saveDocGroups();
    renderDocuments();
    toast(`${number(summary.indexed.length)} file(s) joined ${group}`);
  }
}

/// Captioning is the expensive part of ingest and the choice is per batch, so
/// the window asks with real numbers rather than assuming.
function showCaptionPrompt(pending) {
  pendingCaption = pending;
  nodes.captionBody.textContent =
    `${number(pending.figures)} figure page(s) in these ${number(pending.documents)} document(s). ` +
    `Captioning them with ${pending.model} takes about ${pending.captionMinutesLow}–` +
    `${pending.captionMinutesHigh} minutes the first time; afterwards it is cached.`;

  if (pending.pdfium) {
    nodes.captionWarn.hidden = true;
    nodes.captionYes.disabled = false;
  } else {
    nodes.captionWarn.hidden = false;
    nodes.captionWarn.textContent =
      "PDFium is not installed, so figures cannot be read and only printed text will be " +
      "indexed. Install it, then remove and re-add these documents to caption them.";
    nodes.captionYes.disabled = true;
  }

  setAddEnabled(false);
  openDialog(nodes.captionPrompt);
  (pending.captionDefault && !nodes.captionYes.disabled
    ? nodes.captionYes
    : nodes.captionText
  ).focus();
}

function closeCaptionPrompt() {
  pendingCaption = null;
  closeDialog(nodes.captionPrompt);
  setAddEnabled(true);
}

async function commitCaption(caption) {
  const pending = pendingCaption;
  if (!pending) return;
  pendingCaption = null;
  closeDialog(nodes.captionPrompt);

  ingestActivity.show(caption ? "Captioning figures" : "Indexing text", { fraction: 0 });
  setAddEnabled(false);
  try {
    const summary = await invoke("commit_ingest", { token: pending.token, caption });
    await finishIngest(summary);
  } catch (error) {
    toast(String(error), "error");
  } finally {
    hideActivity();
    setAddEnabled(true);
  }
}

/// Native picker. Returns either an ingest summary or, when the documents hold
/// figures, a cost estimate that becomes a question.
async function addDocuments(scope) {
  // Only hand-picked files honour a named group; a folder names itself.
  pendingAddTarget = scope === "folder" ? null : nextAddGroup;
  ingestActivity.show(
    scope === "folder" ? "Choose a folder to index" : "Choose documents to index",
    { fraction: 0 },
  );
  nodes.addFiles.disabled = true;
  nodes.addFolder.disabled = true;
  try {
    const result = await invoke("add_documents", { scope });
    // A pending selection carries a token; the paths themselves stay in Rust.
    if (result?.token) {
      hideActivity();
      showCaptionPrompt(result);
      return;
    }
    await finishIngest(result);
  } catch (error) {
    toast(String(error), "error");
  } finally {
    hideActivity();
    // While the caption question is open the buttons stay locked behind it.
    if (!nodes.captionPrompt.open) setAddEnabled(true);
  }
}

/* ---------- chat ---------- */

/* Chat answers come from the local model as markdown: citations in [1] or
 * [file p.4] form, the occasional list or code fence. The model's output is untrusted like
 * file text, so it is rendered by building elements from a fixed inline grammar
 * and assigning textContent — no path from a model reply to innerHTML exists. */

function textWithMarkers(parent, source, citations, className) {
  const pattern = /\[([^\]\n]{1,140})\]/g;
  let last = 0;
  let match;
  while ((match = pattern.exec(source))) {
    if (match.index > last) {
      parent.append(document.createTextNode(source.slice(last, match.index)));
    }
    const marker = match[1];
    const target = citations && citations.get(marker);
    const chip = document.createElement(target ? "button" : "span");
    chip.className = target ? `chat-cite ${className}`.trim() : "chat-plain";
    chip.textContent = `[${marker}]`;
    if (target) {
      chip.type = "button";
      chip.title = "Open this document and find this passage";
      chip.addEventListener("click", () => locatePassage(target));
    } else {
      chip.title = "Not one of the passages retrieved for this answer";
    }
    parent.append(chip);
    last = pattern.index + match[0].length;
  }
  if (last < source.length) parent.append(document.createTextNode(source.slice(last)));
}

/* Inline formatting is a pipeline: each stage takes one markdown pattern and
 * hands what it did not match to the next stage, so a `code` span protects
 * its contents, a [link](url) wins over the citation-chip pattern, and only
 * unclaimed text finally reaches textWithMarkers. Everything ends up as text
 * nodes or explicit elements — model output never reaches innerHTML here. */
function inlineInto(parent, source, citations) {
  inlineStep(parent, String(source), citations, INLINE_STAGES);
}

function inlineStep(parent, source, citations, stages) {
  if (!stages.length) {
    textWithMarkers(parent, source, citations);
    return;
  }
  const [stage, ...rest] = stages;
  const pattern = new RegExp(stage.pattern, "g");
  let last = 0;
  let match;
  while ((match = pattern.exec(source))) {
    if (match.index > last) inlineStep(parent, source.slice(last, match.index), citations, rest);
    stage.build(parent, match, citations, rest);
    last = pattern.lastIndex;
  }
  if (last < source.length) inlineStep(parent, source.slice(last), citations, rest);
}

const safeHref = (url) => (/^(https?|mailto):/i.test(url) ? url : null);

const INLINE_STAGES = [
  {
    pattern: "`([^`\\n]+)`",
    build(parent, match) {
      const code = document.createElement("code");
      code.className = "chat-inline-code";
      code.textContent = match[1];
      parent.append(code);
    },
  },
  {
    pattern: "\\[([^\\]\\n]{1,200})\\]\\(([^)\\s]+)\\)",
    build(parent, match, citations, rest) {
      const href = safeHref(match[2]);
      if (!href) {
        inlineStep(parent, match[0], citations, rest);
        return;
      }
      const link = document.createElement("a");
      link.href = href;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      inlineStep(link, match[1], citations, rest);
      parent.append(link);
    },
  },
  {
    pattern: "\\*\\*([\\s\\S]+?)\\*\\*|__([^_\\n]+)__",
    build(parent, match, citations, rest) {
      const strong = document.createElement("strong");
      inlineStep(strong, match[1] ?? match[2], citations, rest);
      parent.append(strong);
    },
  },
  {
    pattern: "\\*([^*\\n]+)\\*|(?<=^|[\\s(])_([^_\\n]+)_(?=$|[\\s).,;:!?、。])",
    build(parent, match, citations, rest) {
      const em = document.createElement("em");
      inlineStep(em, match[1] ?? match[2], citations, rest);
      parent.append(em);
    },
  },
  {
    pattern: "~~([^~\\n]+)~~",
    build(parent, match, citations, rest) {
      const del = document.createElement("del");
      inlineStep(del, match[1], citations, rest);
      parent.append(del);
    },
  },
];

/* Block-level markdown: fenced code (with mermaid and svg as drawing
 * dialects), headings, rules, quotes, nested lists and pipe tables. */
function renderMarkdown(container, source, citations) {
  container.replaceChildren();
  const lines = String(source).split("\n");
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }

    const fence = line.trim().match(/^```([A-Za-z0-9+-]*)\s*$/);
    if (fence) {
      const language = fence[1].toLowerCase();
      const code = [];
      index += 1;
      while (index < lines.length && !lines[index].trim().startsWith("```")) {
        code.push(lines[index]);
        index += 1;
      }
      index += 1;
      appendFenced(container, language, code.join("\n"));
      continue;
    }

    if (/^ {0,3}<svg[\s>]/i.test(line)) {
      const markup = [];
      while (index < lines.length) {
        markup.push(lines[index]);
        const closed = /<\/svg\s*>/i.test(lines[index]);
        index += 1;
        if (closed) break;
      }
      appendSvg(container, markup.join("\n"));
      continue;
    }

    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      const element = document.createElement(`h${Math.min(heading[1].length + 2, 5)}`);
      element.className = "chat-heading";
      inlineInto(element, heading[2], citations);
      container.append(element);
      index += 1;
      continue;
    }

    if (/^ {0,3}(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      const rule = document.createElement("hr");
      rule.className = "chat-rule";
      container.append(rule);
      index += 1;
      continue;
    }

    if (/^ {0,3}>/.test(line)) {
      const quote = document.createElement("blockquote");
      quote.className = "chat-quote";
      while (index < lines.length && /^ {0,3}>/.test(lines[index])) {
        const quoted = document.createElement("p");
        inlineInto(quoted, lines[index].replace(/^ {0,3}>\s?/, ""), citations);
        quote.append(quoted);
        index += 1;
      }
      container.append(quote);
      continue;
    }

    if (/\|/.test(line) && index + 1 < lines.length && isTableDivider(lines[index + 1])) {
      index = appendTable(container, lines, index, citations);
      continue;
    }

    if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
      const [list, next] = buildList(lines, index, citations);
      container.append(list);
      index = next;
      continue;
    }

    const paragraph = document.createElement("p");
    inlineInto(paragraph, line, citations);
    container.append(paragraph);
    index += 1;
  }
}

const LIST_ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const listKind = (text) => (/^\s*\d+[.)]\s+/.test(text) ? "ol" : "ul");

/* A run of list lines; a deeper-indented line folds into the item above it
 * as a sublist, and a blank line only ends the list when the next line is
 * not another item. */
function buildList(lines, start, citations) {
  const base = (lines[start].match(/^\s*/)[0] || "").length;
  const kind = listKind(lines[start]);
  const list = document.createElement(kind);
  let index = start;
  let current = null;
  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      const next = lines[index + 1];
      if (next && LIST_ITEM.test(next) && next.match(/^\s*/)[0].length >= base) {
        index += 1;
        continue;
      }
      break;
    }
    const item = line.match(LIST_ITEM);
    if (!item) break;
    const indent = item[1].length;
    if (indent >= base + 2) {
      if (!current) break;
      const [nested, next] = buildList(lines, index, citations);
      current.append(nested);
      index = next;
      continue;
    }
    if (indent < base || listKind(line) !== kind) break;
    current = document.createElement("li");
    inlineInto(current, item[3], citations);
    list.append(current);
    index += 1;
  }
  return [list, index];
}

const isTableDivider = (line) =>
  /^\s*\|?\s*:?-{2,}:?\s*(?:\|\s*:?-{2,}:?\s*)+\|?\s*$/.test(line);
const tableRow = (line) =>
  line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((cell) => cell.trim());

function appendTable(container, lines, start, citations) {
  const table = document.createElement("table");
  table.className = "chat-table";
  const aligns = tableRow(lines[start + 1]).map((cell) =>
    /^:.*:$/.test(cell) ? "center" : /:$/.test(cell) ? "right" : /^:/.test(cell) ? "left" : "",
  );

  const head = document.createElement("thead");
  const headRow = document.createElement("tr");
  tableRow(lines[start]).forEach((cell, column) => {
    const th = document.createElement("th");
    if (aligns[column]) th.style.textAlign = aligns[column];
    inlineInto(th, cell, citations);
    headRow.append(th);
  });
  head.append(headRow);

  const body = document.createElement("tbody");
  let index = start + 2;
  while (index < lines.length && lines[index].trim() && /\|/.test(lines[index])) {
    const row = document.createElement("tr");
    tableRow(lines[index]).forEach((cell, column) => {
      const td = document.createElement("td");
      if (aligns[column]) td.style.textAlign = aligns[column];
      inlineInto(td, cell, citations);
      row.append(td);
    });
    body.append(row);
    index += 1;
  }

  table.append(head, body);
  container.append(table);
  return index;
}

function appendFenced(container, language, code) {
  if (language === "mermaid") {
    appendMermaid(container, code);
    return;
  }
  if (language === "svg" || language === "xml") {
    appendSvg(container, code);
    return;
  }
  const pre = document.createElement("pre");
  pre.className = "chat-code";
  const codeEl = document.createElement("code");
  codeEl.textContent = code;
  pre.append(codeEl);
  container.append(pre);
}

/* ---------- drawings the model can draw with ----------
 * The two structured escapes from the all-text rule: an <svg> the model drew
 * is parsed and filtered down to inert drawing elements, and a mermaid block
 * goes to the bundled engine under its strict security level. */

const SVG_ELEMENTS = new Set([
  "svg", "g", "defs", "path", "rect", "circle", "ellipse", "line", "polyline",
  "polygon", "text", "tspan", "marker", "use", "linearGradient", "radialGradient",
  "stop", "clipPath", "mask", "pattern", "title", "desc",
  "filter", "feGaussianBlur", "feOffset", "feFlood", "feBlend", "feComposite",
  "feColorMatrix", "feMerge", "feMergeNode", "feTile", "feTurbulence",
]);

function sanitizeSvg(markup) {
  const parsed = new DOMParser().parseFromString(markup, "image/svg+xml");
  const root = parsed.documentElement;
  if (!root || root.nodeName.toLowerCase() !== "svg") return null;
  if (parsed.getElementsByTagName("parsererror").length) return null;
  const walk = (node) => {
    for (const attribute of [...node.attributes]) {
      const name = attribute.name.toLowerCase();
      // Declared entities can hide a scheme; strip them before looking.
      const value = attribute.value.replace(/&(#?\w+);/g, "");
      const link = name === "href" || name === "xlink:href";
      if (
        name.startsWith("on") ||
        /(javascript|vbscript|data)\s*:/i.test(value) ||
        (link && !value.startsWith("#"))
      ) {
        node.removeAttribute(attribute.name);
      }
    }
    for (const child of [...node.children]) {
      if (!SVG_ELEMENTS.has(child.nodeName.toLowerCase())) child.remove();
      else walk(child);
    }
  };
  walk(root);
  return root;
}

function appendSvg(container, markup) {
  const root = sanitizeSvg(markup);
  if (!root) {
    const pre = document.createElement("pre");
    pre.className = "chat-code";
    const codeEl = document.createElement("code");
    codeEl.textContent = markup;
    pre.append(codeEl);
    container.append(pre);
    return;
  }
  root.classList.add("chat-svg");
  container.append(document.adoptNode(root));
}

let mermaidLoader = null;
let mermaidSeq = 0;
/* Diagram colours come from the same tokens as the page, so a drawing cannot
 * be light-on-light after an OS theme switch. initialize is cheap and mermaid
 * re-reads it per render call, so appendMermaid refreshes before each draw. */
function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function mermaidConfig() {
  return {
    startOnLoad: false,
    securityLevel: "strict",
    theme: "base",
    themeVariables: {
      background: cssVar("--color-bg") || "#f5f3ec",
      primaryColor: cssVar("--color-sunk") || "#efe8d9",
      primaryTextColor: cssVar("--color-fg") || "#2f2a24",
      primaryBorderColor: cssVar("--color-border-strong") || "#c8c0ae",
      lineColor: cssVar("--color-muted-fg") || "#8c8577",
      secondaryColor: cssVar("--color-accent-soft") || "#f6ecdf",
      tertiaryColor: cssVar("--color-figure-tint") || "#e7dfcd",
      fontFamily: "system-ui, sans-serif",
    },
  };
}

function loadMermaid() {
  if (!mermaidLoader) {
    mermaidLoader = new Promise((resolve, reject) => {
      const script = document.createElement("script");
      script.src = "./vendor/mermaid.min.js";
      const timer = setTimeout(
        () => reject(new Error("the bundled diagram engine did not load in time")),
        window.MERMAID_LOAD_TIMEOUT_MS ?? 8000,
      );
      script.onload = () => {
        clearTimeout(timer);
        const bundle = window.mermaid;
        const engine = bundle && (bundle.render ? bundle : bundle.default);
        if (!engine || !engine.render) {
          reject(new Error("the bundled diagram engine did not start"));
          return;
        }
        engine.initialize(mermaidConfig());
        resolve(engine);
      };
      script.onerror = () => {
        clearTimeout(timer);
        reject(new Error("the bundled diagram engine could not load"));
      };
      document.head.append(script);
    });
  }
  return mermaidLoader;
}

/* Mermaid answers a syntax error with a bomb drawing instead of a rejection,
 * so a "successful" render is checked for that drawing before it is trusted.
 * Returns the svg text, or null when the engine refused. */
function drawMermaid(engine, id, source) {
  return engine
    .render(id, source)
    .then(({ svg }) => {
      const parsed = new DOMParser().parseFromString(svg, "image/svg+xml");
      const root = parsed.documentElement;
      const ok =
        root &&
        root.nodeName.toLowerCase() === "svg" &&
        !parsed.getElementsByTagName("parsererror").length &&
        !parsed.querySelector(".error-text");
      return ok ? svg : null;
    })
    .catch(() => null);
}

/* Mermaid's flowchart parser rejects unquoted labels that contain the shape
 * brackets themselves — "(safety risk index)" inside "[...]" — and treats a
 * newline inside a label as the end of the statement. Models emit both
 * constantly, and quoting is what mermaid's own docs prescribe, so the
 * repair pass quotes such labels before a second render attempt. */
function repairMermaid(source) {
  const OPENERS = { "[": "]", "(": ")", "{": "}", "((": "))" };
  let out = "";
  let i = 0;
  const length = source.length;
  while (i < length) {
    const token = source.startsWith("((", i) ? "((" : source[i];
    const closer = OPENERS[token];
    if (!closer) {
      out += token;
      i += token.length;
      continue;
    }
    // A shape bracket only counts as a label opener when a node id sits
    // right before it (spaces are tolerated); anything else is syntax this
    // pass does not understand, and copying it unchanged is safest.
    let back = i - 1;
    while (back >= 0 && (source[back] === " " || source[back] === "\t")) back -= 1;
    if (back < 0 || !/[A-Za-z0-9_]/.test(source[back])) {
      out += token;
      i += token.length;
      continue;
    }
    const opener = token;
    let depth = 0;
    let j = i;
    let closed = false;
    while (j < length) {
      if (source[j] === '"') {
        // A quoted label is already safe; skip it whole so brackets inside
        // it cannot confuse the bracket counting.
        const end = source.indexOf('"', j + 1);
        j = end === -1 ? length : end + 1;
        continue;
      }
      if (source.startsWith(opener, j)) {
        depth += 1;
        j += opener.length;
        continue;
      }
      if (source.startsWith(closer, j)) {
        depth -= 1;
        j += closer.length;
        if (depth === 0) {
          closed = true;
          break;
        }
        continue;
      }
      j += 1;
    }
    const raw = closed ? source.slice(i + opener.length, j - closer.length) : source.slice(i + opener.length);
    const trimmed = raw.trim();
    const alreadyQuoted = trimmed.startsWith('"') && trimmed.endsWith('"') && trimmed.length >= 2;
    if (alreadyQuoted || !/[(){}\[\]"\n\r;]/.test(raw)) {
      out += token + raw + (closed ? closer : "");
    } else {
      // Newlines collapse to spaces (a quoted label may span lines), and an
      // inner double quote becomes mermaid's #quot; escape.
      const flat = raw.replace(/\s+/g, " ").trim();
      out += token + '"' + flat.replace(/"/g, "#quot;") + '"' + (closed ? closer : "");
    }
    i = closed ? j : length;
  }
  return out;
}

/* The mermaid source reads as a code block until the engine answers; if it
 * never can, the source stays and says why. A syntax error is given one
 * repaired second attempt — unquoted brackets in labels, labels spanning
 * lines — before the fallback settles in. */
function appendMermaid(container, source) {
  const host = document.createElement("div");
  host.className = "chat-mermaid";
  const pending = document.createElement("pre");
  pending.className = "chat-code";
  const codeEl = document.createElement("code");
  codeEl.textContent = source;
  pending.append(codeEl);
  host.append(pending);
  container.append(host);

  loadMermaid()
    .then((engine) => {
      // Re-read the palette per draw: the OS can switch themes while the
      // window is open, and a cached light palette draws light-on-light.
      engine.initialize(mermaidConfig());
      const id = `rag-mmd-${(mermaidSeq += 1)}`;
      return drawMermaid(engine, id, source).then((drawn) =>
        drawn ?? drawMermaid(engine, id, repairMermaid(source)),
      );
    })
    .then((svg) => {
      if (!svg) throw new Error("mermaid could not draw this source");
      const parsed = new DOMParser().parseFromString(svg, "image/svg+xml");
      const root = parsed.documentElement;
      if (
        root &&
        root.nodeName.toLowerCase() === "svg" &&
        !parsed.getElementsByTagName("parsererror").length
      ) {
        root.classList.add("chat-mermaid-svg");
        host.replaceChildren(document.adoptNode(root));
      } else {
        // securityLevel strict already sanitizes label markup.
        host.replaceChildren();
        host.innerHTML = svg;
      }
    })
    .catch(() => {
      const note = document.createElement("p");
      note.className = "diagram-fallback";
      note.textContent = "The diagram engine could not draw this; the mermaid source is shown instead.";
      host.replaceChildren(note, pending);
    });
}

async function locatePassage({ citation, id }) {
  // Citations carry the shape "file p.45" (or a bare filename, or a figure
  // caption marked "· figure"). Match on the file part, land on the page.
  const filename = citation.replace(/\s*p\.?\s*-?\d+.*$/i, "").replace(/\s*·\s*figure.*$/i, "").trim();
  if (!view.documents.some((doc) => doc.filename === filename)) {
    toast(`${filename} is not in the index anymore`, "error");
    return;
  }
  await selectDocument(filename);
  // The page lands as a metadata filter on the pane, not a text search: a
  // text filter on the first chunk of the page used to pin an unrelated
  // neighbour chunk whenever the page held more than one.
  const pageMatch = citation.match(/p\.?\s*(\d+)/i);
  if (pageMatch) {
    view.page = Number(pageMatch[1]);
    view.offset = 0;
    await loadChunks();
  }
  markCitedChunk(id);
  toast(`Showing ${filename} in the Knowledge window`);
}

/* The page filter alone can still show several chunks at once — definitions
 * pages do that — so the cited chunk is scrolled to and marked by id. */
function markCitedChunk(id) {
  if (!id) return;
  const card = [...nodes.chunkList.children].find((li) => li.dataset.chunkId === id);
  if (!card) return;
  card.classList.add("is-cited");
  card.scrollIntoView({ block: "center" });
}

function citationTargets(sources) {
  const map = new Map();
  (sources ?? []).forEach((source, index) => {
    const citation =
      source.page === -1 || source.page === null
        ? source.filename
        : `${source.filename} p.${source.page}`;
    const target = { citation, id: source.id };
    // The system prompt asks for passage numbers [1], [2], and sources is the
    // same retrieval list in the same order, so the index locates the passage.
    map.set(String(index + 1), target);
    map.set(citation, target);
    if (source.figure) map.set(`${citation} · figure`, target);
  });
  return map;
}

function chatTurn(role, content) {
  const message = document.createElement("div");
  message.className = `chat-msg chat-${role}`;
  const who = document.createElement("span");
  who.className = "chat-who";
  who.textContent = role === "user" ? "You" : "Assistant";
  message.append(who);
  if (role === "assistant") {
    const body = document.createElement("div");
    body.className = "chat-body";
    message.append(body);
    if (content.error) {
      const error = document.createElement("p");
      error.className = "chat-error";
      error.textContent = content.error;
      body.append(error);
    } else {
      renderMarkdown(body, content.answer ?? "", citationTargets(content.sources));
    }
  } else {
    const text = document.createElement("p");
    text.className = "chat-body";
    text.textContent = content;
    message.append(text);
  }
  return message;
}

let excludedDocs = new Set();
const chatHistory = [];
let chatBusy = false;
let currentSessionId = null;
let currentSessionTitle = "";
let sessionsCache = [];
let activeAssistant = null;

/* null means the whole index grounds the chat; a list means only the ticked
 * documents can, and an empty list means nothing grounds it on purpose. */
function groundingDocuments() {
  if (!excludedDocs.size) return null;
  return view.documents
    .filter((doc) => !excludedDocs.has(doc.filename))
    .map((doc) => doc.filename);
}

function renderChatLog() {
  nodes.chatLog.replaceChildren();
  if (!chatHistory.length) {
    nodes.chatLog.append(nodes.chatEmpty);
    nodes.chatEmpty.hidden = false;
    return;
  }
  for (const turn of chatHistory) nodes.chatLog.append(chatTurn(turn.role, turn.content));
}

/* The log follows a new answer only while the reader was already at the
 * bottom. Scrolling up mid-stream to re-read an earlier answer is a choice,
 * so the view stops following and a pill offers the way back. */
let stickToBottom = true;
const BOTTOM_SLOP = 80;

function atChatBottom() {
  return (
    nodes.chatLog.scrollHeight - nodes.chatLog.scrollTop - nodes.chatLog.clientHeight <
    BOTTOM_SLOP
  );
}

function updateJumpPill() {
  const atBottom = atChatBottom();
  nodes.chatJump.hidden = atBottom || !nodes.chatLog.children.length;
}

function scrollChatToBottom(force = false) {
  if (force || stickToBottom) {
    nodes.chatLog.scrollTop = nodes.chatLog.scrollHeight;
  }
  updateJumpPill();
}

/* The submit button doubles as Stop while an answer is in flight. The model
   call is one blocking request, so pressing Stop closes the bubble right
   away; the button stays a disabled Send until the worker settles. */
function setSendStop(stop) {
  nodes.chatSend.textContent = stop ? "Stop" : "Send";
  nodes.chatSend.classList.toggle("btn-stop", stop);
}

function paintSessionRows() {
  nodes.chatSessionList.replaceChildren();
  for (const session of sessionsCache) {
    const item = document.createElement("li");
    item.className = "session";
    if (session.id === currentSessionId) item.setAttribute("aria-current", "true");

    const open = document.createElement("button");
    open.type = "button";
    open.className = "session-open";
    open.addEventListener("click", () => loadSession(session.id));
    const title = document.createElement("span");
    title.className = "session-title";
    title.textContent = session.title;
    title.title = session.title;
    const turns = document.createElement("span");
    turns.className = "session-turns";
    turns.textContent = `${number(session.turns)} ${plural(session.turns, "turn")}`;
    open.append(title, turns);

    const tools = document.createElement("div");
    tools.className = "session-tools";
    const edit = iconButton(
      "edit",
      `Rename “${session.title}”`,
      () => openRenameDialog(session.id, session.title),
    );
    edit.classList.add("session-edit");
    const drop = iconButton(
      "remove",
      `Delete “${session.title}”`,
      () => removeSession(session.id),
      "danger",
    );
    drop.classList.add("session-remove");
    tools.append(edit, drop);

    item.append(open, tools);
    nodes.chatSessionList.append(item);
  }
  if (!sessionsCache.length) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = "No saved sessions. New sessions keep their citations on disk.";
    nodes.chatSessionList.append(empty);
  }
}

async function loadSessions() {
  try {
    sessionsCache = await invoke("list_chat_sessions");
    if (currentSessionId && !sessionsCache.some((session) => session.id === currentSessionId)) {
      currentSessionId = null;
    }
    if (!currentSessionId && sessionsCache.length) await loadSession(sessionsCache[0].id);
    if (!currentSessionId) currentSessionTitle = "";
    paintSessionRows();
  } catch (error) {
    toast(String(error), "error");
  }
}

async function loadSession(id) {
  try {
    const session = await invoke("load_chat_session", { id });
    currentSessionId = session.id;
    chatHistory.length = 0;
    let answered = 0;
    for (const turn of session.turns) {
      let content = turn.content;
      if (turn.role === "assistant") {
        // The backend stores answers as plain text with citations in a
        // parallel array; the transcript renderer wants { answer, sources }.
        if (typeof content === "string") {
          content = { answer: content, sources: session.sources?.[answered] ?? [] };
        }
        answered += 1;
      }
      chatHistory.push({ role: turn.role, content });
    }
    currentSessionTitle = session.title;
    paintSessionRows();
    renderChatLog();
    // Opening a chat is not following a stream; it lands at its latest answer.
    scrollChatToBottom(true);
  } catch (error) {
    toast(String(error), "error");
  }
}

async function newChat() {
  try {
    const session = await invoke("new_chat_session");
    currentSessionId = session.id;
    chatHistory.length = 0;
    currentSessionTitle = "New chat";
    if (!sessionsCache.some((entry) => entry.id === session.id)) {
      sessionsCache.unshift({ id: session.id, title: "New chat", turns: 0 });
    }
    paintSessionRows();
    renderChatLog();
    nodes.chatInput.focus();
  } catch (error) {
    toast(String(error), "error");
  }
}

/* One text-entry dialog serves chats and folders: the caller supplies the
 * prompt and what to do with the answer. */
let renameHandler = null;

function openTextDialog(prompt, value, handler) {
  nodes.chatRenameTitle.textContent = prompt;
  nodes.chatRenameInput.value = value;
  renameHandler = handler;
  openDialog(nodes.chatRenameDialog);
  nodes.chatRenameInput.focus();
  // The whole title starts selected: typing replaces it instead of splicing
  // into the middle of the old name.
  const input = nodes.chatRenameInput;
  if (typeof input.select === "function") {
    try {
      input.select();
    } catch (error) {
      try {
        input.setSelectionRange(0, input.value.length);
      } catch (ignored) {}
    }
  }
}

function openRenameDialog(id, title) {
  openTextDialog("Rename this chat", title, async (next) => {
    await invoke("rename_chat_session", { id, title: next });
    if (id === currentSessionId) currentSessionTitle = next;
    await loadSessions();
    toast("Chat renamed");
  });
}

function openFolderRename(dir, current) {
  const named = dir.startsWith(CUSTOM_GROUP_PREFIX);
  openTextDialog(named ? "Rename this group" : "Rename this folder", current, async (next) => {
    if (named) {
      customGroups.delete(current);
      customGroups.add(next);
      for (const [file, group] of docGroups) {
        if (group === current) docGroups.set(file, next);
      }
      if (nextAddGroup === current) nextAddGroup = next;
      saveCustomGroups();
      saveDocGroups();
      renderDocuments();
      toast("Group renamed");
    } else {
      folderLabels.set(dir, next);
      saveFolderLabels();
      renderDocuments();
      toast("Folder renamed");
    }
  });
}

function createGroup() {
  openTextDialog("New group", "", async (name) => {
    customGroups.add(name);
    saveCustomGroups();
    nextAddGroup = name;
    renderDocuments();
    toast(`New group “${name}” — your next Add documents fills it`);
  });
}

async function commitRename() {
  if (!renameHandler) return;
  const title = nodes.chatRenameInput.value.trim();
  if (!title) return;
  const handler = renameHandler;
  renameHandler = null;
  // Same order as the remove dialog: close first, then act, so a slow
  // backend never leaves a spent dialog sitting over the picker.
  closeDialog(nodes.chatRenameDialog);
  try {
    await handler(title);
  } catch (error) {
    toast(String(error), "error");
  }
}

async function removeSession(id) {
  try {
    await invoke("delete_chat_session", { id });
    if (id === currentSessionId) {
      currentSessionId = null;
      chatHistory.length = 0;
      renderChatLog();
    }
    await loadSessions();
    // The delete went to a trash folder, not the void: one click puts the
    // chat back, transcript and citations intact.
    toast("Chat deleted", "ok", {
      label: "Undo",
      onClick: async () => {
        try {
          await invoke("restore_chat_session", { id });
          await loadSessions();
          await loadSession(id);
          toast("Chat restored");
        } catch (error) {
          toast(String(error), "error");
        }
      },
    });
  } catch (error) {
    toast(String(error), "error");
  }
}

async function sendChat(event) {
  event?.preventDefault();
  const question = nodes.chatInput.value.trim();
  if (!question || chatBusy) return;

  // Chatting with no saved sessions starts one on the spot, so the exchange
  // lands on disk and the chat model names it from the topic.
  if (!currentSessionId) await newChat();

  chatBusy = true;
  setSendStop(true);
  nodes.chatInput.value = "";
  autoGrowChatInput();
  // Asking is a deliberate trip to the bottom, even if the reader had been
  // re-reading an earlier answer when they typed.
  stickToBottom = true;
  chatHistory.push({ role: "user", content: question });
  renderChatLog();
  scrollChatToBottom();
  nodes.chatLog.setAttribute("aria-busy", "true");
  const thinking = chatTurn("assistant", { answer: "Thinking…", sources: [] });
  thinking.classList.add("is-pending");
  nodes.chatLog.append(thinking);
  scrollChatToBottom();
  activeAssistant = thinking;

  try {
    await invoke("chat_completion", {
      sessionId: currentSessionId || null,
      question,
      // Only the thread as the user and assistant exchanged it; passages are
      // retrieved afresh for this question by the backend. An assistant turn
      // carries { answer, sources } in the app, but the backend takes a plain
      // string, so the citations stay behind when the thread goes out.
      history: chatHistory.length > 1
        ? chatHistory.slice(0, -1).map((turn) => ({
            role: turn.role,
            content:
              typeof turn.content === "string" ? turn.content : turn.content.answer,
          }))
        : null,
      documents: groundingDocuments(),
    });
  } catch (error) {
    finishAssistant({ error: String(error), sources: [] });
    toast(String(error), "error");
  } finally {
    chatBusy = false;
    setSendStop(false);
    nodes.chatSend.disabled = false;
    nodes.chatLog.setAttribute("aria-busy", "false");
    // The backend names a first-turn chat from the topic, so the picker's
    // titles and turn counts are only true after a reload of them.
    await loadSessions();
    scrollChatToBottom();
    nodes.chatInput.focus();
  }
}

function finishAssistant(content) {
  const message = activeAssistant;
  activeAssistant = null;
  if (!message) return;
  const body = message.querySelector(".chat-body");
  body.replaceChildren();
  if (content.stopped) {
    const note = document.createElement("p");
    note.className = "chat-stopped";
    note.textContent = "Stopped before the answer landed.";
    body.append(note);
  } else if (content.error) {
    const error = document.createElement("p");
    error.className = "chat-error";
    error.textContent = content.error;
    body.append(error);
  } else {
    renderMarkdown(body, content.answer ?? "", citationTargets(content.sources));
  }
  if (!content.error && !content.stopped) {
    chatHistory.push({
      role: "assistant",
      content: { answer: content.answer ?? "", sources: content.sources ?? [] },
    });
  }
  message.classList.remove("is-pending");
  scrollChatToBottom();
}

listen("chat-event", ({ payload }) => {
  if (payload.kind === "token") {
    // Raw model tokens are JSON fragments; the final answer arrives at "done".
    // Keep the pending indicator alive so the user sees the answer is coming.
    if (activeAssistant) {
      const body = activeAssistant.querySelector(".chat-body");
      if (body) body.textContent = "Thinking…";
    }
  } else if (payload.kind === "done") {
    finishAssistant(payload.value ?? { answer: "", sources: [] });
  } else if (payload.kind === "error") {
    finishAssistant({ error: payload.message, sources: [] });
    toast(payload.message, "error");
  } else if (payload.kind === "cancelled") {
    // Usual case the Stop press already closed the bubble; this covers a
    // cancel that raced ahead of the reply, which is dropped, not rendered.
    finishAssistant({ stopped: true });
  }
});


/* ---------- refresh loop ---------- */

async function refresh() {
  const page = await invoke("browse", { limit: 1 });
  view.documents = page.documents;
  view.status = page.status;
  renderStatus(page.status);
  renderDocuments();

  const known = page.documents.some((doc) => doc.filename === view.document);
  if (view.document && !known) {
    view.document = null;
    nodes.chunkList.replaceChildren();
    nodes.chunkCount.textContent = "";
    setHeadingFor(null);
  }
}

/* ---------- wiring ---------- */

/* The question box grows one line at a time with what you type, up to the
   stylesheet's ceiling where it scrolls; the resize handle is off. */
function autoGrowChatInput() {
  nodes.chatInput.style.height = "auto";
  nodes.chatInput.style.height = `${nodes.chatInput.scrollHeight}px`;
}

let filterTimer;
nodes.chunkFilter.addEventListener("input", () => {
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => {
    // Typing in the box is the user taking over the browse; the citation's
    // page lock gives way to the search.
    view.page = null;
    view.filter = nodes.chunkFilter.value.trim();
    view.offset = 0;
    loadChunks();
  }, 250);
});

nodes.loadMore.addEventListener("click", () => loadChunks({ append: true }));
nodes.knowledgeClose.addEventListener("click", () => closeDialog(nodes.knowledgeDialog));
/* The index note lives in a popover, not a permanent header line. */
function setStatsPopover(open) {
  nodes.statsPopover.hidden = !open;
  nodes.statsInfo.setAttribute("aria-expanded", String(open));
}

nodes.chatForm.addEventListener("submit", sendChat);
nodes.statsInfo.addEventListener("click", (event) => {
  // stopPropagation keeps the outside-click closer from eating this toggle.
  event.stopPropagation();
  setStatsPopover(nodes.statsPopover.hidden);
});
document.addEventListener("click", (event) => {
  if (!nodes.statsPopover.hidden && !nodes.statsPopover.contains(event.target)) {
    setStatsPopover(false);
  }
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !nodes.statsPopover.hidden) setStatsPopover(false);
});
nodes.chatSend.addEventListener("click", (event) => {
  if (!chatBusy) return; // idle, the button is a plain submit
  // Stop never doubles as a send of whatever is typed in the box.
  event.preventDefault();
  finishAssistant({ stopped: true });
  invoke("cancel_chat").catch((error) => toast(String(error), "error"));
});
nodes.chatInput.addEventListener("input", autoGrowChatInput);
/* The user scrolling is the signal: near the bottom means keep following,
 * anywhere above it means stop yanking the view around. */
nodes.chatLog.addEventListener("scroll", () => {
  stickToBottom = atChatBottom();
  updateJumpPill();
});
nodes.chatJump.addEventListener("click", () => {
  stickToBottom = true;
  scrollChatToBottom(true);
  nodes.chatInput.focus();
});
nodes.chatInput.addEventListener("keydown", (event) => {
  // Enter sends; Shift+Enter is a newline. An empty box must not hit the
  // backend, and Enter mid-composition (an IME choosing a candidate) belongs
  // to the input method, not to us.
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    sendChat(event);
  }
});
nodes.chatNew.addEventListener("click", newChat);
nodes.chatRenameOk.addEventListener("click", commitRename);
nodes.chatRenameCancel.addEventListener("click", () => closeDialog(nodes.chatRenameDialog));
nodes.sessionsToggle.addEventListener("click", () => {
  const collapsed = nodes.panes.classList.toggle("sessions-collapsed");
  nodes.sessionsToggle.setAttribute("aria-expanded", String(!collapsed));
  nodes.sessionsToggle.title = collapsed ? "Show the sessions panel" : "Hide the sessions panel";
});
nodes.addFiles.addEventListener("click", () => addDocuments("files"));
nodes.addFolder.addEventListener("click", () => addDocuments("folder"));
nodes.groupNew.addEventListener("click", createGroup);
nodes.confirmOk.addEventListener("click", confirmRemove);
nodes.confirmCancel.addEventListener("click", () => {
  pendingRemoval = null;
  closeConfirm();
});
nodes.confirm.addEventListener("cancel", () => {
  pendingRemoval = null;
});

nodes.captionPrompt.addEventListener("cancel", () => {
  pendingCaption = null;
  setAddEnabled(true);
});

const STAGE_LABELS = {
  preparing: "Loading the model",
  scanning: "Looking for figures",
  extracting: "Reading",
  captioning: "Captioning figures",
  embedding: "Embedding",
  saving: "Writing index",
};

/// Counting figures is the wait between picking documents and the question
/// about captioning, so it says which of its two passes it is in. Text
/// extraction reports the page count up front and then nothing until it ends,
/// which is stated rather than drawn as a count that never moves.
function progressLabel(payload) {
  const { stage, done, total, current, phase } = payload;
  if (stage === "scanning") {
    return phase === "text"
      ? `Reading the text of ${current} — ${number(total)} pages`
      : phase === "pages"
        ? `Looking for figures in ${current} — page ${number(done)} of ${number(total)}`
        : `Counting figures in ${number(done)} of ${number(total)} documents — ${current}`;
  }
  if (stage === "preparing") return `Loading the ${current}…`;
  if (stage === "embedding") return `Embedding ${number(done)} of ${number(total)} chunks`;
  if (stage === "extracting") {
    return `Reading ${number(done)} of ${number(total)} files — ${current}`;
  }
  if (stage === "captioning") {
    return `Captioning figure ${number(done)} of ${number(total)} — ${current}`;
  }
  return STAGE_LABELS[stage] ?? stage;
}

/// Only the passes that report a position get a meter.
function progressFraction(payload) {
  if (payload.stage === "saving") return 1;
  if (payload.stage === "preparing") return undefined;
  if (payload.stage === "scanning" && payload.phase !== "pages" && payload.phase !== "document") {
    return undefined;
  }
  return payload.total ? payload.done / payload.total : undefined;
}

listen("ingest-progress", ({ payload }) => {
  ingestActivity.show(progressLabel(payload), {
    fraction: progressFraction(payload),
    // Only the passes that run on their own count seconds; while a native
    // picker is open the user is the slow part, and a timer blames them.
    tick: payload.stage === "scanning" && payload.phase === "text",
  });

  // The command resolves before the last events land, so re-read the index once
  // progress goes quiet instead of trusting a mid-run count. Re-enabling here is
  // what unlocks the buttons if a straggler event arrives after hideActivity.
  setAddEnabled(false);
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(async () => {
    await refresh().catch((error) => toast(String(error), "error"));
    setAddEnabled(true);
  }, 700);
});

listen("app-ready", () => refresh());

nodes.visionOpen.addEventListener("click", openVision);
nodes.mcpOpen.addEventListener("click", openMcp);
nodes.mcpClose.addEventListener("click", () => closeDialog(nodes.mcpDialog));
nodes.mcpDone.addEventListener("click", () => closeDialog(nodes.mcpDialog));
nodes.mcpCopy.addEventListener("click", copyMcp);
nodes.visionClose.addEventListener("click", commitVision);
nodes.visionDone.addEventListener("click", commitVision);
nodes.visionDialog.addEventListener("cancel", () => {
  // Escape means "I did not mean that": put the stored values back.
  loadVision();
});
nodes.visionEnabled.addEventListener("change", saveVision);
nodes.visionUrl.addEventListener("change", () => {
  saveVision().then(refreshVisionModels);
});
nodes.visionModelSelect.addEventListener("change", saveVision);
nodes.visionKey.addEventListener("change", saveVision);
nodes.visionTest.addEventListener("click", testVision);
nodes.captionYes.addEventListener("click", () => commitCaption(true));
nodes.captionText.addEventListener("click", () => commitCaption(false));
nodes.captionCancel.addEventListener("click", closeCaptionPrompt);

ingestActivity.show("Reading the index…", { fraction: 0 });
refresh()
  .then(loadVision)
  .then(loadSessions)
  .then(() => {
    hideActivity();
    nodes.chatInput.focus();
  })
  .catch((error) => {
    hideActivity();
    renderStatus(null);
    renderDocuments();
    toast(String(error), "error");
  });
