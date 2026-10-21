"use client";

import { ArrowDownLeft, ArrowLeftRight, ArrowUpRight } from "lucide-react";

import { Facts, MxTime, Section } from "@/components/admin/aml/parts";
import { Money } from "@/components/ui/data";
import { Badge, cx } from "@/components/ui/primitives";
import type { DetalleAlertaAml, MovimientoAml } from "@/lib/api/types";
import {
  CHANNEL_LABEL,
  DIRECTION_LABEL,
  ENTITY_LABEL,
  describeCounterpartyKey,
  describeDetails,
  type AlertDetails,
} from "@/lib/aml";

function OperationFacts({ details }: { details: AlertDetails }) {
  switch (details.kind) {
    case "operacion": {
      const c = details.contraparte;
      return (
        <Facts
          rows={[
            ["Channel", details.canal ? CHANNEL_LABEL[details.canal] : null],
            ["Direction", details.sentido ? DIRECTION_LABEL[details.sentido] : null],
            ["Entity", details.entidad ? ENTITY_LABEL[details.entidad] : null],
            ["Amount", details.monto ? <Money key="m" value={details.monto} /> : null],
            ["Counterparty", c?.nombre ?? null],
            ["Counterparty account", c?.cuenta ? <span key="c" className="font-mono">{c.cuenta}</span> : null],
            ["Counterparty RFC/CURP", c?.rfc_curp ? <span key="r" className="font-mono">{c.rfc_curp}</span> : null],
            ["Counterparty client id", c?.cliente_id ? <span key="i" className="font-mono text-xs">{c.cliente_id}</span> : null],
            [
              "Ledger movement",
              details.transaccion_id ? (
                <span key="t" className="font-mono text-xs">
                  {details.transaccion_id}
                </span>
              ) : (
                <span key="t" className="text-muted">
                  None (the operation was refused before any ledger write)
                </span>
              ),
            ],
          ]}
        />
      );
    }
    case "motivo":
      return <Facts rows={[["Reason recorded", <span key="m" className="whitespace-pre-wrap">{details.motivo}</span>]]} />;
    case "legado":
      return (
        <Facts
          rows={[
            ["Legacy folio", details.folio !== null ? `#${details.folio}` : null],
            ["Legacy rule", details.regla],
            ["Description", details.descripcion],
            ["Detail", details.detalle ? <span key="d" className="font-mono text-xs break-all">{details.detalle}</span> : null],
            ["Analyst", details.analista],
            ["Legacy dictamen", details.dictamen ? <span key="x" className="whitespace-pre-wrap">{details.dictamen}</span> : null],
          ]}
        />
      );
    case "red":
      return (
        <Facts
          rows={[
            ["Shared counterparty", details.clave ? <span key="k" className="font-mono">{details.clave}</span> : null],
            ["Clients in the network", String(details.clientes.length)],
          ]}
        />
      );
    case "ninguno":
      return <p className="text-sm text-muted">No operation details were recorded with this alert.</p>;
  }
}

function DirectionIcon({ m }: { m: MovimientoAml }) {
  if (m.canal === "TRANSFERENCIA_ENVIADA" || m.canal === "TRANSFERENCIA_RECIBIDA") {
    return <ArrowLeftRight className={cx("size-3.5", m.sentido === "ENTRADA" ? "text-verified" : "text-ink-2")} aria-hidden />;
  }
  return m.sentido === "ENTRADA" ? (
    <ArrowDownLeft className="size-3.5 text-verified" aria-hidden />
  ) : (
    <ArrowUpRight className="size-3.5 text-ink-2" aria-hidden />
  );
}

/** The operation that raised the alert and the client's movements in its pattern. */
export function TransactionContext({ detalle }: { detalle: DetalleAlertaAml }) {
  const details = describeDetails(detalle.alerta.detalles);
  const { movimientos } = detalle;

  return (
    <Section title="Transaction context" icon={<ArrowLeftRight className="size-3.5" aria-hidden />}>
      <OperationFacts details={details} />
      <div className="space-y-2">
        <p className="text-xs font-medium text-ink-2">Movements in the pattern</p>
        {movimientos.length === 0 ? (
          <p className="text-xs text-muted">No ledger movements are linked to this alert.</p>
        ) : (
          <div className="overflow-x-auto rounded-lg border border-border">
            <table className="w-full min-w-[640px] text-left text-xs">
              <thead>
                <tr className="border-b border-border bg-surface-2/60 text-[11px] text-muted">
                  <th scope="col" className="py-1.5 pr-3 pl-3 font-medium">Time</th>
                  <th scope="col" className="py-1.5 pr-3 font-medium">Channel</th>
                  <th scope="col" className="py-1.5 pr-3 font-medium">Direction</th>
                  <th scope="col" className="py-1.5 pr-3 text-right font-medium">Amount</th>
                  <th scope="col" className="py-1.5 pr-3 font-medium">Counterparty</th>
                  <th scope="col" className="py-1.5 pr-3 font-medium">
                    <span className="sr-only">Held</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {movimientos.map((m) => {
                  const counterparty = describeCounterpartyKey(m.contraparte);
                  return (
                    <tr key={`${m.entidad}:${m.transaccion_id}:${m.sentido}`} className="border-b border-border/60 last:border-b-0">
                      <td className="py-1.5 pr-3 pl-3 text-ink-2">
                        <MxTime at={m.creado_en} />
                      </td>
                      <td className="py-1.5 pr-3">{CHANNEL_LABEL[m.canal]}</td>
                      <td className="py-1.5 pr-3">
                        <span className="inline-flex items-center gap-1">
                          <DirectionIcon m={m} />
                          {DIRECTION_LABEL[m.sentido]}
                        </span>
                      </td>
                      <td className={cx("py-1.5 pr-3 text-right font-semibold", m.sentido === "ENTRADA" && "text-verified")}>
                        <Money value={m.monto} sign={m.sentido === "ENTRADA" ? "+" : "-"} />
                      </td>
                      <td className="max-w-[220px] truncate py-1.5 pr-3 font-mono text-[11px]" title={m.contraparte ?? undefined}>
                        {counterparty ? (
                          <>
                            <span className="font-sans text-muted">{counterparty.label}</span> {counterparty.value}
                          </>
                        ) : (
                          <span className="text-muted">—</span>
                        )}
                      </td>
                      <td className="py-1.5 pr-3">{m.retenido ? <Badge tone="warning">Held</Badge> : null}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </Section>
  );
}
