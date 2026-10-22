"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { FileText, ListTree, ScrollText } from "lucide-react";

import { ClientPanel } from "@/components/admin/aml/client-panel";
import { ClientReviewDrawer } from "@/components/admin/aml/client-review";
import { Dialog } from "@/components/admin/aml/dialog";
import { FreezePanel, ManualFreezeDialog } from "@/components/admin/aml/freeze-panel";
import { HistoryTimeline } from "@/components/admin/aml/history-timeline";
import {
  AlertStateBadge,
  DeadlineText,
  Facts,
  LevelBadge,
  MxTime,
  ScoreMeter,
  Section,
  personLabel,
  useNow,
} from "@/components/admin/aml/parts";
import { ResolutionForm } from "@/components/admin/aml/resolution-form";
import { ScoreBreakdown } from "@/components/admin/aml/score-breakdown";
import { TransactionContext } from "@/components/admin/aml/transaction-context";
import { UifFolioForm } from "@/components/admin/aml/uif-folio-form";
import { Alert, Badge, Button, Skeleton } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { AlertaAml, DetalleAlertaAml, Rol, Uuid } from "@/lib/api/types";
import {
  DECISION_LABEL,
  ORIGIN_LABEL,
  REPORT_TYPE_LABEL,
  canAct,
  canRecordUifFolio,
  isOpen,
  type Thresholds,
} from "@/lib/aml";
import { useAuthStore } from "@/store/auth";

/** Header of the case file: everything the feed row says, in full. */
function CaseHeader({ a, thresholds, me, now }: { a: DetalleAlertaAml["alerta"]; thresholds: Thresholds; me: Uuid; now: Date }) {
  const reportDue = a.estado !== "FALSO_POSITIVO" && !a.folio_uif;
  return (
    <Section title="Alert" icon={<FileText className="size-3.5" aria-hidden />}>
      <div className="flex flex-wrap items-center gap-2">
        <LevelBadge level={a.nivel_riesgo} />
        <ScoreMeter score={a.puntaje} level={a.nivel_riesgo} thresholds={thresholds} />
        <AlertStateBadge estado={a.estado} />
        {a.escalada ? <Badge tone="danger">Escalated</Badge> : null}
        {a.urgente ? <Badge tone="danger">Urgent</Badge> : null}
        {a.resolucion_propuesta === "FALSO_POSITIVO" ? <Badge tone="info">False positive proposed</Badge> : null}
        {a.congelamiento?.estado === "ACTIVO" ? <Badge tone="danger">Client restricted</Badge> : null}
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <Facts
          rows={[
            [
              "Rule",
              <span key="r">
                <span className="font-mono text-xs">{a.regla.codigo}</span> · {a.regla.nombre}{" "}
                <span className="text-muted">(version {a.regla.version})</span>
              </span>,
            ],
            ["Decision", DECISION_LABEL[a.decision]],
            ["Report type", REPORT_TYPE_LABEL[a.tipo_reporte]],
            ["Origin", ORIGIN_LABEL[a.origen]],
            ["Detected", <MxTime key="d" at={a.timestamp} />],
            [
              "Occurrences",
              <span key="o">
                {a.ocurrencias}
                {a.ocurrencias > 1 ? (
                  <span className="text-muted">
                    {" "}
                    · last <MxTime at={a.ultima_ocurrencia_en} />
                  </span>
                ) : null}
              </span>,
            ],
          ]}
        />
        <Facts
          rows={[
            [
              "UIF deadline",
              reportDue ? (
                <span key="u">
                  <DeadlineText fechaLimite={a.fecha_limite_reporte} now={now} />{" "}
                  <span className="text-muted">
                    · <MxTime at={a.fecha_limite_reporte} />
                  </span>
                </span>
              ) : a.folio_uif ? (
                "Reported"
              ) : (
                "Not applicable"
              ),
            ],
            [
              "Assignee",
              a.asignada_a ? (
                <span key="a">
                  {personLabel(a.asignada_a, me)}
                  {a.tomada_en ? (
                    <span className="text-muted">
                      {" "}
                      · since <MxTime at={a.tomada_en} />
                    </span>
                  ) : null}
                </span>
              ) : (
                "Unassigned"
              ),
            ],
            ...(a.resuelta_por
              ? ([
                  [
                    "Resolved by",
                    <span key="rb">
                      {personLabel(a.resuelta_por, me)}
                      {a.resuelta_en ? (
                        <span className="text-muted">
                          {" "}
                          · <MxTime at={a.resuelta_en} />
                        </span>
                      ) : null}
                    </span>,
                  ],
                ] as [string, React.ReactNode][])
              : []),
            ...(a.aprobada_por ? ([["Approved by", personLabel(a.aprobada_por, me)]] as [string, React.ReactNode][]) : []),
            ...(a.folio_uif
              ? ([
                  [
                    "UIF folio",
                    <span key="f">
                      <span className="font-mono">{a.folio_uif}</span>
                      {a.reportada_uif_en ? <span className="text-muted"> · filed {a.reportada_uif_en}</span> : null}
                    </span>,
                  ],
                ] as [string, React.ReactNode][])
              : []),
          ]}
        />
      </div>
    </Section>
  );
}

/** The written record: final dictamen, a pending false-positive proposal and the legacy analyst. */
function DictamenRecord({ a, me }: { a: DetalleAlertaAml["alerta"]; me: Uuid }) {
  if (!a.dictamen && !a.dictamen_propuesto && !a.legado_analista) return null;
  return (
    <Section title="Dictamen" icon={<ScrollText className="size-3.5" aria-hidden />}>
      {a.dictamen ? (
        <div className="space-y-1">
          <p className="text-xs text-muted">Recorded by {personLabel(a.resuelta_por, me)}</p>
          <p className="text-[13px] whitespace-pre-wrap text-ink">{a.dictamen}</p>
        </div>
      ) : null}
      {a.dictamen_propuesto ? (
        <div className="space-y-1 rounded-lg border border-info/30 bg-info-soft p-3">
          <p className="text-xs font-semibold text-info">
            False-positive proposal by {personLabel(a.propuesta_por, me)}, awaiting a second compliance officer
          </p>
          <p className="text-[13px] whitespace-pre-wrap text-ink">{a.dictamen_propuesto}</p>
        </div>
      ) : null}
      {a.legado_analista ? <p className="text-xs text-muted">Legacy analyst: {a.legado_analista}</p> : null}
    </Section>
  );
}

function OtherAlerts({ alertas, onOpen }: { alertas: AlertaAml[]; onOpen: (id: Uuid) => void }) {
  return (
    <Section title="Other alerts of this client" icon={<ListTree className="size-3.5" aria-hidden />}>
      {alertas.length === 0 ? (
        <p className="text-xs text-muted">No other alerts.</p>
      ) : (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {alertas.map((o) => (
            <li key={o.id}>
              <button
                type="button"
                onClick={() => onOpen(o.id)}
                className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-left text-xs transition hover:bg-surface-2 focus-visible:bg-surface-2 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
              >
                <span className="font-mono font-semibold text-ink">#{o.folio}</span>
                <span className="font-mono text-[11px] text-ink-2">{o.regla.codigo}</span>
                <LevelBadge level={o.nivel_riesgo} />
                <AlertStateBadge estado={o.estado} />
                <span className="ml-auto text-muted">
                  <MxTime at={o.timestamp} style="date" />
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

/**
 * The case file (`?alerta=<id>`): loads GET /aml/alertas/{id} on open, which
 * the backend logs as a consultation, and again after every action. Composes
 * the score breakdown, transaction context, client and restriction panels,
 * the resolution form, the client's other alerts and the history.
 */
export function AlertDrawer({
  alertId,
  token,
  role,
  me,
  thresholds,
  onClose,
  onOpenAlert,
  onChanged,
}: {
  alertId: Uuid;
  token: string;
  role: Rol | null;
  me: Uuid;
  thresholds: Thresholds;
  onClose: () => void;
  onOpenAlert: (id: Uuid) => void;
  /** After a successful POST: the alerts it returned (the feed updates them at once) and a poll of the summary. */
  onChanged: (alerts: AlertaAml[]) => void;
}) {
  const router = useRouter();
  const now = useNow();
  const [detalle, setDetalle] = useState<DetalleAlertaAml | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(true);
  const [reloadKey, setReloadKey] = useState(0);
  const [notice, setNotice] = useState<string | null>(null);
  const [freezing, setFreezing] = useState(false);
  const [reviewing, setReviewing] = useState(false);
  const [shownId, setShownId] = useState(alertId);

  // Another alert opened from this drawer: start clean.
  if (shownId !== alertId) {
    setShownId(alertId);
    setDetalle(null);
    setError(null);
    setNotice(null);
    setFreezing(false);
    setReviewing(false);
  }

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    api.amlAlert(token, alertId, controller.signal).then(
      (d) => {
        setDetalle(d);
        setError(null);
        setLoading(false);
      },
      (e: unknown) => {
        if (controller.signal.aborted) return;
        if (e instanceof ApiError && e.isUnauthorized) {
          useAuthStore.getState().logout();
          router.replace("/auth/login?expired=1");
          return;
        }
        setError(e);
        setLoading(false);
      },
    );
    return () => controller.abort();
  }, [alertId, token, reloadKey, router]);

  const done = (message: string, alerts: AlertaAml[]) => {
    setNotice(message);
    setReloadKey((k) => k + 1);
    onChanged(alerts);
  };

  const a = detalle?.alerta ?? null;
  const freeze = detalle?.congelamiento ?? null;
  const restricted = freeze?.estado === "ACTIVO";
  const mayFreeze = Boolean(detalle && canAct(role) && detalle.cliente.id !== me && !restricted);
  const notFound = error instanceof ApiError && error.status === 404;

  return (
    <Dialog
      open
      variant="drawer"
      onClose={onClose}
      title={a ? `Alert #${a.folio}` : "Alert"}
      description={
        a ? (
          <span>
            <span className="font-mono text-xs">{a.regla.codigo}</span> · {a.regla.nombre} · {a.cliente.nombre_completo}
          </span>
        ) : undefined
      }
    >
      <div aria-busy={loading || undefined}>
        {notice ? (
          <div className="px-5 pt-4">
            <Alert tone="success">{notice}</Alert>
          </div>
        ) : null}
        {error ? (
          <div className="px-5 pt-4">
            <Alert tone={detalle ? "warning" : "error"} title={notFound ? "Alert not found" : detalle ? "Showing the last data received" : "Could not load the case file"}>
              {notFound ? "This alert does not exist or is no longer available." : describeError(error)}{" "}
              {notFound ? null : (
                <Button variant="ghost" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
                  Retry
                </Button>
              )}
            </Alert>
          </div>
        ) : null}

        {!detalle && loading ? (
          <div className="space-y-3 p-5">
            <Skeleton className="h-6 w-1/3" />
            <Skeleton className="h-24 w-full" />
            <Skeleton className="h-40 w-full" />
            <Skeleton className="h-32 w-full" />
          </div>
        ) : null}

        {detalle && a ? (
          <>
            <CaseHeader a={a} thresholds={thresholds} me={me} now={now} />
            <DictamenRecord a={a} me={me} />
            <ScoreBreakdown alerta={a} evaluaciones={detalle.evaluaciones} thresholds={thresholds} />
            <TransactionContext detalle={detalle} />
            <ClientPanel
              cliente={detalle.cliente}
              cuentas={detalle.cuentas}
              onFreezeClient={mayFreeze ? () => setFreezing(true) : undefined}
              onOpenReview={() => setReviewing(true)}
            />
            {freeze ? (
              <FreezePanel congelamiento={freeze} role={role} me={me} token={token} onChanged={(message) => done(message, [])} />
            ) : null}
            <ResolutionForm
              key={a.id}
              alerta={a}
              role={role}
              me={me}
              token={token}
              restricted={restricted}
              pendingRequest={restricted ? (freeze?.solicitud_pendiente ?? null) : null}
              onDone={(message, result) => done(message, [result.alerta])}
            />
            {canRecordUifFolio(role, me, a) ? (
              <UifFolioForm key={`uif:${a.id}`} alerta={a} token={token} onDone={(message, result) => done(message, [result.alerta])} />
            ) : null}
            <OtherAlerts alertas={detalle.otras_alertas} onOpen={onOpenAlert} />
            <HistoryTimeline historial={detalle.historial} me={me} />
          </>
        ) : null}
      </div>

      {reviewing && detalle ? (
        <ClientReviewDrawer
          clientId={detalle.cliente.id}
          token={token}
          role={role}
          me={me}
          onClose={() => setReviewing(false)}
          onOpenAlert={(id) => {
            setReviewing(false);
            if (id !== alertId) onOpenAlert(id);
          }}
          onChanged={(message) => done(message, [])}
        />
      ) : null}

      {freezing && detalle ? (
        <ManualFreezeDialog
          token={token}
          cliente={{ id: detalle.cliente.id, nombre_completo: detalle.cliente.nombre_completo }}
          alerta={a && isOpen(a) ? { id: a.id, folio: a.folio } : null}
          onClose={() => setFreezing(false)}
          onDone={(message, alerts) => {
            setFreezing(false);
            done(message, alerts);
          }}
        />
      ) : null}
    </Dialog>
  );
}
