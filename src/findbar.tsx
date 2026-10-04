// 查找/替换栏：Enter 下一处、Shift+Enter 上一处、全部替换（$1 捕获组）。
// 匹配高亮（装饰）由 App 通过 dispatchMatches 下发，这里只呈现计数。

import { useEffect, useRef } from "preact/hooks";
import type { SearchOpts } from "./types";

export function FindBar({
  query,
  replace,
  opts,
  count,
  current,
  error,
  onChange,
  onReplaceChange,
  onOpts,
  onNext,
  onPrev,
  onReplaceAll,
  onClose,
}: {
  query: string;
  replace: string;
  opts: SearchOpts;
  count: number;
  current: number;
  error: string | null;
  onChange(q: string): void;
  onReplaceChange(q: string): void;
  onOpts(o: Partial<SearchOpts>): void;
  onNext(): void;
  onPrev(): void;
  onReplaceAll(): void;
  onClose(): void;
}) {
  const queryRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    queryRef.current?.focus();
    queryRef.current?.select();
  }, []);

  return (
    <div class="findbar">
      <div class="row">
        <span>查找:</span>
        <input
          ref={queryRef}
          type="text"
          value={query}
          onInput={(e) => onChange((e.target as HTMLInputElement).value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              if (e.shiftKey) onPrev();
              else onNext();
            } else if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            }
          }}
          placeholder="支持 $1 捕获组（正则模式）"
        />
        <label class="opt">
          <input
            type="checkbox"
            checked={opts.case_sensitive}
            onChange={(e) => onOpts({ case_sensitive: (e.target as HTMLInputElement).checked })}
          />
          大小写
        </label>
        <label class="opt">
          <input
            type="checkbox"
            checked={opts.whole_word}
            onChange={(e) => onOpts({ whole_word: (e.target as HTMLInputElement).checked })}
          />
          整词
        </label>
        <label class="opt">
          <input
            type="checkbox"
            checked={opts.regex}
            onChange={(e) => onOpts({ regex: (e.target as HTMLInputElement).checked })}
          />
          正则
        </label>
        <span class="count">
          {error ? "正则错误" : count > 0 ? `${current + 1}/${count}` : count > 0 ? "" : query ? "无匹配" : ""}
        </span>
        <span class="spacer" />
        <button onClick={onNext} title="下一处 (Enter)">下一处</button>
        <button onClick={onPrev} title="上一处 (Shift+Enter)">上一处</button>
        <button onClick={onClose} title="关闭 (Esc)">关闭</button>
      </div>
      <div class="row">
        <span>替换为:</span>
        <input
          type="text"
          value={replace}
          onInput={(e) => onReplaceChange((e.target as HTMLInputElement).value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            }
          }}
        />
        <button onClick={onReplaceAll}>全部替换</button>
        <span class="spacer" />
        <span class="hint">Enter 下一处 · Shift+Enter 上一处</span>
      </div>
    </div>
  );
}
