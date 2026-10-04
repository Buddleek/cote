// 与 Rust 后端命令载荷一一对应的类型定义。

export interface TabMeta {
  id: number;
  title: string;
  dirty: boolean;
  path: string | null;
  encoding: string;
  newline: "LF" | "CRLF" | "CR";
  newline_label: string;
  mixed_newline: boolean;
  language: string | null;
  auto_language: boolean;
  externally_changed: "modified" | "deleted" | null;
}

export interface TabPayload {
  meta: TabMeta;
  text: string;
}

/** 标签列表快照：变更类命令随响应携带，前端整体替换本地列表。 */
export interface TabList {
  tabs: TabMeta[];
  active_id: number;
}

export interface NewTabResult {
  tab: TabPayload;
  list: TabList;
}

export interface Stats {
  lines: number;
  chars: number;
  words: number;
}

export interface LangInfo {
  name: string;
  builtin: boolean;
}

export interface ScriptEntry {
  name: string;
}

export interface InitPayload {
  tabs: TabPayload[];
  active_id: number;
  theme: "dark" | "light" | "system";
  encodings: string[];
  languages: LangInfo[];
  scripts: ScriptEntry[];
  scripts_dir: string;
  notices: string[];
  goto: [number, number] | null;
}

export interface OpenResult {
  /** 打开失败（文件不可读）时为 null */
  tab: TabPayload | null;
  list: TabList;
  message: string;
  switched: boolean;
}

export interface CloseResult {
  closed: boolean;
  need_path: boolean;
  list: TabList;
  message: string | null;
}

export interface SaveResult {
  meta: TabMeta;
  list: TabList;
  message: string;
}

export interface ReloadResult {
  tab: TabPayload;
  list: TabList;
  message: string;
  ok: boolean;
}

export interface ExitCheck {
  dirty_with_path: number;
  dirty_untitled: number;
}

export interface EditOp {
  from: number;
  to: number;
  inserted: string;
}

export interface HistoryEdit {
  from: number;
  to: number;
  inserted: string;
}

export interface HistoryResult {
  edits: HistoryEdit[];
  cursor: number;
  list: TabList;
}

export interface StatusResult {
  meta: TabMeta;
  stats: Stats;
}

export interface SearchOpts {
  case_sensitive: boolean;
  whole_word: boolean;
  regex: boolean;
}

export interface SearchMatch {
  from: number;
  to: number;
}

export interface SearchResult {
  matches: SearchMatch[];
  error: string | null;
}

export interface ReplaceAllResult {
  count: number;
  text: string;
  message: string;
  list: TabList;
}

export interface GotoResult {
  from: number;
  line: number;
}

export interface TransformResult {
  text: string;
  message: string | null;
  list: TabList;
}

export interface EncodingResult {
  text: string;
  message: string;
  list: TabList;
}

export interface NewlineResult {
  text: string;
  message: string;
  list: TabList;
}

export type SpanKindName =
  | "plain"
  | "keyword"
  | "str"
  | "comment"
  | "number"
  | "function"
  | "type"
  | "property"
  | "constant"
  | "operator"
  | "punct";

export interface HighlightSpan {
  from: number;
  to: number;
  kind: SpanKindName;
}

export interface HighlightResult {
  spans: HighlightSpan[];
  truncated: boolean;
}

export interface OutlineEntry {
  line: number;
  label: string;
  kind: string;
}

export interface ScriptResult {
  text: string;
  sel: [number, number] | null;
  message: string;
  changed: boolean;
  list: TabList;
}
