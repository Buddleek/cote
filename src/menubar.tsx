// 数据驱动的菜单栏：点击展开、悬停切换、点击外部/Esc 关闭。
// 子菜单（Unicode 规范化 / 外观）支持一层嵌套。

import { useLayoutEffect, useRef, useState } from "preact/hooks";
export interface MenuEntry {
  label?: string;
  accel?: string;
  action?: () => void;
  checked?: boolean;
  disabled?: boolean;
  /** 分隔线 */
  sep?: boolean;
  /** 非点击分组标题 */
  groupLabel?: string;
  submenu?: MenuEntry[];
}

export interface MenuDef {
  title: string;
  /** 每次展开时求值（脚本/语言等动态项） */
  entries: MenuEntry[] | (() => MenuEntry[]);
  onOpen?: () => void;
}

function EntryList({ entries, onPick }: { entries: MenuEntry[]; onPick: () => void }) {
  return (
    <>
      {entries.map((e, i) => {
        if (e.sep) return <div class="sep" key={i} />;
        if (e.groupLabel)
          return (
            <div class="group-label" key={i}>
              {e.groupLabel}
            </div>
          );
        if (e.submenu) {
          return <SubmenuEntry key={i} entry={e} onPick={onPick} />;
        }
        return (
          <button
            key={i}
            class={`entry${e.checked ? " checked" : ""}${e.disabled ? " disabled" : ""}`}
            onClick={() => {
              if (e.disabled) return;
              e.action?.();
              onPick();
            }}
          >
            <span>{e.label}</span>
            {e.accel && <span class="accel">{e.accel}</span>}
          </button>
        );
      })}
    </>
  );
}

function SubmenuEntry({ entry, onPick }: { entry: MenuEntry; onPick: () => void }) {
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  return (
    <div
      style="position: relative"
      onMouseEnter={() => {
        // fixed 定位跳出父 dropdown 的 overflow 裁剪；按触发项实测坐标摆放
        const r = btnRef.current?.getBoundingClientRect();
        if (r) {
          setPos({
            left: Math.min(r.right - 4, window.innerWidth - 240),
            top: Math.max(4, Math.min(r.top - 5, window.innerHeight - 180)),
          });
        }
      }}
      onMouseLeave={() => setPos(null)}
    >
      <button ref={btnRef} class="entry">
        <span>{entry.label} ▸</span>
      </button>
      {pos && (
        <div
          class="dropdown sub"
          style={`position: fixed; left: ${pos.left}px; top: ${pos.top}px; max-height: none;`}
        >
          <EntryList entries={entry.submenu ?? []} onPick={onPick} />
        </div>
      )}
    </div>
  );
}

export function MenuBar({ menus }: { menus: MenuDef[] }) {
  const [openIdx, setOpenIdx] = useState<number | null>(null);
  const barRef = useRef<HTMLDivElement>(null);

  // useLayoutEffect 同步挂载关闭监听:useEffect 的 rAF 调度在窗口被遮挡/节流时
  // 会迟一拍,导致打开后的第一次外部点击关不掉菜单
  useLayoutEffect(() => {
    if (openIdx === null) return;
    const close = (e: MouseEvent) => {
      if (barRef.current && !barRef.current.contains(e.target as Node)) {
        setOpenIdx(null);
      }
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpenIdx(null);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [openIdx]);

  const evalEntries = (m: MenuDef): MenuEntry[] =>
    typeof m.entries === "function" ? m.entries() : m.entries;

  return (
    <div class="menubar" ref={barRef}>
      {menus.map((m, i) => (
        <div class="menu-item" key={m.title}>
          <button
            class={`menu-label${openIdx === i ? " open" : ""}`}
            onClick={() => {
              if (openIdx === i) {
                setOpenIdx(null);
              } else {
                m.onOpen?.();
                setOpenIdx(i);
              }
            }}
            onMouseEnter={() => {
              // 已有展开菜单时悬停切换
              if (openIdx !== null && openIdx !== i) {
                m.onOpen?.();
                setOpenIdx(i);
              }
            }}
          >
            {m.title}
          </button>
          {openIdx === i && (
            <div class="dropdown">
              <EntryList entries={evalEntries(m)} onPick={() => setOpenIdx(null)} />
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
