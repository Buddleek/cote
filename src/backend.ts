// 后端调用桥：所有改变后端状态的 invoke 都经 promise 链串行化，
// 保证编辑序列（apply_edit / undo / replace_all …）不乱序——
// 后端 Document 的撤销分组依赖调用的先后顺序。

import { invoke } from "@tauri-apps/api/core";
import type {
  CloseResult,
  EditOp,
  EncodingResult,
  ExitCheck,
  GotoResult,
  HighlightResult,
  HistoryResult,
  InitPayload,
  LangInfo,
  NewTabResult,
  NewlineResult,
  OpenResult,
  OutlineEntry,
  ReloadResult,
  ReplaceAllResult,
  SaveResult,
  ScriptResult,
  SearchOpts,
  SearchResult,
  StatusResult,
  TabList,
  TransformResult,
} from "./types";

let chain: Promise<unknown> = Promise.resolve();

/** 串行化执行（FIFO）。单个失败不中断队列。 */
export function enqueue<T>(fn: () => Promise<T>): Promise<T> {
  const run = chain.then(fn, fn);
  chain = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(cmd, args) as Promise<T>;
}

/** 需要与编辑序列保序的调用。 */
export function ordered<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return enqueue(() => call<T>(cmd, args));
}

// 只读查询也走队列：确保读到的是已应用全部编辑后的文本。
export const api = {
  getInit: () => call<InitPayload>("get_init"),

  newTab: () => ordered<NewTabResult>("new_tab"),
  openFile: (path: string) => ordered<OpenResult>("open_file", { path }),
  closeTab: (tabId: number, save: boolean) =>
    ordered<CloseResult>("close_tab", { tabId, save }),
  setActive: (tabId: number) => ordered<TabList>("set_active", { tabId }),
  save: (tabId: number) => ordered<SaveResult>("save", { tabId }),
  saveAs: (tabId: number, path: string) => ordered<SaveResult>("save_as", { tabId, path }),
  saveAll: () => ordered<TabList>("save_all"),
  reload: (tabId: number) => ordered<ReloadResult>("reload_tab", { tabId }),
  exitCheck: () => call<ExitCheck>("exit_check"),

  applyEdit: (tabId: number, ops: EditOp[], anchor: number, head: number) =>
    ordered<void>("apply_edit", { tabId, ops, anchor, head }),
  docStatus: (tabId: number, line: number, col: number) =>
    ordered<StatusResult>("doc_status", { tabId, line, col }),
  undo: (tabId: number) => ordered<HistoryResult | null>("undo", { tabId }),
  redo: (tabId: number) => ordered<HistoryResult | null>("redo", { tabId }),

  search: (tabId: number, query: string, opts: SearchOpts) =>
    ordered<SearchResult>("search", { tabId, query, opts }),
  replaceAll: (tabId: number, query: string, replacement: string, opts: SearchOpts) =>
    ordered<ReplaceAllResult>("replace_all", { tabId, query, replacement, opts }),
  gotoLine: (tabId: number, line: number) =>
    ordered<GotoResult | null>("goto_line", { tabId, line }),

  transform: (tabId: number, kind: string, line?: number) =>
    ordered<TransformResult>("transform", { tabId, kind, line: line ?? null }),

  setEncoding: (tabId: number, enc: string) =>
    ordered<EncodingResult>("set_encoding", { tabId, enc }),
  setNewline: (tabId: number, le: string) =>
    ordered<NewlineResult>("set_newline", { tabId, le }),
  listLanguages: () => call<LangInfo[]>("list_languages"),
  setLanguage: (tabId: number, name: string | null) =>
    ordered<TabList>("set_language", { tabId, name }),

  highlight: (tabId: number) => ordered<HighlightResult>("highlight", { tabId }),
  outline: (tabId: number) => ordered<OutlineEntry[]>("outline", { tabId }),
  completions: (tabId: number, prefix: string) =>
    ordered<string[]>("completions", { tabId, prefix }),

  listScripts: () => call<{ name: string }[]>("list_scripts"),
  runScript: (tabId: number, name: string) => ordered<ScriptResult>("run_script", { tabId, name }),

  setTheme: (theme: string) => call<void>("set_theme", { theme }),
};
