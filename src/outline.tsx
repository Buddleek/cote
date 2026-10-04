// 大纲侧栏：语言规则提取的条目，点击跳转到行。

import type { OutlineEntry } from "./types";

export function OutlinePanel({
  items,
  onJump,
  onClose,
}: {
  items: OutlineEntry[];
  onJump(line: number): void;
  onClose(): void;
}) {
  return (
    <div class="outline">
      <div class="outline-head">
        <span>大纲（{items.length} 项）</span>
        <button onClick={onClose} title="关闭">×</button>
      </div>
      <div class="outline-list">
        {items.length === 0 ? (
          <div class="outline-empty">当前语言没有大纲规则或文档暂无条目。</div>
        ) : (
          items.map((it, i) => (
            <button
              key={i}
              class="outline-item"
              title={`${it.kind}: ${it.label}（第 ${it.line + 1} 行）`}
              onClick={() => onJump(it.line + 1)}
            >
              <span class="kind">{it.kind}</span>
              {it.label}
            </button>
          ))
        )}
      </div>
    </div>
  );
}
