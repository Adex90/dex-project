"use client";

import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";

import { cx } from "@/components/ui/primitives";

/*
 * The frame every printable document opens in: the Document Inbox's viewer
 * and the Tax Center's Constancia de Retenciones preview.
 *
 * - A native <dialog> opened as a modal: focus stays inside, the page behind
 *   is inert, Escape and the backdrop close it, and focus returns to what
 *   opened it. Full screen on a phone, a centered sheet from `sm` up.
 * - Printing: `printCopy` is rendered a second time at the end of <body>
 *   (`.document-print-copy`, hidden on screen). The print styles in
 *   globals.css print that copy alone, in paper colors and normal flow, so a
 *   long document breaks across pages instead of being clipped to the
 *   dialog's scroll box.
 */

export function DocumentDialog({
  eyebrow,
  title,
  testId,
  onClose,
  footer,
  printCopy,
  children,
}: {
  eyebrow: string;
  title: string;
  testId: string;
  onClose: () => void;
  footer?: ReactNode;
  /** What the browser prints (null until there is something to print). */
  printCopy?: ReactNode;
  children: ReactNode;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    dialog.showModal();
    const root = document.documentElement;
    const overflow = root.style.overflow;
    root.style.overflow = "hidden";
    return () => {
      root.style.overflow = overflow;
      if (dialog.open) dialog.close();
    };
  }, []);

  return (
    <>
      <dialog
        ref={dialogRef}
        aria-labelledby={titleId}
        onClose={onClose}
        onClick={(event) => {
          // A click on the backdrop lands on the dialog element itself.
          if (event.target === dialogRef.current) dialogRef.current?.close();
        }}
        className={cx(
          // Phone: the whole screen. Wider: a sheet in the middle.
          "inset-0 m-0 h-dvh max-h-dvh w-full max-w-full overflow-hidden p-0",
          "bg-surface text-ink sm:m-auto sm:h-[min(92dvh,1100px)] sm:max-w-3xl sm:border sm:border-border",
          "backdrop:bg-black/80 backdrop:backdrop-blur-[2px]",
          "transition duration-200 starting:translate-y-4 starting:opacity-0 motion-reduce:transition-none",
        )}
        data-testid={testId}
      >
        <div className="flex h-full flex-col">
          <header className="flex shrink-0 items-start justify-between gap-3 border-b border-border px-4 py-3 sm:px-5">
            <div className="min-w-0">
              <p className="text-[10px] font-semibold tracking-[0.2em] text-synthetic-yellow uppercase">{eyebrow}</p>
              <h2 id={titleId} className="truncate text-base font-semibold text-ink">
                {title}
              </h2>
            </div>
            <button
              type="button"
              onClick={() => dialogRef.current?.close()}
              className="grid size-9 shrink-0 place-items-center text-muted transition hover:bg-surface-2 hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
              aria-label="Close"
            >
              <X className="size-5" aria-hidden />
            </button>
          </header>

          <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6">{children}</div>

          {footer ? (
            <footer className="flex shrink-0 flex-wrap items-center justify-end gap-2 border-t border-border px-4 py-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))] sm:px-5 sm:pb-3">
              {footer}
            </footer>
          ) : null}
        </div>
      </dialog>
      {printCopy ? <PrintCopy>{printCopy}</PrintCopy> : null}
    </>
  );
}

/** The copy the browser prints: at the end of <body>, hidden on screen (see globals.css). */
export function PrintCopy({ children }: { children: ReactNode }) {
  const [host, setHost] = useState<HTMLElement | null>(null);
  useEffect(() => setHost(document.body), []);
  if (!host) return null;
  return createPortal(
    <div className="document-print-copy" aria-hidden="true">
      {children}
    </div>,
    host,
  );
}
