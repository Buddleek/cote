// 状态栏：光标行列、统计、状态消息 + 编码/换行/语言快捷菜单。

import { useState } from "preact/hooks";
import type { MenuEntry } from "./menubar";
import type { Stats, TabMeta } from "./types";

function CellDropdown({
  label,
  entries,
}: {
  label: string;
  entries: MenuEntry[];
}) {
  const [open, setOpen] = useState(false);
  return (
    <button
      class="cell"
      onClick={() => setOpen(!open)}
      onMouseLeave={() => setOpen(false)}
    >
      {label}
      {open && (
        <div class="dropdown pop" onMouseEnter={() => setOpen(true)}>
          {entries.map((e, i) =>
            e.sep ? (
              <div class="sep" key={i} />
            ) : (
              <button
                key={i}
                class={`entry${e.checked ? " checked" : ""}`}
                onClick={() => {
                  setOpen(false);
                  e.action?.();
                }}
              >
                <span>{e.label}</span>
              </button>
            ),
          )}
        </div>
      )}
    </button>
  );
}

export function StatusBar({
  meta,
  stats,
  cursor,
  message,
  encodings,
  languages,
  onEncoding,
  onNewline,
  onLanguage,
}: {
  meta: TabMeta | null;
  stats: Stats | null;
  cursor: { line: number; col: number };
  message: string;
  encodings: string[];
  languages: { name: string; builtin: boolean }[];
  onEncoding(enc: string): void;
  onNewline(le: "LF" | "CRLF" | "CR"): void;
  onLanguage(name: string | null): void;
}) {
  if (!meta) return null;
  const encEntries: MenuEntry[] = encodings.map((e) => ({
    label: e,
    checked: meta.encoding === e,
    action: () => onEncoding(e),
  }));
  const nlEntries: MenuEntry[] = (["LF", "CRLF", "CR"] as const).map((le) => ({
    label: { LF: "LF", CRLF: "CRLF (Windows)", CR: "CR (经典 Mac)" }[le],
    checked: meta.newline === le,
    action: () => onNewline(le),
  }));
  const langEntries: MenuEntry[] = [
    { label: "自动检测", checked: meta.auto_language, action: () => onLanguage(null) },
    { sep: true },
    { groupLabel: "内置" },
    ...languages.filter((l) => l.builtin).map((l) => ({
      label: l.name,
      checked: !meta.auto_language && meta.language === l.name,
      action: () => onLanguage(l.name),
    })),
    ...(languages.some((l) => !l.builtin)
      ? ([
          { sep: true },
          { groupLabel: "自定义" },
          ...languages.filter((l) => !l.builtin).map((l) => ({
            label: l.name,
            checked: !meta.auto_language && meta.language === l.name,
            action: () => onLanguage(l.name),
          })),
        ] as MenuEntry[])
      : []),
  ];
  const langName = meta.auto_language ? meta.language ?? "纯文本" : meta.language ?? "纯文本";

  return (
    <div class="statusbar">
      <span class="cell">
        行 {cursor.line}, 列 {cursor.col}
      </span>
      {stats && (
        <span class="cell">
          {stats.lines} 行　{stats.chars} 字符　{stats.words} 词
        </span>
      )}
      <span class="cell status-msg">{message}</span>
      <CellDropdown label={`编码: ${meta.encoding} ▾`} entries={encEntries} />
      <CellDropdown
        label={
          meta.mixed_newline
            ? `⚠ 换行: 混合（按 ${meta.newline_label} 保存） ▾`
            : `换行: ${meta.newline_label} ▾`
        }
        entries={nlEntries}
      />
      <CellDropdown label={`语言: ${langName} ▾`} entries={langEntries} />
    </div>
  );
}
