"use client";

import type { KeyboardEvent } from "react";
import { ArrowDown, LockKeyhole } from "lucide-react";

import { AlertStateBadge, DeadlineText, LevelBadge, MxTime, ScoreMeter, personLabel } from "@/components/admin/aml/parts";
import { Money } from "@/components/ui/data";
import { Badge, Button, Skeleton, cx } from "@/components/ui/primitives";
import type { AlertaAml, Uuid } from "@/lib/api/types";
import { DECISION_LABEL, isOpen, type Thresholds } from "@/lib/aml";

/** UIF deadline cell: only meaningful while a report may still be due. */
function DeadlineCell({ a, now }: { a: AlertaAml; now: Date }) {
  if (a.estado === "FALSO_POSITIVO") return <span className="text-muted">—</span>;
  if (a.folio_uif) return <span className="text-verified">Reported</span>;
  return <DeadlineText fechaLimite={a.fecha_limite_reporte} now={now} />;
}

const head = "py-2 pr-3 font-medium whitespace-nowrap";

/**
 * The alert feed, severity first (`sortAlerts`). Alerts that arrive by polling
 * wait behind the "N new alerts" pill instead of shifting rows under the
 * officer's cursor; rows updated in place are briefly highlighted.
 */
export function AlertFeed({
  rows,
  pendingCount,
  onMergePending,
  highlighted,
  thresholds,
  me,
  now,
  selectedId,
  onOpen,
  loading,
  hasMore,
  loadingMore,
  onLoadMore,
  filtered,
}: {
  rows: AlertaAml[];
  pendingCount: number;
  onMergePending: () => void;
  highlighted: ReadonlySet<string>;
  thresholds: Thresholds;
  me: Uuid;
  now: Date;
  selectedId: string | null;
  onOpen: (id: Uuid) => void;
  loading: boolean;
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => void;
  /** Filters or a search are narrowing the view (changes the empty-state copy). */
  filtered: boolean;
}) {
  const onKeyDown = (event: KeyboardEvent<HTMLTableRowElement>, id: Uuid) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onOpen(id);
    }
  };

  return (
    <div>
      <div aria-live="polite" className="flex justify-center">
        {pendingCount > 0 ? (
          <button
            type="button"
            onClick={onMergePending}
            className="mt-3 inline-flex items-center gap-1.5 rounded-full bg-accent px-3 py-1 text-xs font-semibold text-accent-ink shadow-sm transition hover:brightness-110 focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none"
          >
            <ArrowDown className="size-3.5" aria-hidden />
            {pendingCount} new {pendingCount === 1 ? "alert" : "alerts"}
          </button>
        ) : null}
      </div>

      {loading && rows.length === 0 ? (
        <div className="space-y-2 p-5" aria-busy="true">
          {Array.from({ length: 6 }, (_, i) => (
            <Skeleton key={i} className="h-9 w-full" />
          ))}
        </div>
      ) : rows.length === 0 ? (
        <p className="p-10 text-center text-sm text-muted">
          {filtered ? "No alerts match these filters." : "No open alerts. New alerts appear here within seconds."}
        </p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[1180px] border-collapse text-left text-[13px]">
            <caption className="sr-only">Alerts, most severe first. Press Enter on a row to open its case file.</caption>
            <thead>
              <tr className="border-b border-border bg-surface-2/60 text-[11px] tracking-wide text-muted uppercase">
                <th scope="col" className={cx(head, "pl-5")}>Folio</th>
                <th scope="col" className={head}>Detected</th>
                <th scope="col" className={head}>Client</th>
                <th scope="col" className={head}>Rule</th>
                <th scope="col" className={head}>Level</th>
                <th scope="col" className={head}>Score</th>
                <th scope="col" className={cx(head, "text-right")}>Amount</th>
                <th scope="col" className={cx(head, "text-right")}>Hits</th>
                <th scope="col" className={head}>Decision</th>
                <th scope="col" className={head}>Status</th>
                <th scope="col" className={head}>
                  <span className="sr-only">Client restricted</span>
                </th>
                <th scope="col" className={head}>UIF deadline</th>
                <th scope="col" className={cx(head, "pr-5")}>Assignee</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((a) => {
                const amount = a.monto_acumulado ?? a.monto;
                const restricted = a.congelamiento?.estado === "ACTIVO";
                return (
                  <tr
                    key={a.id}
                    tabIndex={0}
                    onClick={() => onOpen(a.id)}
                    onKeyDown={(e) => onKeyDown(e, a.id)}
                    data-testid="aml-alert-row"
                    data-level={a.nivel_riesgo}
                    className={cx(
                      "cursor-pointer border-b border-border/70 transition-colors duration-700 hover:bg-surface-2/60",
                      "focus-visible:bg-surface-2 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent",
                      highlighted.has(a.id) && "bg-accent-soft",
                      selectedId === a.id && "bg-surface-2/80",
                      !isOpen(a) && "text-ink-2",
                    )}
                  >
                    <td className="py-2 pr-3 pl-5 font-mono text-xs font-semibold text-ink">#{a.folio}</td>
                    <td className="py-2 pr-3 text-ink-2">
                      <MxTime at={a.timestamp} relative now={now} />
                    </td>
                    <td className="max-w-[220px] py-2 pr-3">
                      <span className="flex items-center gap-1.5">
                        <span className="truncate font-medium text-ink" title={a.cliente.nombre_completo}>
                          {a.cliente.nombre_completo}
                        </span>
                        {a.cliente.es_interno ? <Badge tone="accent">Staff</Badge> : null}
                        {a.cliente.es_pep ? <Badge tone="warning">PEP</Badge> : null}
                      </span>
                    </td>
                    <td className="max-w-[220px] py-2 pr-3">
                      <span className="block font-mono text-[11px] text-ink">{a.regla.codigo}</span>
                      <span className="block truncate text-xs text-muted" title={a.regla.nombre}>
                        {a.regla.nombre}
                      </span>
                    </td>
                    <td className="py-2 pr-3">
                      <LevelBadge level={a.nivel_riesgo} />
                    </td>
                    <td className="py-2 pr-3">
                      <ScoreMeter score={a.puntaje} level={a.nivel_riesgo} thresholds={thresholds} />
                    </td>
                    <td className="py-2 pr-3 text-right whitespace-nowrap">
                      {amount ? <Money value={amount} /> : <span className="text-muted">—</span>}
                    </td>
                    <td className="py-2 pr-3 text-right tabular-nums">{a.ocurrencias}</td>
                    <td className="py-2 pr-3 whitespace-nowrap">
                      <span className={cx(a.decision === "BLOQUEADA" && "font-semibold text-danger", a.decision === "RETENIDA" && "font-semibold text-warning")}>
                        {DECISION_LABEL[a.decision]}
                      </span>
                    </td>
                    <td className="py-2 pr-3">
                      <span className="flex items-center gap-1">
                        <AlertStateBadge estado={a.estado} />
                        {a.escalada ? <Badge tone="danger">Escalated</Badge> : null}
                      </span>
                    </td>
                    <td className="py-2 pr-3">
                      {restricted ? (
                        <span title="Client restricted" className="inline-flex text-danger">
                          <LockKeyhole className="size-3.5" aria-hidden />
                          <span className="sr-only">Client restricted</span>
                        </span>
                      ) : null}
                    </td>
                    <td className="py-2 pr-3">
                      <DeadlineCell a={a} now={now} />
                    </td>
                    <td className="max-w-[160px] truncate py-2 pr-5 text-ink-2">{personLabel(a.asignada_a, me, "—")}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {hasMore ? (
        <div className="border-t border-border p-3 text-center">
          <Button variant="ghost" size="sm" onClick={onLoadMore} loading={loadingMore}>
            Load older alerts
          </Button>
        </div>
      ) : null}
    </div>
  );
}
