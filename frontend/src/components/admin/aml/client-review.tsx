"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { BarChart3, Landmark, ListTree, Send, Snowflake } from "lucide-react";

import { ClientPanel } from "@/components/admin/aml/client-panel";
import { Dialog } from "@/components/admin/aml/dialog";
import { FreezePanel } from "@/components/admin/aml/freeze-panel";
import { AlertStateBadge, LevelBadge, MxTime, Section } from "@/components/admin/aml/parts";
import { MovementsTable } from "@/components/admin/aml/transaction-context";
import { Money, formatDate } from "@/components/ui/data";
import { Alert, Badge, Button, Skeleton } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { AgregadoAml, RevisionClienteAml, Rol, Uuid } from "@/lib/api/types";
import { FREEZE_ORIGIN_LABEL, FREEZE_STATE_LABEL } from "@/lib/aml";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";

const CREDIT_STATE_LABEL: Record<RevisionClienteAml["creditos"][number]["estado"], string> = {
  EN_FONDEO: "Funding",
  VIGENTE: "Current",
  VENCIDO: "Past due",
  LIQUIDADO: "Paid off",
  CASTIGADO: "Written off",
};

function Aggregates({ agregados }: { agregados: RevisionClienteAml["agregados"] }) {
  const rows: [string, AgregadoAml][] = [
    ["Last 24 hours", agregados.h24],
    ["Last 7 days", agregados.d7],
    ["Last 30 days", agregados.d30],
  ];
  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full min-w-[480px] text-left text-xs">
        <caption className="sr-only">Incoming and outgoing movements by window</caption>
        <thead>
          <tr className="border-b border-border bg-surface-2/60 text-[11px] text-muted">
            <th scope="col" className="py-1.5 pr-3 pl-3 font-medium">Window</th>
            <th scope="col" className="py-1.5 pr-3 text-right font-medium">Incoming</th>
            <th scope="col" className="py-1.5 pr-3 text-right font-medium">Outgoing</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(([label, a]) => (
            <tr key={label} className="border-b border-border/60 last:border-b-0">
              <th scope="row" className="py-1.5 pr-3 pl-3 font-medium text-ink-2">
                {label}
              </th>
              <td className="py-1.5 pr-3 text-right">
                <span className="text-muted">{a.entradas_n} ·</span> <Money value={a.entradas_monto} />
              </td>
              <td className="py-1.5 pr-3 text-right">
                <span className="text-muted">{a.salidas_n} ·</span> <Money value={a.salidas_monto} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * The client review (GET /aml/clientes/{id}, logged as a consultation): KYC
 * profile and accounts, 24 h / 7 d / 30 d aggregates, the last 30 days of
 * movements, pending SPEI orders, fixed-term contracts, credits, every alert
 * and every restriction of the client. Read-only except the restriction panel.
 */
export function ClientReviewDrawer({
  clientId,
  token,
  role,
  me,
  onClose,
  onOpenAlert,
  onChanged,
}: {
  clientId: Uuid;
  token: string;
  role: Rol | null;
  me: Uuid;
  onClose: () => void;
  onOpenAlert: (id: Uuid) => void;
  /** After a restriction request was created from here. */
  onChanged: (message: string) => void;
}) {
  const router = useRouter();
  const [review, setReview] = useState<RevisionClienteAml | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    api.amlClient(token, clientId, controller.signal).then(
      (r) => {
        setReview(r);
        setError(null);
      },
      (e: unknown) => {
        if (controller.signal.aborted) return;
        if (e instanceof ApiError && e.isUnauthorized) {
          useAuthStore.getState().expire();
          router.replace(EXPIRED_LOGIN_PATH);
          return;
        }
        setError(e);
      },
    );
    return () => controller.abort();
  }, [clientId, token, reloadKey, router]);

  // The active restriction and lifted ones that still hold kept funds get the full panel (with its actions).
  const withPanel = review?.congelamientos.filter((c) => c.estado === "ACTIVO" || c.retenciones.some((r) => r.estado === "VIGENTE")) ?? [];
  const past = review?.congelamientos.filter((c) => !withPanel.includes(c)) ?? [];

  return (
    <Dialog
      open
      variant="drawer"
      onClose={onClose}
      title={review ? `Client review: ${review.cliente.nombre_completo}` : "Client review"}
      description="Viewing this review is recorded in the audit trail."
    >
      {notice ? (
        <div className="px-5 pt-4">
          <Alert tone="success">{notice}</Alert>
        </div>
      ) : null}
      {error ? (
        <div className="px-5 pt-4">
          <Alert tone={review ? "warning" : "error"} title={review ? "Showing the last data received" : "Could not load the client review"}>
            {error instanceof ApiError && error.status === 404 ? "This client does not exist." : describeError(error)}{" "}
            <Button variant="ghost" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
              Retry
            </Button>
          </Alert>
        </div>
      ) : null}
      {!review && !error ? (
        <div className="space-y-3 p-5" aria-busy="true">
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-32 w-full" />
          <Skeleton className="h-48 w-full" />
        </div>
      ) : null}

      {review ? (
        <>
          <ClientPanel cliente={review.cliente} cuentas={review.cuentas} />

          <Section title="Activity" icon={<BarChart3 className="size-3.5" aria-hidden />}>
            <Aggregates agregados={review.agregados} />
            <p className="text-xs text-muted">
              Relevant operations in the register: <span className="font-semibold text-ink">{review.relevantes_n}</span>
            </p>
            {review.movimientos.length === 0 ? (
              <p className="text-xs text-muted">No movements in the last 30 days.</p>
            ) : (
              <MovementsTable movimientos={review.movimientos} caption="Movements of the last 30 days, newest first" />
            )}
          </Section>

          <Section title="Pending SPEI orders" icon={<Send className="size-3.5" aria-hidden />}>
            {review.ordenes_spei_pendientes.length === 0 ? (
              <p className="text-xs text-muted">No pending orders.</p>
            ) : (
              <ul className="divide-y divide-border rounded-lg border border-border text-xs">
                {review.ordenes_spei_pendientes.map((o) => (
                  <li key={o.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
                    <Money value={o.monto} className="font-semibold" />
                    <span className="text-ink-2">{o.beneficiario_nombre}</span>
                    <span className="font-mono text-[11px] text-muted">{o.beneficiario_clabe}</span>
                    <Badge tone={o.estado === "PENDIENTE" ? "warning" : "info"}>{o.estado === "PENDIENTE" ? "Pending" : "Sent"}</Badge>
                    <span className="ml-auto text-muted">
                      {o.intentos_envio} {o.intentos_envio === 1 ? "attempt" : "attempts"} · <MxTime at={o.creado_en} />
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </Section>

          <Section title="Fixed term and credit" icon={<Landmark className="size-3.5" aria-hidden />}>
            {review.mutuos_vigentes.length === 0 && review.creditos.length === 0 ? (
              <p className="text-xs text-muted">No fixed-term contracts or credits.</p>
            ) : (
              <ul className="divide-y divide-border rounded-lg border border-border text-xs">
                {review.mutuos_vigentes.map((m) => (
                  <li key={m.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
                    <span className="font-medium text-ink">Fixed-term contract #{m.folio}</span>
                    <Money value={m.principal} />
                    <span className="ml-auto text-muted">matures {formatDate(m.fecha_vencimiento)}</span>
                  </li>
                ))}
                {review.creditos.map((c) => (
                  <li key={c.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
                    <span className="font-medium text-ink">Credit #{c.folio}</span>
                    <Badge>{CREDIT_STATE_LABEL[c.estado]}</Badge>
                    <span>
                      <Money value={c.saldo_insoluto} /> <span className="text-muted">of</span> <Money value={c.principal} />
                    </span>
                    <span className="ml-auto text-muted">due {formatDate(c.fecha_vencimiento)}</span>
                  </li>
                ))}
              </ul>
            )}
          </Section>

          <Section title="Alerts" icon={<ListTree className="size-3.5" aria-hidden />}>
            {review.alertas.length === 0 ? (
              <p className="text-xs text-muted">No alerts.</p>
            ) : (
              <ul className="divide-y divide-border rounded-lg border border-border">
                {review.alertas.map((a) => (
                  <li key={a.id}>
                    <button
                      type="button"
                      onClick={() => onOpenAlert(a.id)}
                      className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-left text-xs transition hover:bg-surface-2 focus-visible:bg-surface-2 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
                    >
                      <span className="font-mono font-semibold text-ink">#{a.folio}</span>
                      <span className="font-mono text-[11px] text-ink-2">{a.regla.codigo}</span>
                      <LevelBadge level={a.nivel_riesgo} />
                      <AlertStateBadge estado={a.estado} />
                      <span className="ml-auto text-muted">
                        <MxTime at={a.timestamp} style="date" />
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </Section>

          {withPanel.map((c) => (
            <FreezePanel
              key={c.id}
              congelamiento={c}
              role={role}
              me={me}
              token={token}
              onOpenAlert={onOpenAlert}
              onChanged={(message) => {
                setNotice(message);
                setReloadKey((k) => k + 1);
                onChanged(message);
              }}
            />
          ))}
          <Section title={withPanel.length > 0 ? "Earlier restrictions" : "Restrictions"} icon={<Snowflake className="size-3.5" aria-hidden />}>
            {past.length === 0 ? (
              <p className="text-xs text-muted">
                {withPanel.length > 0 ? "No earlier restrictions." : "This client has never been restricted."}
              </p>
            ) : (
              <ul className="divide-y divide-border rounded-lg border border-border text-xs">
                {past.map((c) => (
                  <li key={c.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
                    <span className="font-mono font-semibold text-ink">#{c.folio}</span>
                    <Badge>{FREEZE_STATE_LABEL[c.estado]}</Badge>
                    <span className="text-ink-2">{FREEZE_ORIGIN_LABEL[c.origen]}</span>
                    <span className="text-muted">
                      <MxTime at={c.congelado_en} style="date" />
                      {c.levantado_en ? (
                        <>
                          {" "}
                          → <MxTime at={c.levantado_en} style="date" />
                        </>
                      ) : null}
                    </span>
                    <span className="ml-auto">
                      held <Money value={c.retenido_vigente} />
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </Section>
        </>
      ) : null}
    </Dialog>
  );
}
