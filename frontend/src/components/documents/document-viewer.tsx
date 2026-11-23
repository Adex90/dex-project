"use client";

import { useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Download, Loader2, Printer, X } from "lucide-react";

import { DocumentBody } from "@/components/documents/document-body";
import { Alert, Button, Skeleton, cx } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { downloadContent, downloadName } from "@/lib/documents";
import type { DocumentSummary, FinancialDocument } from "@/types/documents";

/*
 * The document viewer: one statement or trade confirmation, fetched when it
 * opens, with "Print / Save as PDF" and "Download (.json)".
 *
 * - A native <dialog> opened as a modal: focus stays inside, the page behind
 *   is inert, Escape and the backdrop close it, and focus returns to the row
 *   that opened it. Full screen on a phone, a centered sheet from `sm` up.
 * - Printing: while a document is open, a second copy of it sits at the end
 *   of <body> (`.document-print-copy`, hidden on screen). The print styles in
 *   globals.css print that copy alone, in paper colors and normal flow, so a
 *   long statement breaks across pages instead of being clipped to the
 *   dialog's scroll box. The copy shows every hash in full.
 * - The download is the document exactly as the API served it, as JSON: the
 *   reference hash can be recomputed from it.
 */

export function DocumentViewer({
  summary,
  load,
  onClose,
}: {
  /** The inbox row that opened it: its title shows while the document loads. */
  summary: DocumentSummary;
  /** Loads the full document; rejects with the error to show. */
  load: (id: string, signal: AbortSignal) => Promise<FinancialDocument>;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [doc, setDoc] = useState<FinancialDocument | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [round, setRound] = useState(0);

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

  useEffect(() => {
    const controller = new AbortController();
    load(summary.id, controller.signal).then(
      (loaded) => {
        setDoc(loaded);
        setError(null);
      },
      (cause: unknown) => {
        if (!controller.signal.aborted) setError(cause);
      },
    );
    return () => controller.abort();
  }, [load, summary.id, round]);

  function download() {
    if (!doc) return;
    const url = URL.createObjectURL(new Blob([downloadContent(doc)], { type: "application/json" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = downloadName(doc);
    document.body.append(link);
    link.click();
    link.remove();
    // Revoked after the click has been handled: the download keeps its own reference.
    setTimeout(() => URL.revokeObjectURL(url), 1_000);
  }

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
        data-testid="document-viewer"
      >
        <div className="flex h-full flex-col">
          <header className="flex shrink-0 items-start justify-between gap-3 border-b border-border px-4 py-3 sm:px-5">
            <div className="min-w-0">
              <p className="text-[10px] font-semibold tracking-[0.2em] text-synthetic-yellow uppercase">
                {summary.kind === "trade_confirmation" ? "Trade confirmation" : "Account statement"}
              </p>
              <h2 id={titleId} className="truncate text-base font-semibold text-ink">
                {summary.title}
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

          <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6">
            {doc ? (
              <DocumentBody doc={doc} />
            ) : error ? (
              <Alert tone="error" title="Could not open this document">
                {describeError(error)}{" "}
                <Button variant="ghost" size="sm" onClick={() => setRound((n) => n + 1)}>
                  Try again
                </Button>
              </Alert>
            ) : (
              <div className="space-y-4" aria-busy="true">
                <p className="flex items-center gap-2 text-sm text-muted">
                  <Loader2 className="size-4 animate-spin text-synthetic-yellow" aria-hidden /> Opening {summary.folio}…
                </p>
                <Skeleton className="h-16 w-full" />
                <Skeleton className="h-40 w-full" />
                <Skeleton className="h-24 w-full" />
              </div>
            )}
          </div>

          <footer className="flex shrink-0 flex-wrap items-center justify-end gap-2 border-t border-border px-4 py-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))] sm:px-5 sm:pb-3">
            <Button variant="secondary" onClick={download} disabled={!doc} data-testid="document-download">
              <Download className="size-4" aria-hidden /> Download (.json)
            </Button>
            <Button onClick={() => window.print()} disabled={!doc} data-testid="document-print">
              <Printer className="size-4" aria-hidden /> Print / Save as PDF
            </Button>
          </footer>
        </div>
      </dialog>
      {doc ? <PrintCopy doc={doc} /> : null}
    </>
  );
}

/** The copy the browser prints: at the end of <body>, hidden on screen (see globals.css). */
function PrintCopy({ doc }: { doc: FinancialDocument }) {
  const [host, setHost] = useState<HTMLElement | null>(null);
  useEffect(() => setHost(document.body), []);
  if (!host) return null;
  return createPortal(
    <div className="document-print-copy" aria-hidden="true">
      <DocumentBody doc={doc} variant="print" />
    </div>,
    host,
  );
}
