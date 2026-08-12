/*
 * THROWAWAY PROTOTYPE — 三个 UI variants，共享一个纯内存状态。
 * 计划：在独立页面通过 ?variant=A|B|C 比较“专注接力 / 命令中心 / 空间画布”。
 */

const VARIANTS = {
  A: "专注接力",
  B: "命令中心",
  C: "空间画布",
};

const now = new Date();
const todayAt = (hour) => {
  const value = new Date(now);
  value.setHours(hour, 0, 0, 0);
  return toLocalInputValue(value);
};

// 演示数据覆盖普通、带截止时间、带提醒和归档四类状态。
const seedNotes = [
  {
    id: "note-product",
    content:
      "QuickNote MVP\n\n首页先只保留仍在关注的便签。\n快捷键呼出后直接落在上次编辑位置。",
    archived: false,
    dueAt: todayAt(18),
    reminderAt: todayAt(18),
    updatedAt: Date.now() - 90_000,
  },
  {
    id: "note-weekend",
    content: "周末采购\n\n咖啡豆\n厨房纸\n给绿植换土",
    archived: false,
    dueAt: "",
    reminderAt: "",
    updatedAt: Date.now() - 3_600_000,
  },
  {
    id: "note-reading",
    content: "待读\n\nDesigning Interfaces 第三版\nWindows App SDK 通知文档",
    archived: false,
    dueAt: "",
    reminderAt: todayAt(20),
    updatedAt: Date.now() - 86_400_000,
  },
  {
    id: "note-trip",
    content: "杭州行程\n\n车票和酒店信息已经整理完毕。",
    archived: true,
    dueAt: "",
    reminderAt: "",
    updatedAt: Date.now() - 172_800_000,
  },
];

const state = {
  variant: getVariantFromUrl(),
  notes: structuredClone(seedNotes),
  currentId: "note-product",
  surface: "home",
  filter: "active",
  draft: null,
  saveStatus: "saved",
  recording: false,
  toast: null,
  metadataOpen: false,
};

let saveTimer = null;
let toastTimer = null;

// 页面始终从当前 URL 解析 variant，保证链接可分享且刷新稳定。
function getVariantFromUrl() {
  const requested = new URLSearchParams(window.location.search).get("variant");
  return VARIANTS[requested] ? requested : "A";
}

function toLocalInputValue(date) {
  const offset = date.getTimezoneOffset() * 60_000;
  return new Date(date.getTime() - offset).toISOString().slice(0, 16);
}

function escapeHtml(value = "") {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function noteTitle(note) {
  const firstLine = note?.content.trim().split("\n")[0];
  return firstLine || "空白便签";
}

function notePreview(note) {
  const lines = note?.content.trim().split("\n").filter(Boolean) ?? [];
  return lines.slice(1).join(" · ") || "还没有追加内容";
}

function currentNote() {
  return state.draft ?? state.notes.find((note) => note.id === state.currentId);
}

function visibleNotes() {
  const archived = state.filter === "archived";
  return state.notes
    .filter((note) => note.archived === archived)
    .sort((a, b) => b.updatedAt - a.updatedAt);
}

function formatDate(value) {
  if (!value) return "";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function announce(message) {
  document.querySelector("#live-region").textContent = message;
}

// A 的单栏断点与 CSS 保持一致；单栏没有常驻编辑区，改由浮窗承接编辑。
function usesCompactCapture() {
  return state.variant === "A" && window.matchMedia("(max-width: 900px)").matches;
}

// 浮窗打开时页面仍保留主页 editor，因此始终选取当前可见的那一个。
function activeEditor() {
  return state.surface === "capture"
    ? document.querySelector(".capture-window [data-editor]")
    : document.querySelector("[data-editor]");
}

// 保存状态只模拟用户可见反馈，不写磁盘或浏览器存储。
function queueAutosave() {
  window.clearTimeout(saveTimer);
  state.saveStatus = "saving";
  render({ preserveFocus: true });
  saveTimer = window.setTimeout(() => {
    state.saveStatus = "saved";
    render({ preserveFocus: true });
    announce("已自动保存");
  }, 650);
}

function showToast(message, action = null) {
  window.clearTimeout(toastTimer);
  state.toast = { message, action };
  render({ preserveFocus: true });
  toastTimer = window.setTimeout(() => {
    state.toast = null;
    render({ preserveFocus: true });
  }, 5000);
}

// 空白 draft 只有输入后才进入 notes，体现“无痕取消”。
function ensureDraftCommitted(content) {
  if (!state.draft || !content.trim()) return;
  state.draft.content = content;
  state.draft.updatedAt = Date.now();
  state.notes.unshift(state.draft);
  state.currentId = state.draft.id;
  state.draft = null;
  announce("新便签已创建并成为当前便签");
}

function startNewNote() {
  state.draft = {
    id: `note-${Date.now()}`,
    content: "",
    archived: false,
    dueAt: "",
    reminderAt: "",
    updatedAt: Date.now(),
  };
  state.surface = usesCompactCapture() || state.variant !== "A" ? "capture" : "home";
  state.recording = false;
  render({ focusEditor: true });
  announce(
    state.surface === "capture"
      ? "已打开空白新便签；不输入内容即可无痕取消"
      : "已在编辑区打开空白新便签；不输入内容并切换便签即可无痕取消",
  );
}

function openCapture() {
  state.surface = "capture";
  render({ focusEditor: true });
  announce(`已打开当前便签：${noteTitle(currentNote())}`);
}

function closeCapture() {
  if (state.draft && !state.draft.content.trim()) {
    state.draft = null;
    announce("空白新便签已无痕取消");
  }
  state.recording = false;
  state.surface = "home";
  render({ focusReturn: true });
}

// 原型用关闭 overlay + 显式焦点/反馈模拟“激活并显示 App 主窗口”。
function openMainWindow() {
  if (state.draft && !state.draft.content.trim()) {
    state.draft = null;
  }
  state.recording = false;
  state.surface = "home";
  render();
  showToast("QuickNote 主窗口已打开");

  const focusTarget = usesCompactCapture()
    ? document.querySelector(`[data-note-id="${state.currentId}"]`)
    : activeEditor();
  focusTarget?.focus();
  announce("QuickNote 主窗口已打开");
}

function openNote(noteId) {
  state.currentId = noteId;
  state.draft = null;
  state.recording = false;
  if (state.variant === "A" && !usesCompactCapture()) {
    state.surface = "home";
    render({ focusEditor: true });
    announce(`已切换当前便签：${noteTitle(currentNote())}`);
    return;
  }
  openCapture();
}

function updateContent(value, selectionStart = value.length) {
  const note = currentNote();
  if (!note) return;
  note.content = value;
  note.updatedAt = Date.now();
  ensureDraftCommitted(value);
  state.currentId = currentNote().id;
  queueAutosave();
  requestAnimationFrame(() => {
    const editor = activeEditor();
    if (editor) editor.setSelectionRange(selectionStart, selectionStart);
  });
}

function archiveCurrent() {
  const note = currentNote();
  if (!note || state.draft) {
    closeCapture();
    return;
  }
  const snapshot = { ...note };
  note.archived = true;
  note.reminderAt = "";
  note.updatedAt = Date.now();
  state.surface = "home";
  state.filter = "active";
  const next = visibleNotes()[0];
  if (next) state.currentId = next.id;
  showToast(`“${noteTitle(note)}”已归档，未来提醒已停止`, {
    label: "撤销",
    type: "undo-archive",
    payload: snapshot,
  });
}

function restoreNote(noteId) {
  const note = state.notes.find((item) => item.id === noteId);
  if (!note) return;
  note.archived = false;
  note.updatedAt = Date.now();
  state.currentId = note.id;
  state.filter = "active";
  showToast(`“${noteTitle(note)}”已恢复到主页`);
}

function undoArchive(snapshot) {
  const note = state.notes.find((item) => item.id === snapshot.id);
  if (!note) return;
  Object.assign(note, snapshot, { archived: false, updatedAt: Date.now() });
  state.currentId = note.id;
  state.toast = null;
  render();
  announce("已撤销归档");
}

function toggleRecording() {
  const note = currentNote();
  if (!note) return;
  if (!state.recording) {
    state.recording = true;
    render({ focusEditor: true });
    announce("正在录音；再次按下按钮结束并插入模拟转写");
    return;
  }

  state.recording = false;
  const editor = activeEditor();
  const caret = editor?.selectionStart ?? note.content.length;
  const transcript = "这是插入当前光标的模拟语音转写。";
  const separator = note.content && caret === note.content.length ? "\n" : "";
  const content = `${note.content.slice(0, caret)}${separator}${transcript}${note.content.slice(caret)}`;
  updateContent(content, caret + separator.length + transcript.length);
  announce("录音已结束，模拟转写已插入当前光标");
}

function changeVariant(direction) {
  const keys = Object.keys(VARIANTS);
  const currentIndex = keys.indexOf(state.variant);
  state.variant = keys[(currentIndex + direction + keys.length) % keys.length];
  const url = new URL(window.location.href);
  url.searchParams.set("variant", state.variant);
  window.history.replaceState({}, "", url);
  render();
  announce(`已切换到方案 ${state.variant}：${VARIANTS[state.variant]}`);
}

function switcherTemplate() {
  return `
    <nav class="prototype-switcher" aria-label="Prototype 方案切换">
      <button type="button" data-action="previous-variant" aria-label="上一个方案">←</button>
      <div class="prototype-label">${state.variant} — ${VARIANTS[state.variant]}</div>
      <button type="button" data-action="next-variant" aria-label="下一个方案">→</button>
    </nav>
  `;
}

function saveBadgeTemplate() {
  const saving = state.saveStatus === "saving";
  return `<span class="save-badge ${saving ? "saving" : ""}">${saving ? "正在保存…" : "✓ 已自动保存"}</span>`;
}

function metadataBadgesTemplate(note) {
  return `
    ${note.reminderAt ? `<span class="meta-badge">提醒 ${formatDate(note.reminderAt)}</span>` : ""}
    ${note.dueAt ? `<span class="meta-badge">截止 ${formatDate(note.dueAt)}</span>` : ""}
  `;
}

function noteListTemplate(kind) {
  const notes = visibleNotes();
  if (!notes.length) {
    return `<div class="empty-state">这里还没有${state.filter === "archived" ? "已归档" : "进行中"}的便签。</div>`;
  }

  if (kind === "a") {
    return notes
      .map(
        (note) => `
          <button class="a-note-card ${note.id === state.currentId ? "active" : ""}" type="button" data-note-id="${note.id}">
            ${note.id === state.currentId && !note.archived ? '<span class="current-badge"><span class="status-dot"></span>当前便签</span>' : ""}
            <h3 class="note-title">${escapeHtml(noteTitle(note))}</h3>
            <p class="note-preview">${escapeHtml(notePreview(note))}</p>
            <div class="editor-meta">${metadataBadgesTemplate(note)}</div>
          </button>
        `,
      )
      .join("");
  }

  if (kind === "b") {
    return notes
      .map(
        (note) => `
          <button class="b-table-row ${note.id === state.currentId ? "active" : ""}" type="button" data-note-id="${note.id}">
            <span>
              <h3>${note.id === state.currentId && !note.archived ? "● " : ""}${escapeHtml(noteTitle(note))}</h3>
              <p class="note-preview">${escapeHtml(notePreview(note))}</p>
            </span>
            <span class="muted">${note.archived ? "已归档" : note.dueAt ? formatDate(note.dueAt) : "—"}</span>
          </button>
        `,
      )
      .join("");
  }

  return notes
    .filter((note) => note.id !== state.currentId || note.archived)
    .map(
      (note) => `
        <button class="c-card" type="button" data-note-id="${note.id}">
          ${note.archived ? '<span class="meta-badge">已归档</span>' : ""}
          <h3>${escapeHtml(noteTitle(note))}</h3>
          <p class="note-preview">${escapeHtml(notePreview(note))}</p>
          <div class="editor-meta">${metadataBadgesTemplate(note)}</div>
          <span class="muted">${note.archived ? "打开以恢复" : "打开并设为当前 →"}</span>
        </button>
      `,
    )
    .join("");
}

function editorTemplate(context) {
  const note = currentNote();
  if (!note) return `<div class="empty-state">没有可编辑的便签。</div>`;
  const restore = note.archived;
  const editorClass = context === "capture" ? "capture-editor" : "main-editor";
  const lifecycleAction = restore
    ? '<button class="primary-button" type="button" data-action="restore-current">恢复到主页</button>'
    : state.draft
      ? ""
      : '<button class="danger-button" type="button" data-action="archive-current">归档</button>';
  const surfaceAction =
    context === "capture"
      ? '<button class="primary-button" type="button" data-action="open-main-window">打开主窗口</button>'
      : context === "b"
        ? '<button class="primary-button" type="button" data-action="open-capture">快速记录</button>'
        : "";
  return `
    <textarea
      class="${editorClass}"
      data-editor
      aria-label="便签内容"
      placeholder="直接输入；离开时会自动保存"
    >${escapeHtml(note.content)}</textarea>
    <div class="${context === "capture" ? "capture-footer" : context === "b" ? "b-action-rail" : "editor-footer"}">
      <div class="button-row">
        ${saveBadgeTemplate()}
        <button class="chip-button ${state.recording ? "recording" : ""}" type="button" data-action="toggle-recording">
          ${state.recording ? "■ 结束录音" : "● 语音"}
        </button>
        <button class="chip-button" type="button" data-action="open-metadata">◷ 时间</button>
      </div>
      <div class="button-row">
        ${lifecycleAction}
        ${surfaceAction}
      </div>
    </div>
  `;
}

function variantATemplate() {
  const note = currentNote();
  const archivedCount = state.notes.filter((item) => item.archived).length;
  const isArchiveView = state.filter === "archived";
  return `
    <main class="app-shell variant-a">
      <header class="a-header">
        <div class="brand-lockup">
          <span class="brand-name">QuickNote</span>
        </div>
        <div class="button-row">
          <button class="quiet-button" type="button" data-action="open-capture">唤起当前 <kbd>Ctrl ⇧ Space</kbd></button>
        </div>
      </header>
      <div class="a-layout">
        <section class="a-list-panel" aria-label="便签列表">
          <div class="panel-heading">
            <div>
              <h1>${isArchiveView ? "已归档" : "便签"}</h1>
              <p class="panel-summary">${visibleNotes().length} 张${isArchiveView ? "可恢复" : "正在关注"}</p>
            </div>
            ${
              isArchiveView
                ? '<button class="archive-link" type="button" data-filter="active">← 返回便签</button>'
                : `<button class="archive-link" type="button" data-filter="archived" aria-label="已归档${archivedCount ? `，${archivedCount} 张` : ""}">已归档${archivedCount ? `<span aria-hidden="true">${archivedCount}</span>` : ""}</button>`
            }
          </div>
          <div class="a-note-list">${noteListTemplate("a")}</div>
          ${
            isArchiveView
              ? ""
              : '<footer class="a-list-footer"><button class="primary-button" type="button" data-action="new-note">＋ 新建便签</button></footer>'
          }
        </section>
        <section class="a-editor-panel" aria-label="当前便签编辑器">
          <div class="editor-heading">
            <div>
              <p class="eyebrow">${note?.archived ? "已归档便签" : "当前便签"}</p>
              <h2>${escapeHtml(noteTitle(note))}</h2>
            </div>
            ${!note?.archived ? '<span class="current-badge"><span class="status-dot"></span>快捷键将打开这里</span>' : ""}
          </div>
          <div class="editor-meta">${metadataBadgesTemplate(note)}</div>
          ${editorTemplate("a")}
        </section>
      </div>
      ${sharedOverlaysTemplate()}
    </main>
  `;
}

function variantBTemplate() {
  const note = currentNote();
  return `
    <main class="app-shell variant-b">
      <div class="b-layout">
        <aside class="b-sidebar" aria-label="应用导航">
          <div class="b-logo">QuickNote</div>
          <nav class="b-nav">
            <button class="${state.filter === "active" ? "active" : ""}" type="button" data-filter="active">⌂　主页</button>
            <button class="${state.filter === "archived" ? "active" : ""}" type="button" data-filter="archived">↶　已归档</button>
            <button type="button" data-action="new-note">＋　新建</button>
          </nav>
          <div class="b-shortcut">随时追加到当前便签<br /><kbd>Ctrl + Shift + Space</kbd></div>
        </aside>
        <section class="b-list-panel" aria-label="便签列表">
          <header class="b-toolbar">
            <div><p class="eyebrow">${state.filter === "active" ? "ACTIVE" : "ARCHIVED"}</p><h1>${state.filter === "active" ? "主页" : "已归档"}</h1></div>
            <button class="primary-button" type="button" data-action="open-capture">快速记录</button>
          </header>
          <div>${noteListTemplate("b")}</div>
        </section>
        <section class="b-editor-panel" aria-label="当前便签编辑器">
          <header class="b-editor-head">
            <div>
              <p class="eyebrow">${note?.archived ? "ARCHIVED NOTE" : "CURRENT NOTE"}</p>
              <h2>${escapeHtml(noteTitle(note))}</h2>
            </div>
            <div class="button-row">${metadataBadgesTemplate(note)}</div>
          </header>
          ${editorTemplate("b")}
        </section>
      </div>
      ${sharedOverlaysTemplate()}
    </main>
  `;
}

function variantCTemplate() {
  const note = currentNote();
  return `
    <main class="app-shell variant-c">
      <header class="c-topbar">
        <div class="c-wordmark">QuickNote</div>
        <div class="button-row">
          <button class="quiet-button" type="button" data-filter="${state.filter === "active" ? "archived" : "active"}">${state.filter === "active" ? "查看归档" : "返回主页"}</button>
          <button class="primary-button" type="button" data-action="new-note">写一张新便签</button>
        </div>
      </header>
      <div class="c-main">
        <section class="c-hero" aria-label="当前便签">
          <div>
            <p class="eyebrow">${note?.archived ? "已归档" : "当前便签 · 快捷键直达"}</p>
            <h1>${escapeHtml(noteTitle(note))}</h1>
            <p>${escapeHtml(notePreview(note))}</p>
            <div class="editor-meta">${metadataBadgesTemplate(note)} ${saveBadgeTemplate()}</div>
          </div>
          <div class="c-hero-actions">
            ${note?.archived ? '<button class="primary-button" type="button" data-action="restore-current">恢复到主页</button>' : '<button class="primary-button" type="button" data-action="open-capture">继续记录</button>'}
            <button class="quiet-button" type="button" data-action="open-metadata">设置时间</button>
            ${!note?.archived ? '<button class="quiet-button" type="button" data-action="archive-current">归档</button>' : ""}
          </div>
        </section>
        <div class="c-section-head">
          <div><p class="eyebrow">${state.filter === "active" ? "仍在关注" : "可以恢复"}</p><h2>${state.filter === "active" ? "其他便签" : "归档画布"}</h2></div>
          <span class="muted">点开即${state.filter === "active" ? "设为当前" : "查看并恢复"}</span>
        </div>
        <section class="c-canvas" aria-label="便签画布">${noteListTemplate("c") || '<div class="empty-state">没有其他便签。</div>'}</section>
      </div>
      ${sharedOverlaysTemplate()}
    </main>
  `;
}

function captureTemplate() {
  if (state.surface !== "capture") return "";
  const note = currentNote();
  return `
    <section class="capture-backdrop" role="presentation" data-action="backdrop-close">
      <div class="capture-window" role="dialog" aria-modal="true" aria-labelledby="capture-title">
        <header class="capture-head">
          <div class="capture-title-wrap">
            <p class="eyebrow">${state.draft ? "新便签 · 空白可取消" : "快速记录 · 当前便签"}</p>
            <h2 id="capture-title">${escapeHtml(noteTitle(note))}</h2>
          </div>
          <button class="icon-button" type="button" data-action="close-capture" aria-label="关闭快速记录">×</button>
        </header>
        ${editorTemplate("capture")}
      </div>
    </section>
  `;
}

function metadataDialogTemplate() {
  const note = currentNote();
  const datetimeControl = ({ id, name, label, value }) => `
    <div class="datetime-control ${value ? "has-value" : ""}" data-datetime-control>
      <input id="${id}" name="${name}" type="datetime-local" value="${escapeHtml(value || "")}" />
      <span class="datetime-empty" aria-hidden="true">----/--/-- --:--</span>
      <button
        class="datetime-clear"
        type="button"
        data-clear-datetime="${name}"
        aria-label="清除${label}"
        ${value ? "" : "hidden"}
      >×</button>
    </div>
  `;
  return `
    <dialog class="meta-dialog" data-metadata-dialog aria-labelledby="metadata-title">
      <form method="dialog" data-metadata-form>
        <header class="dialog-head">
          <h2 id="metadata-title">提醒与截止时间</h2>
          <button class="icon-button" value="cancel" aria-label="关闭">×</button>
        </header>
        <div class="dialog-body">
          <div class="field">
            <label for="reminder-at">提醒</label>
            ${datetimeControl({ id: "reminder-at", name: "reminderAt", label: "提醒", value: note?.reminderAt })}
            <p class="field-hint">提醒可以独立修改或清除；它不改变截止时间。</p>
          </div>
          <div class="field">
            <label for="due-at">截止时间</label>
            ${datetimeControl({ id: "due-at", name: "dueAt", label: "截止时间", value: note?.dueAt })}
            <p class="field-hint">提醒为空时，设置截止时间会同时添加同一时刻的提醒。</p>
          </div>
        </div>
        <footer class="dialog-footer">
          <button class="quiet-button" value="cancel">取消</button>
          <button class="primary-button" value="save" data-action="save-metadata">保存</button>
        </footer>
      </form>
    </dialog>
  `;
}

function stateInspectorTemplate() {
  const note = currentNote();
  const exposed = {
    variant: state.variant,
    surface: state.surface,
    filter: state.filter,
    currentId: state.currentId,
    draftUncommitted: Boolean(state.draft),
    saveStatus: state.saveStatus,
    recording: state.recording,
    currentNote: note
      ? {
          title: noteTitle(note),
          archived: note.archived,
          dueAt: note.dueAt || null,
          reminderAt: note.reminderAt || null,
        }
      : null,
    noteCounts: {
      active: state.notes.filter((item) => !item.archived).length,
      archived: state.notes.filter((item) => item.archived).length,
    },
  };
  return `
    <details class="state-inspector">
      <summary>Prototype state</summary>
      <pre>${escapeHtml(JSON.stringify(exposed, null, 2))}</pre>
    </details>
  `;
}

function toastTemplate() {
  if (!state.toast) return "";
  return `
    <div class="toast" role="status">
      <span>${escapeHtml(state.toast.message)}</span>
      ${state.toast.action ? `<button type="button" data-action="toast-action">${escapeHtml(state.toast.action.label)}</button>` : ""}
    </div>
  `;
}

function sharedOverlaysTemplate() {
  return `
    ${captureTemplate()}
    ${metadataDialogTemplate()}
    ${switcherTemplate()}
    ${stateInspectorTemplate()}
    ${toastTemplate()}
  `;
}

// 重渲染前记录焦点和 selection，让 autosave 反馈不打断输入。
function render(options = {}) {
  const previousEditor = document.querySelector("[data-editor]:focus");
  const selection = previousEditor
    ? { start: previousEditor.selectionStart, end: previousEditor.selectionEnd }
    : null;
  const app = document.querySelector("#app");
  const templates = { A: variantATemplate, B: variantBTemplate, C: variantCTemplate };
  app.innerHTML = templates[state.variant]();
  bindEvents();

  if (state.metadataOpen) {
    const dialog = document.querySelector("[data-metadata-dialog]");
    dialog?.showModal();
  }

  if (options.focusEditor || (options.preserveFocus && selection)) {
    const editor = activeEditor();
    editor?.focus();
    if (editor && selection) {
      editor.setSelectionRange(selection.start, selection.end);
    } else if (editor && options.focusEditor) {
      // 首次呼出默认在末尾继续追加；重渲染则保留用户当前 caret。
      editor.setSelectionRange(editor.value.length, editor.value.length);
    }
  }

  if (options.focusReturn) {
    document.querySelector('[data-action="open-capture"]')?.focus();
  }
}

function saveMetadata(form) {
  const note = currentNote();
  if (!note) return;
  const formData = new FormData(form);
  const dueAt = String(formData.get("dueAt") || "");
  note.dueAt = dueAt;
  // 默认联动只发生在编辑阶段；保存时两个字段始终按当前值独立写入。
  note.reminderAt = String(formData.get("reminderAt") || "");
  note.updatedAt = Date.now();
  state.metadataOpen = false;
  queueAutosave();
  announce("截止时间与提醒已更新");
}

function bindEvents() {
  document.querySelectorAll("[data-note-id]").forEach((button) => {
    button.addEventListener("click", () => {
      const note = state.notes.find((item) => item.id === button.dataset.noteId);
      if (state.variant === "A") {
        openNote(button.dataset.noteId);
      } else if (note?.archived) {
        state.currentId = note.id;
        render();
      } else {
        openNote(button.dataset.noteId);
      }
    });
  });

  document.querySelectorAll("[data-filter]").forEach((button) => {
    button.addEventListener("click", () => {
      state.filter = button.dataset.filter;
      const next = visibleNotes()[0];
      if (next) state.currentId = next.id;
      state.draft = null;
      render();
    });
  });

  document.querySelectorAll("[data-editor]").forEach((editor) => {
    editor.addEventListener("input", (event) => {
      // 浮窗存在时忽略被遮挡的主页 editor。
      if (event.target !== activeEditor()) return;
      updateContent(event.target.value, event.target.selectionStart);
    });
  });

  document.querySelectorAll("[data-action]").forEach((element) => {
    element.addEventListener("click", (event) => handleAction(event, element.dataset.action));
  });

  const dialog = document.querySelector("[data-metadata-dialog]");
  dialog?.addEventListener("close", () => {
    state.metadataOpen = false;
  });
  dialog?.addEventListener("cancel", () => {
    state.metadataOpen = false;
  });

  // 提醒为空时，修改截止时间会同步提醒；已有提醒保持独立。
  const dueInput = document.querySelector('[name="dueAt"]');
  const reminderInput = document.querySelector('[name="reminderAt"]');

  // 空值使用明确的横线占位；有值时显示独立清除键，不依赖浏览器格式提示。
  const syncDatetimeControl = (input) => {
    const control = input?.closest("[data-datetime-control]");
    if (!control) return;
    const hasValue = Boolean(input.value);
    control.classList.toggle("has-value", hasValue);
    control.querySelector("[data-clear-datetime]").hidden = !hasValue;
  };

  [reminderInput, dueInput].forEach((input) => {
    input?.addEventListener("input", () => syncDatetimeControl(input));
    input?.addEventListener("change", () => syncDatetimeControl(input));
  });

  document.querySelectorAll("[data-clear-datetime]").forEach((button) => {
    button.addEventListener("click", () => {
      const input = dialog.querySelector(`[name="${button.dataset.clearDatetime}"]`);
      input.value = "";
      syncDatetimeControl(input);
      input.focus();
      announce(`${button.getAttribute("aria-label")}成功`);
    });
  });

  dueInput?.addEventListener("change", () => {
    if (dueInput.value && !reminderInput.value) {
      reminderInput.value = dueInput.value;
      syncDatetimeControl(reminderInput);
    }
  });
}

function handleAction(event, action) {
  if (action === "backdrop-close" && event.target !== event.currentTarget) return;
  const actions = {
    "open-capture": openCapture,
    "close-capture": closeCapture,
    "backdrop-close": closeCapture,
    "open-main-window": openMainWindow,
    "new-note": startNewNote,
    "archive-current": archiveCurrent,
    "toggle-recording": toggleRecording,
    "previous-variant": () => changeVariant(-1),
    "next-variant": () => changeVariant(1),
    "open-metadata": () => {
      state.metadataOpen = true;
      render();
    },
    "restore-current": () => restoreNote(currentNote()?.id),
    "save-metadata": () => {
      event.preventDefault();
      saveMetadata(event.target.closest("form"));
    },
    "toast-action": () => {
      if (state.toast?.action?.type === "undo-archive") {
        undoArchive(state.toast.action.payload);
      }
    },
  };
  actions[action]?.();
}

// 全局快捷键仅在浏览器内模拟；真实 RegisterHotKey 明确不属于本原型。
window.addEventListener("keydown", (event) => {
  const target = event.target;
  const isEditing =
    target.matches?.("input, textarea, [contenteditable='true']") ?? false;

  if (event.ctrlKey && event.shiftKey && event.code === "Space") {
    event.preventDefault();
    openCapture();
    return;
  }

  if (event.key === "Escape" && state.surface === "capture") {
    event.preventDefault();
    closeCapture();
    return;
  }

  if (event.key === "Escape" && state.draft && state.surface === "home") {
    event.preventDefault();
    state.draft = null;
    render();
    document.querySelector('[data-action="new-note"]')?.focus();
    announce("空白新便签已无痕取消");
    return;
  }

  if (!isEditing && event.key === "ArrowLeft") {
    event.preventDefault();
    changeVariant(-1);
  }

  if (!isEditing && event.key === "ArrowRight") {
    event.preventDefault();
    changeVariant(1);
  }
});

window.addEventListener("popstate", () => {
  state.variant = getVariantFromUrl();
  render();
});

render();
