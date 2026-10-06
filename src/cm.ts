// CodeMirror 6 编辑器装配：高亮装饰（后端 spans 注入）、按键、补全源。
//
// 数据流：本模块产生的视图变更（应用后端下发的 undo/替换等）带
// SyncAnnot 标记，updateListener 见到该标记不再回传后端——
// 避免编辑回路成环。

import { autocompletion, closeCompletion, completionStatus, startCompletion, type CompletionContext } from "@codemirror/autocomplete";
import { defaultKeymap } from "@codemirror/commands";
import {
  Annotation,
  StateEffect,
  StateField,
  type Extension,
  type Range,
} from "@codemirror/state";
import {
  Decoration,
  EditorView,
  keymap,
  lineNumbers,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { api } from "./backend";
import type { HighlightSpan } from "./types";

/** 后端下发文本变更时打上标记（undo/重载/整体替换等），不再回传。 */
export const SyncAnnot = Annotation.define<boolean>();

const setSpans = StateEffect.define<HighlightSpan[]>();
const setMatches = StateEffect.define<{ from: number; to: number; active: boolean }[]>();

/** 后端语法 spans → 装饰集（编辑时随 changes 映射，滞后于下一次下发是可接受的）。 */
const spansField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(set, tr) {
    set = set.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(setSpans)) {
        const marks: Range<Decoration>[] = [];
        // 防护与 egui 版 build_job_from_spans 一致：排序、丢弃越界/重叠段
        const spans = [...e.value].sort((a, b) => a.from - b.from || a.to - b.to);
        let pos = 0;
        for (const s of spans) {
          const from = Math.max(0, Math.min(s.from, tr.state.doc.length));
          const to = Math.max(0, Math.min(s.to, tr.state.doc.length));
          if (to <= from || from < pos) continue;
          marks.push(Decoration.mark({ class: `cm-t-${s.kind}` }).range(from, to));
          pos = to;
        }
        set = Decoration.set(marks, true);
      } else if (e.is(setMatches)) {
        const marks = e.value.map((m) =>
          m.active
            ? Decoration.mark({ class: "cm-match-active" }).range(m.from, m.to)
            : Decoration.mark({ class: "cm-match" }).range(m.from, m.to),
        );
        set = Decoration.set(marks, true);
      }
    }
    return set;
  },
});

const decorationsTheme = EditorView.decorations.from(spansField);

export interface EditorHooks {
  onDocChanged(update: ViewUpdate): void;
  onSelectionChanged(update: ViewUpdate): void;
  /** Tab 键插入制表符（补全激活时交还给补全）。 */
  onTab?(view: EditorView): boolean;
}

const WORD_RE = /^[\p{L}\p{N}_]+$/u;

export function createExtensions(tabId: number, hooks: EditorHooks): Extension[] {
  return [
    lineNumbers(),
    EditorView.lineWrapping,
    spansField,
    decorationsTheme,
    autocompletion({
      // 对照 egui 版：仅 Ctrl+Space 显式触发
      activateOnTyping: false,
      override: [async (ctx: CompletionContext) => {
        const word = ctx.matchBefore(/[\p{L}\p{N}_]+/u);
        if (!word || (word.from === word.to && !ctx.explicit)) return null;
        let items: string[];
        try {
          items = await api.completions(tabId, word.text);
        } catch {
          return null;
        }
        if (items.length === 0) return null;
        return {
          from: word.from,
          options: items.map((label) => ({ label, type: "text" })),
          validFor: WORD_RE,
        };
      }],
    }),
    keymap.of([
      {
        key: "Tab",
        run: (view) => {
          if (completionStatus(view.state) === "active") return false;
          if (hooks.onTab) return hooks.onTab(view);
          return false;
        },
      },
      { key: "Ctrl-Space", run: startCompletion },
      { key: "Escape", run: (view) => closeCompletion(view) },
    ]),
    keymap.of(defaultKeymap),
    EditorView.updateListener.of((update) => {
      const nonSyncDocChange = update.transactions.some(
        (tr) => tr.docChanged && tr.annotation(SyncAnnot) !== true,
      );
      if (nonSyncDocChange) {
        hooks.onDocChanged(update);
      }
      if (update.selectionSet || update.docChanged) {
        hooks.onSelectionChanged(update);
      }
    }),
    EditorView.contentAttributes.of({ spellcheck: "false" }),
  ];
}

export function dispatchSpans(view: EditorView, spans: HighlightSpan[]) {
  view.dispatch({ effects: setSpans.of(spans) });
}

export function dispatchMatches(
  view: EditorView,
  matches: { from: number; to: number; active: boolean }[],
) {
  view.dispatch({ effects: setMatches.of(matches) });
}

/** 用后端下发的新文本整体替换文档（undo/重载/变换/替换全部等）。
 *  cursor 显式给出目标光标（UTF-16 偏移，超界夹紧）：整体替换会把旧选区
 *  映射到新文末，不传则光标跳到结尾。 */
export function replaceWholeDoc(view: EditorView, text: string, cursor?: number, select?: [number, number]) {
  const cur = cursor !== undefined ? Math.max(0, Math.min(cursor, text.length)) : undefined;
  view.dispatch({
    changes: { from: 0, to: view.state.doc.length, insert: text },
    selection: select ? { anchor: select[0], head: select[1] } : cur !== undefined ? { anchor: cur } : undefined,
    annotations: SyncAnnot.of(true),
    scrollIntoView: true,
  });
}

/** Tab 键插入真实制表符（纯文本编辑器语义）。 */
export function insertTabChar(view: EditorView): boolean {
  view.dispatch(view.state.replaceSelection("\t"));
  return true;
}

export { setSpans, setMatches };
