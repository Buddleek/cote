// 标签栏：标题 + 脏标记圆点 + 关闭按钮；中键关闭。

import type { TabMeta } from "./types";

export function TabBar({
  tabs,
  activeId,
  onActivate,
  onClose,
}: {
  tabs: TabMeta[];
  activeId: number;
  onActivate(id: number): void;
  onClose(id: number): void;
}) {
  return (
    <div class="tabbar">
      {tabs.map((t) => (
        <div
          key={t.id}
          class={`tab${t.id === activeId ? " active" : ""}`}
          onMouseDown={(e) => {
            if (e.button === 1) {
              e.preventDefault();
              onClose(t.id);
            } else if (e.button === 0) {
              onActivate(t.id);
            }
          }}
          title={t.path ?? t.title}
        >
          {t.dirty && <span class="dirty-dot">●</span>}
          <span class="tab-title">{t.title}</span>
          <button
            class="close-btn"
            title="关闭标签页 (Ctrl+W)"
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => onClose(t.id)}
          >
            ×
          </button>
        </div>
      ))}
    </div>
  );
}
