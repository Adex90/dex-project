"use client";

import { useCallback, useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { Printer } from "lucide-react";

import { DocumentDialog } from "@/components/documents/document-dialog";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { ConstanciaBody } from "@/components/taxes/constancia-body";
import { FiscalStatusCard } from "@/components/taxes/fiscal-status-card";
import { TaxExportPanel } from "@/components/taxes/tax-export-panel";
import { TaxSummary } from "@/components/taxes/tax-summary";
import { W8benDialog } from "@/components/taxes/w8ben-dialog";
import { Alert, Button, Skeleton } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { saveFile } from "@/lib/download";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { exportFileName, parseTaxCenter, parseW8benState, verifyExport, type ExportCheck } from "@/lib/taxes";
import { useAuthStore } from "@/store/auth";
import type { TaxCenter, W8benCertification, W8benState } from "@/types/taxes";

const UNREADABLE = "Your tax information could not be read. Please try again.";

/*
 * Taxes (roadmap item 26): the Tax Center.
 *
 * - Fiscal status (components/taxes/fiscal-status-card.tsx): the RFC, CURP,
 *   residence and citizenship on file, and the W-8BEN: status, validity and
 *   the gold Certify / Renew flow (w8ben-dialog.tsx).
 * - Realized gains & withholding (tax-summary.tsx) for the selected fiscal
 *   year: gross sales, cost basis, net result, fixed-term interest, ISR
 *   withheld and an ISR preview from the year's published rates.
 * - Certificates & export (tax-export-panel.tsx): the accounting CSV,
 *   reconciled with the summary before it is saved, and the Constancia de
 *   Retenciones preview, printed like the Document Inbox's documents.
 *
 * Layout: one column on phones (status, summary, export); from xl, the
 * summary in two thirds and the status and export beside it.
 */
export default function TaxesPage() {
  const router = useRouter();
  const { token } = useDashboard();
  const [year, setYear] = useState<number | null>(null);
  const [center, setCenter] = useState<TaxCenter | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [certifying, setCertifying] = useState(false);
  const [previewing, setPreviewing] = useState(false);

  const endSession = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  /** The Tax Center for `year` (the current one when null); rethrows after ending the session on a 401. */
  const fetchCenter = useCallback(
    async (wanted: number | null, signal?: AbortSignal): Promise<TaxCenter> => {
      try {
        const parsed = parseTaxCenter(await api.taxCenter(token, wanted ?? undefined, signal));
        if (!parsed) throw new Error(UNREADABLE);
        return parsed;
      } catch (error) {
        if (error instanceof ApiError && error.isUnauthorized) endSession();
        throw error;
      }
    },
    [token, endSession],
  );

  const load = useCallback(
    async (wanted: number | null, signal?: AbortSignal) => {
      setLoading(true);
      try {
        setCenter(await fetchCenter(wanted, signal));
        setLoadError(null);
      } catch (error) {
        if (error instanceof DOMException && error.name === "AbortError") return;
        setLoadError(describeError(error));
      } finally {
        setLoading(false);
      }
    },
    [fetchCenter],
  );

  useEffect(() => {
    const controller = new AbortController();
    void load(year, controller.signal);
    return () => controller.abort();
  }, [load, year]);

  const certify = useCallback(
    async (body: W8benCertification): Promise<W8benState> => {
      let raw: unknown;
      try {
        raw = await api.certifyW8ben(token, body);
      } catch (error) {
        if (error instanceof ApiError && error.isUnauthorized) endSession();
        throw error;
      }
      // A 2xx means it is on record: if the answer cannot be read, read the record.
      const state = parseW8benState(raw) ?? (await fetchCenter(year)).w8ben;
      setCenter((c) => (c ? { ...c, w8ben: state } : c));
      return state;
    },
    [token, endSession, fetchCenter, year],
  );

  const download = useCallback(async (): Promise<ExportCheck> => {
    if (!center) throw new Error(UNREADABLE);
    const wanted = center.summary.year;
    // The summary again, then the export: both from the ledgers as they are now.
    const fresh = await fetchCenter(wanted);
    setCenter(fresh);
    let text: string;
    try {
      ({ text } = await api.taxExport(token, wanted));
    } catch (error) {
      if (error instanceof ApiError && error.isUnauthorized) endSession();
      throw new Error(describeError(error));
    }
    const check = verifyExport(text, fresh.summary);
    if (check.ok) saveFile(exportFileName(wanted), text, "text/csv;charset=utf-8");
    return check;
  }, [center, fetchCenter, token, endSession]);

  return (
    <div className="space-y-6" data-testid="taxes-page">
      <div className="space-y-1">
        <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">Tax center</p>
        <h2 className="text-2xl font-semibold tracking-tight text-ink">Your taxes, from the ledgers</h2>
        <p className="max-w-2xl text-sm text-muted">
          Your fiscal status and W-8BEN, what you realized and earned this fiscal year, and the files your accountant
          needs. Every figure comes from the same hash-chained ledgers as your statements.
        </p>
      </div>

      {loadError ? (
        <Alert tone="error" title="Could not load your tax information">
          {loadError}{" "}
          <Button variant="ghost" size="sm" onClick={() => void load(year)}>
            Retry
          </Button>
        </Alert>
      ) : null}

      {center ? (
        <div className="grid grid-cols-1 items-start gap-6 xl:grid-cols-3">
          <div className="min-w-0 xl:col-start-3 xl:row-start-1">
            <FiscalStatusCard profile={center.profile} w8ben={center.w8ben} onCertify={() => setCertifying(true)} />
          </div>
          <div className="min-w-0 xl:col-span-2 xl:col-start-1 xl:row-span-2 xl:row-start-1">
            <TaxSummary summary={center.summary} years={center.years} onYear={setYear} loading={loading} />
          </div>
          <div className="min-w-0 xl:col-start-3 xl:row-start-2">
            <TaxExportPanel
              key={center.summary.year}
              year={center.summary.year}
              inProgress={center.summary.in_progress}
              onDownload={download}
              onConstancia={() => setPreviewing(true)}
            />
          </div>
        </div>
      ) : loadError ? null : (
        <div className="grid gap-6 xl:grid-cols-3" aria-busy="true">
          <Skeleton className="h-96 xl:col-span-2" />
          <Skeleton className="h-96" />
        </div>
      )}

      {certifying && center ? (
        <W8benDialog
          profile={center.profile}
          renewing={center.w8ben.status !== "pending"}
          treatyRate={center.w8ben.us_dividend_rate_treaty}
          onSubmit={certify}
          onClose={() => setCertifying(false)}
        />
      ) : null}

      {previewing && center ? (
        <DocumentDialog
          eyebrow="Constancia de Retenciones"
          title={`Fiscal year ${center.summary.year} · preview`}
          testId="constancia-viewer"
          onClose={() => setPreviewing(false)}
          printCopy={<ConstanciaBody center={center} variant="print" />}
          footer={
            <Button onClick={() => window.print()} data-testid="constancia-print">
              <Printer className="size-4" aria-hidden /> Print / Save as PDF
            </Button>
          }
        >
          <ConstanciaBody center={center} />
        </DocumentDialog>
      ) : null}
    </div>
  );
}
