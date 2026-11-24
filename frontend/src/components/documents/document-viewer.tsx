"use client";

import { useEffect, useState } from "react";
import { Download, Loader2, Printer } from "lucide-react";

import { DocumentBody } from "@/components/documents/document-body";
import { DocumentDialog } from "@/components/documents/document-dialog";
import { Alert, Button, Skeleton } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { downloadContent, downloadName } from "@/lib/documents";
import { saveFile } from "@/lib/download";
import type { DocumentSummary, FinancialDocument } from "@/types/documents";

/*
 * The document viewer: one statement or trade confirmation, fetched when it
 * opens, with "Print / Save as PDF" and "Download (.json)", in the shared
 * document frame (document-dialog.tsx: a modal, and a paper copy to print
 * with every hash in full).
 *
 * The download is the document exactly as the API served it, as JSON: the
 * reference hash can be recomputed from it.
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
  const [doc, setDoc] = useState<FinancialDocument | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [round, setRound] = useState(0);

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

  return (
    <DocumentDialog
      eyebrow={summary.kind === "trade_confirmation" ? "Trade confirmation" : "Account statement"}
      title={summary.title}
      testId="document-viewer"
      onClose={onClose}
      printCopy={doc ? <DocumentBody doc={doc} variant="print" /> : null}
      footer={
        <>
          <Button
            variant="secondary"
            onClick={() => doc && saveFile(downloadName(doc), downloadContent(doc), "application/json")}
            disabled={!doc}
            data-testid="document-download"
          >
            <Download className="size-4" aria-hidden /> Download (.json)
          </Button>
          <Button onClick={() => window.print()} disabled={!doc} data-testid="document-print">
            <Printer className="size-4" aria-hidden /> Print / Save as PDF
          </Button>
        </>
      }
    >
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
    </DocumentDialog>
  );
}
