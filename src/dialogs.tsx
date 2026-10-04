// 模态对话框：关闭标签确认 / 退出确认 / 跳转到行。

import { useEffect, useRef } from "preact/hooks";

export type Modal =
  | { kind: "close-tab"; title: string }
  | { kind: "exit"; dirtyWithPath: number; dirtyUntitled: number }
  | { kind: "goto" }
  | null;

export function Dialogs({
  modal,
  onConfirmCloseTab,
  onDiscardCloseTab,
  onExitSaveAll,
  onExitDiscard,
  onGoto,
  onCancel,
}: {
  modal: Modal;
  onConfirmCloseTab(): void;
  onDiscardCloseTab(): void;
  onExitSaveAll(): void;
  onExitDiscard(): void;
  onGoto(line: number): void;
  onCancel(): void;
}) {
  const gotoRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (modal?.kind === "goto") {
      gotoRef.current?.focus();
      gotoRef.current?.select();
    }
  }, [modal?.kind]);

  if (!modal) return null;

  if (modal.kind === "close-tab") {
    return (
      <div class="modal-overlay">
        <div class="modal">
          <h3>未保存的修改</h3>
          <p>“{modal.title}” 有未保存的修改。</p>
          <div class="btn-row">
            <button onClick={onCancel}>取消</button>
            <button onClick={onDiscardCloseTab}>不保存</button>
            <button class="primary" onClick={onConfirmCloseTab}>保存并关闭</button>
          </div>
        </div>
      </div>
    );
  }

  if (modal.kind === "exit") {
    const hasUntitled = modal.dirtyUntitled > 0;
    const total = modal.dirtyWithPath + modal.dirtyUntitled;
    return (
      <div class="modal-overlay">
        <div class="modal">
          <h3>未保存的修改</h3>
          <p>
            {hasUntitled
              ? "存在未命名的未保存标签页，保存需先逐个另存。"
              : `有 ${total} 个标签页有未保存的修改。`}
          </p>
          <div class="btn-row">
            <button onClick={onCancel}>取消</button>
            <button onClick={onExitDiscard}>不保存并退出</button>
            <button class="primary" onClick={onExitSaveAll} disabled={hasUntitled}>
              保存全部并退出
            </button>
          </div>
        </div>
      </div>
    );
  }

  // goto
  const submit = () => {
    const v = parseInt(gotoRef.current?.value ?? "", 10);
    if (Number.isFinite(v) && v > 0) onGoto(v);
  };
  return (
    <div class="modal-overlay">
      <div class="modal">
        <h3>跳转到行</h3>
        <input
          ref={gotoRef}
          type="text"
          placeholder="行号:"
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
            } else if (e.key === "Escape") {
              e.preventDefault();
              onCancel();
            }
          }}
        />
        <div class="btn-row">
          <button onClick={onCancel}>取消</button>
          <button class="primary" onClick={submit}>跳转</button>
        </div>
      </div>
    </div>
  );
}
