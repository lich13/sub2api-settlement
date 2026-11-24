import React, { useEffect, useRef } from "react";
import { X } from "lucide-react";

export function Modal({
  title,
  children,
  close,
  wide = false,
  busy = false,
  dirty = false,
}: {
  title: string;
  children: React.ReactNode;
  close: () => void;
  wide?: boolean;
  busy?: boolean;
  dirty?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const composing = useRef(false);
  const imeEscape = useRef(false);
  useEffect(() => {
    ref.current?.showModal();
    const el = ref.current;
    return () => el?.close();
  }, []);
  return (
    <dialog
      ref={ref}
      className={"modal " + (wide ? "wide" : "")}
      onCompositionStart={() => {
        composing.current = true;
      }}
      onCompositionEnd={() => {
        composing.current = false;
      }}
      onKeyDown={(e) => {
        if (
          composing.current ||
          e.nativeEvent.isComposing ||
          e.nativeEvent.keyCode === 229
        ) {
          if (e.key === "Escape" || e.nativeEvent.keyCode === 229) {
            imeEscape.current = true;
            e.preventDefault();
            e.stopPropagation();
          }
          return;
        }
        imeEscape.current = false;
        if (e.key === "Escape" && (busy || dirty)) {
          e.preventDefault();
          e.stopPropagation();
        }
      }}
      onCancel={(e) => {
        e.preventDefault();
        const blocked = composing.current || imeEscape.current || busy || dirty;
        imeEscape.current = false;
        if (!blocked) close();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget && !busy && !dirty) close();
      }}
    >
      <header>
        <h2>{title}</h2>
        <button
          type="button"
          className="icon-button"
          disabled={busy}
          onClick={() => !busy && close()}
          aria-label="关闭"
        >
          <X size={19} />
        </button>
      </header>
      {children}
    </dialog>
  );
}
