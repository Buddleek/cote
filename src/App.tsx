// 应用编排：标签页 ↔ CodeMirror 视图 ↔ 后端命令。
//
// 结构约定：
// - 每个标签一个 EditorView（保留各自光标/滚动状态），仅激活标签挂载到 DOM；
// - 所有变更类后端调用经 backend.ts 的串行队列；
// - 后端下发的文本变更（撤销/整体替换/重载）打 SyncAnnot，不再回传。

import { startCompletion, completionStatus } from "@codemirror/autocomplete";
import { EditorState } from "@codemirror/state";
import { EditorView, type ViewUpdate } from "@codemirror/view";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { api } from "./backend";
import {
  createExtensions,
  dispatchMatches,
  dispatchSpans,
  insertTabChar,
  replaceWholeDoc,
  SyncAnnot,
  type EditorHooks,
} from "./cm";
import { Dialogs, type Modal } from "./dialogs";
import { FindBar } from "./findbar";
import { MenuBar, type MenuDef } from "./menubar";
import { OutlinePanel } from "./outline";
import { StatusBar } from "./statusbar";
import { TabBar } from "./tabbar";
import type {
  EditOp,
  LangInfo,
  OutlineEntry,
  SearchOpts,
  Stats,
  TabList,
  TabMeta,
} from "./types";

type Theme = "dark" | "light" | "system";

function useRefState<T>(initial: T) {
  const [value, setValue] = useState<T>(initial);
  const ref = useRef<T>(initial);
  const set = (v: T) => {
    ref.current = v;
    setValue(v);
  };
  return [ref, value, set] as const;
}

const darkMq = window.matchMedia("(prefers-color-scheme: dark)");

export function App() {
  // ---------- 状态 ----------
  const [ready, setReady] = useState(false);
  const [metas, setMetas] = useState<TabMeta[]>([]);
  const [activeId, setActiveId] = useState(0);
  const [stats, setStats] = useState<Stats | null>(null);
  const [cursor, setCursor] = useState({ line: 1, col: 1 });
  const [statusMsg, setStatusMsg] = useState("");
  const [encodings, setEncodings] = useState<string[]>([]);
  const [languages, setLanguages] = useState<LangInfo[]>([]);
  const [scripts, setScripts] = useState<{ name: string }[]>([]);
  const [scriptsDir, setScriptsDir] = useState("");
  const [outlineItems, setOutlineItems] = useState<OutlineEntry[]>([]);
  const [findCount, setFindCount] = useState(0);
  const [findCurrent, setFindCurrent] = useState(0);
  const [findError, setFindError] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState<number[]>([]);

  const [themeRef, theme, setThemeState] = useRefState<Theme>("system");
  const [outlineOpenRef, outlineOpen, setOutlineOpen] = useRefState(false);
  const [findOpenRef, findOpen, setFindOpen] = useRefState(false);
  const [findQueryRef, findQuery, setFindQueryS] = useRefState("");
  const [replaceQueryRef, replaceQuery, setReplaceQueryS] = useRefState("");
  const [findOptsRef, findOpts, setFindOptsS] = useRefState<SearchOpts>({
    case_sensitive: false,
    whole_word: false,
    regex: false,
  });
  const [modalRef, modal, setModalS] = useRefState<Modal>(null);

  // ---------- refs ----------
  const views = useRef(new Map<number, EditorView>());
  const hostRef = useRef<HTMLDivElement>(null);
  const activeIdRef = useRef(0);
  const metasRef = useRef<TabMeta[]>([]);
  const cursorLineRef = useRef(1);
  const pendingGoto = useRef<[number, number] | null>(null);
  const matchList = useRef<{ from: number; to: number }[]>([]);
  const findCurrentRef = useRef(0);
  const lastCloseId = useRef(0);
  const timers = useRef<Record<string, ReturnType<typeof setTimeout>>>({});

  // 动作表：全局事件监听器通过它调用最新闭包
  const actions = useRef<Record<string, (...args: never[]) => void>>({});

  const activeMeta = metas.find((m) => m.id === activeId) ?? null;

  // ---------- 基础工具 ----------

  function setStatus(msg: string) {
    setStatusMsg(msg);
  }

  function schedule(kind: string, ms: number, fn: () => void) {
    clearTimeout(timers.current[kind]);
    timers.current[kind] = setTimeout(fn, ms);
  }

  function mergeMeta(tabId: number, patch: Partial<TabMeta>) {
    metasRef.current = metasRef.current.map((m) => (m.id === tabId ? { ...m, ...patch } : m));
    setMetas(metasRef.current);
  }

  function applyList(list: TabList) {
    metasRef.current = list.tabs;
    setMetas(list.tabs);
    if (list.active_id !== activeIdRef.current) {
      activeIdRef.current = list.active_id;
      setActiveId(list.active_id);
    }
  }

  function computeCursor(view: EditorView): { line: number; col: number } {
    const pos = view.state.selection.main.head;
    const line = view.state.doc.lineAt(pos);
    const before = line.text.slice(0, pos - line.from);
    // CRLF 以 \n 分行，行尾 \r 是内容字符，不计入列
    const col = pos - line.from + 1 - (before.match(/\r/g)?.length ?? 0);
    return { line: line.number, col };
  }

  function activeView(): EditorView | null {
    return views.current.get(activeIdRef.current) ?? null;
  }

  // ---------- 编辑器视图 ----------

  function makeHooks(tabId: number): EditorHooks {
    return {
      onDocChanged(update: ViewUpdate) {
        const ops = collectOps(update);
        if (ops.length === 0) return;
        const sel = update.state.selection.main;
        mergeMeta(tabId, { dirty: true });
        api.applyEdit(tabId, ops, sel.anchor, sel.head).then(() => {
          afterTextChange(tabId);
        });
      },
      onSelectionChanged(update: ViewUpdate) {
        if (tabId !== activeIdRef.current) return;
        const c = computeCursor(update.view);
        cursorLineRef.current = c.line;
        setCursor(c);
        schedule("status", 300, () => reportStatus(tabId));
      },
      onTab: (view) => insertTabChar(view),
    };
  }

  function createView(tabId: number, text: string) {
    const hooks = makeHooks(tabId);
    const view = new EditorView({
      state: EditorState.create({ doc: text, extensions: createExtensions(tabId, hooks) }),
    });
    views.current.set(tabId, view);
    return view;
  }

  function destroyView(tabId: number) {
    const v = views.current.get(tabId);
    if (v) {
      v.destroy();
      views.current.delete(tabId);
    }
  }

  /** 挂载激活标签的视图（切换标签时换入换出 DOM）。 */
  useLayoutEffect(() => {
    if (!ready) return;
    const host = hostRef.current;
    const view = views.current.get(activeId);
    if (!host || !view) return;
    if (host.firstChild && host.firstChild !== view.dom) {
      host.removeChild(host.firstChild);
    }
    if (view.dom.parentElement !== host) host.appendChild(view.dom);
    view.requestMeasure();
    // 启动跳转（会话恢复 / CLI --line --column），只应用一次
    if (pendingGoto.current) {
      const [l, c] = pendingGoto.current;
      pendingGoto.current = null;
      const line = view.state.doc.line(Math.max(1, Math.min(l, view.state.doc.lines)));
      const pos = line.from + Math.max(0, Math.min(c - 1, line.length));
      view.dispatch({ selection: { anchor: pos }, scrollIntoView: true });
    }
    setTimeout(() => view.focus(), 0);
  }, [ready, activeId]);

  // ---------- 编辑同步 ----------

  function collectOps(update: ViewUpdate): EditOp[] {
    const ops: EditOp[] = [];
    for (const tr of update.transactions) {
      if (tr.annotation(SyncAnnot) === true || !tr.docChanged) continue;
      let shift = 0;
      tr.changes.iterChangedRanges((fromA, toA, fromB, toB) => {
        const inserted = tr.state.doc.sliceString(fromB, toB);
        ops.push({ from: fromA + shift, to: toA + shift, inserted });
        shift += inserted.length - (toA - fromA);
      });
    }
    return ops;
  }

  /** 后端已应用变更后的界面刷新（状态/高亮/大纲/查找计数，全部防抖）。 */
  function afterTextChange(tabId: number) {
    if (tabId !== activeIdRef.current) return;
    schedule("status", 150, () => reportStatus(tabId));
    schedule("hl", 200, () => refreshHighlight(tabId));
    if (outlineOpenRef.current) schedule("ol", 400, () => refreshOutline(tabId));
    if (findOpenRef.current && findQueryRef.current) schedule("find", 250, () => refreshFind());
  }

  function reportStatus(tabId: number) {
    const view = views.current.get(tabId);
    if (!view) return;
    const c = computeCursor(view);
    cursorLineRef.current = c.line;
    api
      .docStatus(tabId, c.line, c.col)
      .then((r) => {
        if (activeIdRef.current !== tabId) return;
        setStats(r.stats);
        mergeMeta(tabId, r.meta);
      })
      .catch(() => undefined);
  }

  function refreshHighlight(tabId: number) {
    api
      .highlight(tabId)
      .then((r) => {
        if (activeIdRef.current !== tabId) return;
        const view = views.current.get(tabId);
        if (view) dispatchSpans(view, r.spans);
      })
      .catch(() => undefined);
  }

  function refreshOutline(tabId: number) {
    api
      .outline(tabId)
      .then((items) => {
        if (activeIdRef.current === tabId) setOutlineItems(items);
      })
      .catch(() => undefined);
  }

  // ---------- 文件 ----------

  async function doNewTab() {
    const r = await api.newTab();
    createView(r.tab.meta.id, r.tab.text);
    applyList(r.list);
  }

  async function doOpenDialog() {
    const path = await open({ multiple: false, directory: false });
    if (typeof path !== "string") return;
    await doOpenPath(path);
  }

  async function doOpenPath(path: string) {
    const r = await api.openFile(path);
    if (r.tab) {
      // 复用空未命名标签时视图已存在 → 同步新文本
      const view = views.current.get(r.tab.meta.id);
      if (view) {
        if (view.state.doc.toString() !== r.tab.text) replaceWholeDoc(view, r.tab.text);
      } else {
        createView(r.tab.meta.id, r.tab.text);
      }
    }
    applyList(r.list);
    setStatus(r.message);
    schedule("hl", 50, () => refreshHighlight(activeIdRef.current));
    if (outlineOpenRef.current) schedule("ol", 50, () => refreshOutline(activeIdRef.current));
  }

  async function doSaveAs(tabId: number): Promise<boolean> {
    const meta = metasRef.current.find((m) => m.id === tabId);
    let defaultName = meta?.title ?? "untitled.txt";
    if (defaultName.startsWith("未命名")) defaultName = "untitled.txt";
    const path = await save({ defaultPath: defaultName });
    if (!path) return false;
    const r = await api.saveAs(tabId, path);
    applyList(r.list);
    setStatus(r.message);
    return !r.message.startsWith("保存失败");
  }

  async function doSave() {
    const r = await api.save(activeIdRef.current);
    if (r.message === "__need_path__") {
      await doSaveAs(activeIdRef.current);
      return;
    }
    applyList(r.list);
    setStatus(r.message);
  }

  async function doSaveAsActive() {
    await doSaveAs(activeIdRef.current);
  }

  async function doReload() {
    const tabId = activeIdRef.current;
    const r = await api.reload(tabId);
    applyList(r.list);
    setStatus(r.message);
    if (r.ok) {
      const view = views.current.get(tabId);
      if (view) replaceWholeDoc(view, r.tab.text);
      refreshHighlight(tabId);
      if (outlineOpenRef.current) refreshOutline(tabId);
    }
  }

  // ---------- 关闭 / 退出 ----------

  async function requestCloseTab(tabId: number) {
    const meta = metasRef.current.find((m) => m.id === tabId);
    if (meta?.dirty) {
      lastCloseId.current = tabId;
      setModalS({ kind: "close-tab", title: meta.title });
      return;
    }
    await closeTabRaw(tabId, false);
  }

  async function closeTabRaw(tabId: number, saveFirst: boolean) {
    const r = await api.closeTab(tabId, saveFirst);
    if (r.need_path) {
      // 无路径脏文档：先另存为（用户取消则放弃关闭）
      const ok = await doSaveAs(tabId);
      if (!ok) return;
      const r2 = await api.closeTab(tabId, true);
      finishClose(r2, tabId);
      return;
    }
    if (!r.closed && r.message) {
      setStatus(r.message);
      return;
    }
    finishClose(r, tabId);
  }

  function finishClose(r: { closed: boolean; list: TabList }, closedId: number) {
    if (!r.closed) return;
    destroyView(closedId);
    applyList(r.list);
    schedule("hl", 50, () => refreshHighlight(activeIdRef.current));
    if (outlineOpenRef.current) schedule("ol", 50, () => refreshOutline(activeIdRef.current));
  }

  async function requestExit() {
    const c = await api.exitCheck();
    if (c.dirty_with_path + c.dirty_untitled > 0) {
      setModalS({ kind: "exit", dirtyWithPath: c.dirty_with_path, dirtyUntitled: c.dirty_untitled });
    } else {
      exitNow();
    }
  }

  function exitNow() {
    getCurrentWindow().destroy();
  }

  // ---------- 撤销 / 重做 ----------

  async function doUndo() {
    const tabId = activeIdRef.current;
    const r = await api.undo(tabId);
    if (!r) {
      setStatus("没有可撤销的操作");
      return;
    }
    applyHistory(tabId, r.edits, r.cursor);
    applyList(r.list);
  }

  async function doRedo() {
    const tabId = activeIdRef.current;
    const r = await api.redo(tabId);
    if (!r) {
      setStatus("没有可重做的操作");
      return;
    }
    applyHistory(tabId, r.edits, r.cursor);
    applyList(r.list);
  }

  function applyHistory(
    tabId: number,
    edits: { from: number; to: number; inserted: string }[],
    cursor: number,
  ) {
    const view = views.current.get(tabId);
    if (!view) return;
    // 各条编辑坐标基于前一条应用后的文本 → 逐条 dispatch，光标随最后一条设置
    edits.forEach((e, i) => {
      const last = i === edits.length - 1;
      view.dispatch({
        changes: { from: e.from, to: e.to, insert: e.inserted },
        ...(last ? { selection: { anchor: cursor }, scrollIntoView: true } : {}),
        annotations: SyncAnnot.of(true),
      });
    });
    refreshHighlight(tabId);
    if (outlineOpenRef.current) refreshOutline(tabId);
    if (findOpenRef.current && findQueryRef.current) refreshFind();
  }

  // ---------- 查找 / 替换 ----------

  function openFind() {
    const view = activeView();
    let prefill = "";
    if (view) {
      const sel = view.state.selection.main;
      if (!sel.empty) {
        const t = view.state.doc.sliceString(sel.from, sel.to);
        if (!t.includes("\n")) prefill = t;
      }
    }
    setFindQueryS(prefill);
    setFindOpen(true);
  }

  function closeFind() {
    setFindOpen(false);
    matchList.current = [];
    const view = activeView();
    if (view) dispatchMatches(view, []);
  }

  function setFindOpts(patch: Partial<SearchOpts>) {
    setFindOptsS({ ...findOptsRef.current, ...patch });
  }

  async function refreshFind() {
    const tabId = activeIdRef.current;
    const view = activeView();
    if (!view) return;
    const q = findQueryRef.current;
    if (!q) {
      matchList.current = [];
      setFindCount(0);
      setFindError(null);
      dispatchMatches(view, []);
      return;
    }
    const r = await api.search(tabId, q, findOptsRef.current);
    if (activeIdRef.current !== tabId) return;
    if (r.error) {
      setFindError(r.error);
      matchList.current = [];
      setFindCount(0);
      dispatchMatches(view, []);
      return;
    }
    setFindError(null);
    matchList.current = r.matches;
    setFindCount(r.matches.length);
    if (findCurrentRef.current >= r.matches.length) {
      findCurrentRef.current = 0;
      setFindCurrent(0);
    }
    paintMatches(view);
  }

  function paintMatches(view: EditorView) {
    const cur = findCurrentRef.current;
    dispatchMatches(
      view,
      matchList.current.map((m, i) => ({ ...m, active: i === cur })),
    );
  }

  function findJump(dir: 1 | -1) {
    const view = activeView();
    if (!view || matchList.current.length === 0) return;
    const list = matchList.current;
    const pos = view.state.selection.main.head;
    let idx: number;
    if (dir === 1) {
      idx = list.findIndex((m) => m.from >= pos);
      if (idx < 0) idx = 0;
    } else {
      let lo = 0;
      while (lo < list.length && list[lo].from < pos) lo++;
      idx = lo === 0 ? list.length - 1 : lo - 1;
    }
    const m = list[idx];
    findCurrentRef.current = idx;
    setFindCurrent(idx);
    view.dispatch({ selection: { anchor: m.from, head: m.to }, scrollIntoView: true });
    paintMatches(view);
  }

  async function doReplaceAll() {
    const tabId = activeIdRef.current;
    if (!findQueryRef.current) {
      setStatus("查找内容为空");
      return;
    }
    const r = await api.replaceAll(tabId, findQueryRef.current, replaceQueryRef.current, findOptsRef.current);
    const view = views.current.get(tabId);
    if (view) replaceWholeDoc(view, r.text);
    applyList(r.list);
    setStatus(r.message);
    refreshHighlight(tabId);
    refreshFind();
  }

  // ---------- 跳转 / 大纲 ----------

  async function doGoto(line: number) {
    const tabId = activeIdRef.current;
    const r = await api.gotoLine(tabId, line);
    setModalS(null);
    if (!r) {
      setStatus("请输入有效行号");
      return;
    }
    const view = activeView();
    if (!view) return;
    view.dispatch({ selection: { anchor: r.from }, scrollIntoView: true });
    view.focus();
  }

  function toggleOutline() {
    const next = !outlineOpenRef.current;
    setOutlineOpen(next);
    if (next) refreshOutline(activeIdRef.current);
    else setOutlineItems([]);
  }

  // ---------- 变换 ----------

  async function doTransform(kind: string, line?: number) {
    const tabId = activeIdRef.current;
    const r = await api.transform(tabId, kind, line);
    if (r.message) {
      setStatus(r.message);
      return;
    }
    const view = views.current.get(tabId);
    if (view) replaceWholeDoc(view, r.text);
    applyList(r.list);
    refreshHighlight(tabId);
    if (outlineOpenRef.current) refreshOutline(tabId);
    if (findOpenRef.current && findQueryRef.current) refreshFind();
  }

  function transformLine(kind: string) {
    doTransform(kind, cursorLineRef.current - 1);
  }

  // ---------- 编码 / 换行 / 语言 ----------

  async function doSetEncoding(enc: string) {
    const tabId = activeIdRef.current;
    const r = await api.setEncoding(tabId, enc);
    const view = views.current.get(tabId);
    if (view && r.text !== view.state.doc.toString()) replaceWholeDoc(view, r.text);
    applyList(r.list);
    setStatus(r.message);
    refreshHighlight(tabId);
    if (outlineOpenRef.current) refreshOutline(tabId);
  }

  async function doSetNewline(le: string) {
    const tabId = activeIdRef.current;
    const r = await api.setNewline(tabId, le);
    const view = views.current.get(tabId);
    if (view && r.text !== view.state.doc.toString()) replaceWholeDoc(view, r.text);
    applyList(r.list);
    setStatus(r.message);
  }

  async function doSetLanguage(name: string | null) {
    const r = await api.setLanguage(activeIdRef.current, name);
    applyList(r);
    refreshHighlight(activeIdRef.current);
    if (outlineOpenRef.current) refreshOutline(activeIdRef.current);
  }

  // ---------- 脚本 ----------

  function refreshScripts() {
    api
      .listScripts()
      .then(setScripts)
      .catch(() => undefined);
  }

  async function doRunScript(name: string) {
    const tabId = activeIdRef.current;
    const r = await api.runScript(tabId, name);
    const view = views.current.get(tabId);
    if (view) {
      if (r.changed && r.text !== view.state.doc.toString()) replaceWholeDoc(view, r.text);
      if (r.sel) {
        view.dispatch({ selection: { anchor: r.sel[0], head: r.sel[1] }, scrollIntoView: true });
      }
    }
    applyList(r.list);
    setStatus(r.message);
    if (r.changed) {
      refreshHighlight(tabId);
      if (outlineOpenRef.current) refreshOutline(tabId);
    }
  }

  // ---------- 外观 ----------

  function applyThemeToDom(pref: Theme) {
    const effective = pref === "system" ? (darkMq.matches ? "dark" : "light") : pref;
    document.body.dataset.theme = effective;
  }

  function setThemePref(pref: Theme) {
    setThemeState(pref);
    applyThemeToDom(pref);
    api.setTheme(pref);
  }

  function toggleTheme() {
    const cur = themeRef.current === "system" ? (darkMq.matches ? "dark" : "light") : themeRef.current;
    setThemePref(cur === "dark" ? "light" : "dark");
    setStatus(`外观已切换为${cur === "dark" ? "浅色" : "深色"}`);
  }

  // ---------- 初始化 ----------

  useEffect(() => {
    let disposed = false;
    const unlisteners: (() => void)[] = [];

    (async () => {
      const init = await api.getInit();
      if (disposed) return;
      setEncodings(init.encodings);
      setLanguages(init.languages);
      setScripts(init.scripts);
      setScriptsDir(init.scripts_dir);
      for (const tp of init.tabs) createView(tp.meta.id, tp.text);
      metasRef.current = init.tabs.map((t) => t.meta);
      setMetas(metasRef.current);
      activeIdRef.current = init.active_id;
      setActiveId(init.active_id);
      setThemeState(init.theme as Theme);
      applyThemeToDom(init.theme as Theme);
      if (init.notices.length > 0) setStatus(init.notices.join("　"));
      pendingGoto.current = init.goto;
      setReady(true);

      unlisteners.push(
        await listen<{ tabId: number; kind: "modified" | "deleted" }>("file-changed", ({ payload }) => {
          mergeMeta(payload.tabId, { externally_changed: payload.kind });
          setDismissed((d) => d.filter((id) => id !== payload.tabId));
          if (payload.tabId === activeIdRef.current) {
            setStatus(payload.kind === "deleted" ? "⚠ 文件已被删除或移动" : "⚠ 文件已被外部程序修改");
          }
        }),
      );

      // 窗口关闭 → 退出确认流
      await getCurrentWindow().onCloseRequested((event) => {
        event.preventDefault();
        actions.current.requestExit?.([] as never);
      });

      // 系统主题变化（跟随系统时生效）
      const onSystemTheme = () => {
        if (themeRef.current === "system") applyThemeToDom("system");
      };
      darkMq.addEventListener("change", onSystemTheme);
      unlisteners.push(() => darkMq.removeEventListener("change", onSystemTheme));
    })().catch((e) => {
      setStatus(`初始化失败：${e}`);
    });

    return () => {
      disposed = true;
      unlisteners.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ---------- 全局快捷键 ----------

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing || e.defaultPrevented) return;
      const mod = e.ctrlKey || e.metaKey;
      const key = e.key.toLowerCase();
      const a = actions.current;

      // 模态优先：Esc 关闭（goto 输入框自行处理 Enter/Esc）
      if (modalRef.current) {
        if (key === "escape") setModalS(null);
        return;
      }
      // Esc：关闭查找栏（补全弹层激活时交给补全）
      if (key === "escape" && findOpenRef.current) {
        const view = activeView();
        if (!view || completionStatus(view.state) !== "active") {
          e.preventDefault();
          closeFind();
        }
        return;
      }
      if (!mod) return;

      if (!e.shiftKey && (key === "n" || key === "t")) {
        e.preventDefault();
        a.doNewTab?.();
      } else if (!e.shiftKey && key === "o") {
        e.preventDefault();
        a.doOpenDialog?.();
      } else if (!e.shiftKey && key === "s") {
        e.preventDefault();
        a.doSave?.();
      } else if (e.shiftKey && key === "s") {
        e.preventDefault();
        a.doSaveAsActive?.();
      } else if (!e.shiftKey && key === "w") {
        e.preventDefault();
        a.requestCloseTab?.(activeIdRef.current as never);
      } else if (!e.shiftKey && key === "f") {
        e.preventDefault();
        a.openFind?.();
      } else if (!e.shiftKey && key === "g") {
        e.preventDefault();
        setModalS({ kind: "goto" });
      } else if (!e.shiftKey && key === "z") {
        e.preventDefault();
        a.doUndo?.();
      } else if ((!e.shiftKey && key === "y") || (e.shiftKey && key === "z")) {
        e.preventDefault();
        a.doRedo?.();
      } else if (e.shiftKey && key === "l") {
        e.preventDefault();
        a.toggleTheme?.();
      } else if (key === "f5") {
        e.preventDefault();
        a.doReload?.();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 查找条件变化 → 重搜（防抖）
  useEffect(() => {
    if (!findOpen) return;
    schedule("find", 150, () => refreshFind());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [findOpen, findQuery, findOpts, activeId]);

  // 激活标签变化 → 刷新状态/高亮/大纲
  useEffect(() => {
    if (!ready) return;
    setStats(null);
    const tabId = activeIdRef.current;
    reportStatus(tabId);
    refreshHighlight(tabId);
    if (outlineOpenRef.current) refreshOutline(tabId);
    if (findOpenRef.current) refreshFind();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId, ready]);

  // ---------- 动作表 ----------

  actions.current = {
    doNewTab,
    doOpenDialog,
    doSave,
    doSaveAsActive,
    doReload,
    requestCloseTab,
    requestExit,
    doUndo,
    doRedo,
    openFind,
    toggleTheme,
  } as never;

  // ---------- 菜单 ----------

  const menus: MenuDef[] = [
    {
      title: "文件",
      entries: [
        { label: "新建标签页", accel: "Ctrl+N", action: () => void doNewTab() },
        { label: "打开…", accel: "Ctrl+O", action: () => void doOpenDialog() },
        { sep: true },
        { label: "保存", accel: "Ctrl+S", action: () => void doSave() },
        { label: "另存为…", accel: "Ctrl+Shift+S", action: () => void doSaveAsActive() },
        { label: "重新加载", accel: "F5", action: () => void doReload() },
        { sep: true },
        { label: "关闭标签页", accel: "Ctrl+W", action: () => void requestCloseTab(activeIdRef.current) },
        { label: "退出", action: () => void requestExit() },
      ],
    },
    {
      title: "编辑",
      entries: [
        { label: "撤销", accel: "Ctrl+Z", action: () => void doUndo() },
        { label: "重做", accel: "Ctrl+Y", action: () => void doRedo() },
        { sep: true },
        { label: "跳转到行…", accel: "Ctrl+G", action: () => setModalS({ kind: "goto" }) },
        {
          label: "单词补全",
          accel: "Ctrl+Space",
          action: () => {
            const view = activeView();
            if (view) startCompletion(view);
          },
        },
        { sep: true },
        { label: "删除当前行", action: () => void transformLine("delete_line") },
        { label: "复制当前行", action: () => void transformLine("duplicate_line") },
        { label: "当前行上移", action: () => void transformLine("move_up") },
        { label: "当前行下移", action: () => void transformLine("move_down") },
        { label: "切换行注释", action: () => void doTransform("toggle_comment") },
      ],
    },
    {
      title: "格式",
      entries: [
        { label: "转为大写", action: () => void doTransform("upper") },
        { label: "转为小写", action: () => void doTransform("lower") },
        { label: "词首大写", action: () => void doTransform("title") },
        { sep: true },
        { label: "全角 → 半角", action: () => void doTransform("full_to_half") },
        { label: "半角 → 全角", action: () => void doTransform("half_to_full") },
        {
          label: "Unicode 规范化",
          submenu: [
            { label: "NFC", action: () => void doTransform("nfc") },
            { label: "NFD", action: () => void doTransform("nfd") },
            { label: "NFKC", action: () => void doTransform("nfkc") },
            { label: "NFKD", action: () => void doTransform("nfkd") },
          ],
        },
        { sep: true },
        { label: "Tab → 空格 (4)", action: () => void doTransform("tabs_to_spaces") },
        { label: "行首空格 → Tab (4)", action: () => void doTransform("spaces_to_tabs") },
        { sep: true },
        { label: "行排序（升序）", action: () => void doTransform("sort_asc") },
        { label: "行排序（降序）", action: () => void doTransform("sort_desc") },
        { label: "行去重", action: () => void doTransform("unique_lines") },
        { label: "行反转", action: () => void doTransform("reverse_lines") },
        { label: "去行尾空白", action: () => void doTransform("trim_trailing") },
      ],
    },
    {
      title: "查找",
      entries: [
        { label: "查找 / 替换…", accel: "Ctrl+F", action: () => openFind() },
        { label: "下一处", accel: "Enter", action: () => findJump(1), disabled: findCount === 0 },
        { label: "上一处", accel: "Shift+Enter", action: () => findJump(-1), disabled: findCount === 0 },
        { label: "全部替换", action: () => void doReplaceAll(), disabled: findCount === 0 },
      ],
    },
    {
      title: "查看",
      entries: [
        { label: "大纲", checked: outlineOpen, action: () => toggleOutline() },
        { sep: true },
        {
          label: "外观",
          submenu: (["dark", "light", "system"] as const).map((p) => ({
            label: { dark: "深色", light: "浅色", system: "跟随系统" }[p],
            checked: theme === p,
            action: () => setThemePref(p),
          })),
        },
        { label: "切换深色 / 浅色", accel: "Ctrl+Shift+L", action: () => toggleTheme() },
      ],
    },
    {
      title: "编码",
      entries: () =>
        encodings.map((enc) => ({
          label: enc,
          checked: activeMeta?.encoding === enc,
          action: () => void doSetEncoding(enc),
        })),
    },
    {
      title: "换行",
      entries: (["LF", "CRLF", "CR"] as const).map((le) => ({
        label: { LF: "LF", CRLF: "CRLF (Windows)", CR: "CR (经典 Mac)" }[le],
        checked: activeMeta?.newline === le,
        action: () => void doSetNewline(le),
      })),
    },
    {
      title: "语言",
      entries: () => [
        { label: "自动检测", checked: activeMeta?.auto_language ?? true, action: () => void doSetLanguage(null) },
        { sep: true },
        { groupLabel: "内置" },
        ...languages
          .filter((l) => l.builtin)
          .map((l) => ({
            label: l.name,
            checked: !activeMeta?.auto_language && activeMeta?.language === l.name,
            action: () => void doSetLanguage(l.name),
          })),
        ...(languages.some((l) => !l.builtin)
          ? [
              { sep: true },
              { groupLabel: "自定义" },
              ...languages
                .filter((l) => !l.builtin)
                .map((l) => ({
                  label: l.name,
                  checked: !activeMeta?.auto_language && activeMeta?.language === l.name,
                  action: () => void doSetLanguage(l.name),
                })),
            ]
          : []),
      ],
    },
    {
      title: "脚本",
      onOpen: refreshScripts,
      entries: () =>
        scripts.length === 0
          ? [{ label: `暂无脚本。将 .js 文件放入：${scriptsDir}`, disabled: true }]
          : scripts.map((s) => ({ label: s.name, action: () => void doRunScript(s.name) })),
    },
  ];

  // ---------- 渲染 ----------

  if (!ready) {
    return <div style={{ padding: "20px", color: "var(--fg-dim)" }}>正在启动…</div>;
  }

  const showBanner = activeMeta?.externally_changed && !dismissed.includes(activeMeta.id);

  return (
    <>
      <MenuBar menus={menus} />
      <TabBar
        tabs={metas}
        activeId={activeId}
        onActivate={(id) => {
          if (id === activeIdRef.current) return;
          // 乐观切换，后端确认后校正
          activeIdRef.current = id;
          setActiveId(id);
          api.setActive(id).then(applyList).catch(() => undefined);
        }}
        onClose={(id) => void requestCloseTab(id)}
      />
      <div class="main-area">
        <div class="editor-column">
          {showBanner && (
            <div class="banner">
              <span class="warn">
                ⚠ {activeMeta!.externally_changed === "deleted" ? "文件已被删除或移动" : "文件已被外部程序修改"}
              </span>
              <span style={{ flex: 1 }} />
              <button
                onClick={() => setDismissed((d) => [...d, activeMeta!.id])}
              >
                忽略
              </button>
              <button onClick={() => void doReload()}>重新加载</button>
            </div>
          )}
          {findOpen && (
            <FindBar
              query={findQuery}
              replace={replaceQuery}
              opts={findOpts}
              count={findCount}
              current={findCurrent}
              error={findError}
              onChange={setFindQueryS}
              onReplaceChange={setReplaceQueryS}
              onOpts={setFindOpts}
              onNext={() => findJump(1)}
              onPrev={() => findJump(-1)}
              onReplaceAll={() => void doReplaceAll()}
              onClose={() => closeFind()}
            />
          )}
          <div class="editor-host">
            <div class="cm-holder" ref={hostRef} style="position:absolute;inset:0" />
          </div>
        </div>
        {outlineOpen && (
          <OutlinePanel
            items={outlineItems}
            onJump={(line) => void doGoto(line)}
            onClose={() => toggleOutline()}
          />
        )}
      </div>
      <StatusBar
        meta={activeMeta}
        stats={stats}
        cursor={cursor}
        message={statusMsg}
        encodings={encodings}
        languages={languages}
        onEncoding={(enc) => void doSetEncoding(enc)}
        onNewline={(le) => void doSetNewline(le)}
        onLanguage={(name) => void doSetLanguage(name)}
      />
      <Dialogs
        modal={modal}
        onConfirmCloseTab={() => {
          setModalS(null);
          void closeTabRaw(lastCloseId.current, true);
        }}
        onDiscardCloseTab={() => {
          setModalS(null);
          void closeTabRaw(lastCloseId.current, false);
        }}
        onExitSaveAll={() => {
          setModalS(null);
          api
            .saveAll()
            .then(() => exitNow())
            .catch(() => exitNow());
        }}
        onExitDiscard={() => {
          setModalS(null);
          exitNow();
        }}
        onGoto={(line) => void doGoto(line)}
        onCancel={() => setModalS(null)}
      />
    </>
  );
}
