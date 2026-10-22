"use client";

import { useState } from "react";
import { History } from "lucide-react";

import { MxTime, Section, personLabel } from "@/components/admin/aml/parts";
import { cx } from "@/components/ui/primitives";
import type { BitacoraAml, Uuid } from "@/lib/api/types";
import { ACTOR_TYPE_LABEL, eventLabel, stateLabel } from "@/lib/aml";

/** Events that loosen or tighten a restriction stand out in the timeline. */
const STRONG_EVENTS = new Set([
  "CONGELAMIENTO_APLICADO",
  "CONGELAMIENTO_LEVANTADO",
  "ALERTA_CONFIRMADA",
  "ALERTA_FALSO_POSITIVO",
  "LEVANTAMIENTO_APROBADO",
  "RETENCION_LIBERADA",
]);

/**
 * The audit trail of the alert and its restriction (aml.bitacora, newest
 * first): event, actor or "System", state change and reason. Case-file views
 * are logged too; they are folded away unless asked for.
 */
export function HistoryTimeline({ historial, me }: { historial: BitacoraAml[]; me: Uuid }) {
  const [showViews, setShowViews] = useState(false);
  const views = historial.filter((h) => h.evento === "CONSULTA_EXPEDIENTE").length;
  const rows = showViews ? historial : historial.filter((h) => h.evento !== "CONSULTA_EXPEDIENTE");

  return (
    <Section
      title="History"
      icon={<History className="size-3.5" aria-hidden />}
      action={
        views > 0 ? (
          <button
            type="button"
            onClick={() => setShowViews((v) => !v)}
            aria-pressed={showViews}
            className="text-xs font-medium text-accent hover:underline focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
          >
            {showViews ? "Hide case-file views" : `Show case-file views (${views})`}
          </button>
        ) : null
      }
    >
      {rows.length === 0 ? (
        <p className="text-xs text-muted">No recorded events yet.</p>
      ) : (
        <ol className="relative space-y-3 border-l border-border pl-4">
          {rows.map((h) => {
            const actor = h.actor ? personLabel(h.actor, me) : ACTOR_TYPE_LABEL[h.actor_tipo];
            const change =
              h.estado_anterior !== null || h.estado_nuevo !== null
                ? `${h.estado_anterior !== null ? stateLabel(h.estado_anterior) : "—"} → ${h.estado_nuevo !== null ? stateLabel(h.estado_nuevo) : "—"}`
                : null;
            return (
              <li key={h.id} className="relative">
                <span
                  className={cx(
                    "absolute top-1.5 -left-[21px] size-2.5 rounded-full border-2 border-surface",
                    STRONG_EVENTS.has(h.evento) ? "bg-accent" : "bg-border-strong",
                  )}
                  aria-hidden
                />
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
                  <p className="text-[13px] font-medium text-ink">{eventLabel(h.evento)}</p>
                  <span className="text-[11px] text-muted">
                    <MxTime at={h.ocurrido_en} />
                  </span>
                </div>
                <p className="text-xs text-ink-2">
                  {actor}
                  {h.actor && h.actor_tipo !== "SISTEMA" ? (
                    <span className="text-muted"> · {ACTOR_TYPE_LABEL[h.actor_tipo]}</span>
                  ) : null}
                  {change ? <span className="text-muted"> · {change}</span> : null}
                </p>
                {h.motivo ? <p className="mt-1 text-xs whitespace-pre-wrap text-ink-2">{h.motivo}</p> : null}
              </li>
            );
          })}
        </ol>
      )}
    </Section>
  );
}
