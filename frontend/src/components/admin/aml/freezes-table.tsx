"use client";

import type { KeyboardEvent } from "react";
import { Snowflake } from "lucide-react";

import { Dialog } from "@/components/admin/aml/dialog";
import { FreezePanel } from "@/components/admin/aml/freeze-panel";
import { MxTime, SlaText, useNow } from "@/components/admin/aml/parts";
import { Money } from "@/components/ui/data";
import { Alert, Badge, Skeleton, cx } from "@/components/ui/primitives";
import type { CongelamientoAml, Rol, Uuid } from "@/lib/api/types";
import type { FreezeListState } from "@/hooks/use-aml-feed";
import { FREEZE_ORIGIN_LABEL, FREEZE_STATE_LABEL, REQUEST_TYPE_LABEL, compareInstants } from "@/lib/aml";
import { isZero } from "@/lib/decimal";

const STATES: [FreezeListState, string][] = [
  ["ACTIVO", "Active"],
  ["LEVANTADO", "Lifted"],
  ["TODOS", "All"],
];

const head = "py-2 pr-3 font-medium whitespace-nowrap";

/**
 * The frozen-clients tab: restrictions of the selected state (active by
 * default), most recent first. A row opens the restriction panel in a drawer.
 */
export function FreezesTable({
  freezes,
  state,
  onStateChange,
  onOpen,
}: {
  freezes: CongelamientoAml[] | null;
  state: FreezeListState;
  onStateChange: (state: FreezeListState) => void;
  onOpen: (id: Uuid) => void;
}) {
  const now = useNow();
  const rows = freezes ? [...freezes].sort((a, b) => compareInstants(b.congelado_en, a.congelado_en) || b.folio - a.folio) : null;
  const onKeyDown = (event: KeyboardEvent<HTMLTableRowElement>, id: Uuid) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onOpen(id);
    }
  };

  return (
    <div>
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3 sm:px-5">
        <div role="group" aria-label="Restriction state" className="inline-flex overflow-hidden rounded-lg border border-border-strong">
          {STATES.map(([value, label]) => (
            <button
              key={value}
              type="button"
              aria-pressed={state === value}
              onClick={() => onStateChange(value)}
              className={cx(
                "h-8 border-r border-border-strong px-3 text-xs font-medium transition last:border-r-0",
                "focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none focus-visible:ring-inset",
                state === value ? "bg-accent-soft text-accent" : "bg-surface text-ink-2 hover:bg-surface-2",
              )}
            >
              {label}
            </button>
          ))}
        </div>
        <p className="text-xs text-muted">Restricted clients cannot send money; incoming funds are held. They are not told why.</p>
      </div>

      {rows === null ? (
        <div className="space-y-2 p-5" aria-busy="true">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} className="h-9 w-full" />
          ))}
        </div>
      ) : rows.length === 0 ? (
        <div className="flex flex-col items-center gap-2 p-10 text-center text-sm text-muted">
          <Snowflake className="size-5" aria-hidden />
          {state === "ACTIVO" ? "No client is restricted right now." : "No restrictions to show."}
        </div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[920px] border-collapse text-left text-[13px]">
            <caption className="sr-only">Restricted clients. Press Enter on a row to open the restriction.</caption>
            <thead>
              <tr className="border-b border-border bg-surface-2/60 text-[11px] tracking-wide text-muted uppercase">
                <th scope="col" className={cx(head, "pl-5")}>Folio</th>
                <th scope="col" className={head}>Client</th>
                <th scope="col" className={head}>Origin</th>
                <th scope="col" className={head}>Since</th>
                <th scope="col" className={head}>Review</th>
                <th scope="col" className={cx(head, "text-right")}>Held</th>
                <th scope="col" className={cx(head, "text-right")}>Blocked attempts</th>
                <th scope="col" className={cx(head, "pr-5")}>Pending request</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((c) => (
                <tr
                  key={c.id}
                  tabIndex={0}
                  onClick={() => onOpen(c.id)}
                  onKeyDown={(e) => onKeyDown(e, c.id)}
                  data-testid="aml-freeze-row"
                  className={cx(
                    "cursor-pointer border-b border-border/70 hover:bg-surface-2/60",
                    "focus-visible:bg-surface-2 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent",
                  )}
                >
                  <td className="py-2 pr-3 pl-5 font-mono text-xs font-semibold text-ink">#{c.folio}</td>
                  <td className="max-w-[240px] py-2 pr-3">
                    <span className="flex items-center gap-1.5">
                      <span className="truncate font-medium text-ink" title={c.cliente.nombre_completo}>
                        {c.cliente.nombre_completo}
                      </span>
                      {c.confirmado ? <Badge tone="danger">Confirmed</Badge> : null}
                    </span>
                  </td>
                  <td className="py-2 pr-3 text-ink-2">{FREEZE_ORIGIN_LABEL[c.origen]}</td>
                  <td className="py-2 pr-3 text-ink-2">
                    <MxTime at={c.congelado_en} relative now={now} />
                  </td>
                  <td className="py-2 pr-3">
                    {c.estado === "ACTIVO" ? <SlaText slaVenceEn={c.sla_vence_en} now={now} /> : <Badge>{FREEZE_STATE_LABEL[c.estado]}</Badge>}
                  </td>
                  <td className="py-2 pr-3 text-right whitespace-nowrap">
                    {isZero(c.retenido_vigente) ? <span className="text-muted">—</span> : <Money value={c.retenido_vigente} />}
                  </td>
                  <td className="py-2 pr-3 text-right tabular-nums">{c.intentos_bloqueados}</td>
                  <td className="py-2 pr-5">
                    {c.solicitud_pendiente ? (
                      <Badge tone="info">
                        #{c.solicitud_pendiente.folio} · {REQUEST_TYPE_LABEL[c.solicitud_pendiente.tipo]}
                      </Badge>
                    ) : (
                      <span className="text-muted">—</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/** A restriction opened from the frozen-clients tab, with a link to the alert that caused it. */
export function FreezeDrawer({
  congelamiento,
  role,
  me,
  token,
  notice,
  onClose,
  onOpenAlert,
  onChanged,
}: {
  congelamiento: CongelamientoAml;
  role: Rol | null;
  me: Uuid;
  token: string;
  notice: string | null;
  onClose: () => void;
  onOpenAlert: (id: Uuid) => void;
  onChanged: (message: string) => void;
}) {
  return (
    <Dialog
      open
      variant="drawer"
      className="max-w-2xl"
      onClose={onClose}
      title={congelamiento.cliente.nombre_completo}
    >
      {notice ? (
        <div className="px-5 pt-4">
          <Alert tone="success">{notice}</Alert>
        </div>
      ) : null}
      <FreezePanel congelamiento={congelamiento} role={role} me={me} token={token} onChanged={onChanged} onOpenAlert={onOpenAlert} />
    </Dialog>
  );
}
