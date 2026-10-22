"use client";

import { RefreshCw } from "lucide-react";

import { Alert, Button, Skeleton, cx } from "@/components/ui/primitives";
import type { ResumenAml } from "@/lib/api/types";
import { formatMexicoCity } from "@/lib/aml";

interface Counter {
  label: string;
  value: number;
  /** Red when above zero. */
  alarm?: boolean;
  hint?: string;
}

/**
 * Counters of the console (GET /aml/resumen), the observation-mode banner and
 * the live indicator: the time of the last successful poll, in Mexico City.
 */
export function SummaryBar({
  summary,
  lastUpdated,
  refreshing,
  stale,
  daysNotice,
  onRefresh,
}: {
  summary: ResumenAml | null;
  lastUpdated: number | null;
  refreshing: boolean;
  /** The last poll failed: the counters may be out of date. */
  stale: boolean;
  /** Warning window of UIF deadlines (parameter `dias_aviso_plazo`, 7 by default). */
  daysNotice: number;
  onRefresh: () => void;
}) {
  const counters: Counter[] = summary
    ? [
        { label: "Pending", value: summary.pendientes },
        { label: "In review", value: summary.en_revision },
        { label: "Critical open", value: summary.criticas_abiertas, alarm: true },
        { label: "Frozen clients", value: summary.congelamientos_activos },
        {
          label: "Awaiting second approval",
          value: summary.solicitudes_pendientes + summary.propuestas_pendientes,
          hint: "Unfreeze and release requests, plus rule changes",
        },
        { label: "Freeze review overdue", value: summary.sla_congelamiento_vencidos, alarm: true },
        { label: `UIF reports due in ${daysNotice} days`, value: summary.reportes_por_vencer },
        { label: "UIF reports overdue", value: summary.reportes_vencidos, alarm: true },
      ]
    : [];

  return (
    <section aria-label="Summary" className="space-y-3">
      {summary?.modo_observacion ? (
        <Alert tone="warning">
          Observation mode is on: alerts are recorded but no account is restricted automatically.
        </Alert>
      ) : null}
      <div className="flex flex-col gap-3 lg:flex-row lg:items-stretch">
        <dl className="grid flex-1 grid-cols-2 gap-px overflow-hidden rounded-xl border border-border bg-border sm:grid-cols-4 xl:grid-cols-8">
          {summary
            ? counters.map((c) => (
                <div key={c.label} className="bg-surface px-3 py-2.5" title={c.hint}>
                  <dt className="text-[11px] leading-tight font-medium text-muted">{c.label}</dt>
                  <dd
                    className={cx(
                      "mt-1 text-xl font-semibold tabular-nums",
                      c.alarm && c.value > 0 ? "text-danger" : "text-ink",
                    )}
                  >
                    {c.value}
                  </dd>
                </div>
              ))
            : Array.from({ length: 8 }, (_, i) => (
                <div key={i} className="bg-surface px-3 py-2.5">
                  <Skeleton className="h-3 w-20" />
                  <Skeleton className="mt-2 h-6 w-8" />
                </div>
              ))}
        </dl>
        <div className="flex items-center justify-between gap-3 rounded-xl border border-border bg-surface px-3 py-2 lg:flex-col lg:items-end lg:justify-center">
          <span className="inline-flex items-center gap-1.5 text-xs text-muted" aria-live="polite">
            <span className="relative flex size-2">
              {stale ? null : <span className="absolute inline-flex size-full animate-ping rounded-full bg-verified opacity-60" />}
              <span className={cx("relative inline-flex size-2 rounded-full", stale ? "bg-warning" : "bg-verified")} />
            </span>
            {stale ? "Reconnecting" : "Live"}
            {lastUpdated !== null ? ` · updated ${formatMexicoCity(lastUpdated, "time")}` : ""}
          </span>
          <Button variant="secondary" size="sm" onClick={onRefresh} disabled={refreshing}>
            <RefreshCw className={cx("size-3.5", refreshing && "animate-spin")} aria-hidden />
            Refresh
          </Button>
        </div>
      </div>
    </section>
  );
}
