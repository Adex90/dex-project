"use client";

import { useState } from "react";
import { CheckCircle2, Download, FileText } from "lucide-react";

import { Alert, Button } from "@/components/ui/primitives";
import { exportFileName, type ExportCheck } from "@/lib/taxes";

/*
 * Certificates and export, for the selected fiscal year:
 *
 * - The accounting CSV (RFC 4180): every movement that changed the balance,
 *   with each trade's cost basis and result. Before it is saved, the page
 *   reconciles it with the year's summary to the unit; a CSV that does not
 *   match is not saved.
 * - The Constancia de Retenciones, as a preview to print or save as PDF.
 */

export function TaxExportPanel({
  year,
  inProgress,
  onDownload,
  onConstancia,
}: {
  year: number;
  inProgress: boolean;
  /** Fetches, reconciles and saves the CSV; resolves with the check, rejects with the API's error. */
  onDownload: () => Promise<ExportCheck>;
  onConstancia: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [check, setCheck] = useState<ExportCheck | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function download() {
    setBusy(true);
    setCheck(null);
    setError(null);
    try {
      setCheck(await onDownload());
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "The export could not be downloaded.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="border border-border bg-surface" aria-labelledby="tax-export-title" data-testid="tax-export">
      <header className="border-b border-border px-4 py-3 sm:px-5">
        <h3 id="tax-export-title" className="text-sm font-semibold text-ink">
          Certificates &amp; export
        </h3>
      </header>
      <div className="divide-y divide-border">
        <div className="space-y-3 px-4 py-4 sm:px-5">
          <div className="flex items-start gap-3">
            <Download className="mt-0.5 size-4 shrink-0 text-synthetic-yellow" aria-hidden />
            <div className="min-w-0 space-y-1">
              <p className="text-sm font-semibold text-ink">Accounting CSV · {year}</p>
              <p className="text-xs text-ink-2">
                Every movement that changed your balance, with each trade&apos;s cost basis and result, interest and the
                ledger&apos;s hash. Checked against the summary before it is saved.
              </p>
            </div>
          </div>
          <Button
            variant="secondary"
            onClick={() => void download()}
            loading={busy}
            className="w-full"
            data-testid="tax-csv"
          >
            Download {exportFileName(year)}
          </Button>
          {check?.ok ? (
            <p className="flex items-center gap-1.5 text-xs text-synthetic-yellow" data-testid="tax-csv-verified">
              <CheckCircle2 className="size-3.5" aria-hidden /> Verified: {check.rows} movement
              {check.rows === 1 ? "" : "s"}, totals match the summary.
            </p>
          ) : null}
          {check && !check.ok ? (
            <Alert tone="error" title="Not saved">
              {check.problem} Reload and try again.
            </Alert>
          ) : null}
          {error ? (
            <Alert tone="error" title="Could not download the export">
              {error}
            </Alert>
          ) : null}
        </div>

        <div className="space-y-3 px-4 py-4 sm:px-5">
          <div className="flex items-start gap-3">
            <FileText className="mt-0.5 size-4 shrink-0 text-synthetic-yellow" aria-hidden />
            <div className="min-w-0 space-y-1">
              <p className="text-sm font-semibold text-ink">Constancia de Retenciones · {year}</p>
              <p className="text-xs text-ink-2">
                Interest, withholding and the result of selling listed shares, ready to print.{" "}
                {inProgress ? "A preview: the year is still in progress." : "A preview of the year's constancia."}
              </p>
            </div>
          </div>
          <Button onClick={onConstancia} className="w-full" data-testid="tax-constancia">
            Open preview
          </Button>
        </div>
      </div>
    </section>
  );
}
