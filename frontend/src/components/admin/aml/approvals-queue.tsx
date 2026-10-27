"use client";

import { useState } from "react";
import { Check, Users, X } from "lucide-react";

import { Dialog } from "@/components/admin/aml/dialog";
import { MxTime, NarrativeField, SlaText, personLabel, useNow } from "@/components/admin/aml/parts";
import { useSubmit } from "@/components/admin/aml/use-submit";
import { Money } from "@/components/ui/data";
import { Alert, Badge, Button, Skeleton } from "@/components/ui/primitives";
import { api } from "@/lib/api/endpoints";
import type { AlertaAml, CongelamientoAml, DecisionLevantamientoAml, Rol, SolicitudAml, TipoSolicitudAml, Uuid } from "@/lib/api/types";
import { addUnits, formatMoney } from "@/lib/decimal";
import {
  COMENTARIO_MAX,
  COMENTARIO_MIN,
  LEVEL_LABEL,
  REQUEST_TYPE_LABEL,
  canAct,
  canDecideRequest,
  compareInstants,
  isOpen,
  validateNarrative,
} from "@/lib/aml";

/** What approving each request type does, in one sentence. */
const APPROVE_EFFECT: Record<TipoSolicitudAml, string> = {
  LEVANTAR_Y_LIBERAR: "This lifts the restriction on the client's account and releases the held funds.",
  LEVANTAR_CONSERVANDO_RETENCIONES:
    "This lifts the restriction on the client's account; the held funds stay held until a separate approved request releases them.",
  LIBERAR_RETENCIONES: "This releases the funds still held for the client to their available balance.",
};
const REJECT_EFFECT = "This rejects the request: the restriction and the held funds stay as they are, and any false-positive proposals on this client are withdrawn.";

function decisionMessage(r: DecisionLevantamientoAml): string {
  const folio = `#${r.solicitud.folio}`;
  if (r.idempotente) return `The decision on request ${folio} was already recorded.`;
  if (r.solicitud.estado === "RECHAZADA") return `Request ${folio} rejected.`;
  const parts = [`Request ${folio} approved.`];
  if (r.congelamiento.estado === "LEVANTADO" && r.solicitud.tipo !== "LIBERAR_RETENCIONES") parts.push("The restriction was lifted.");
  if (r.liberaciones.length > 0) {
    const total = r.liberaciones.reduce((sum, l) => addUnits(sum, l.monto), "0");
    parts.push(`${formatMoney(total)} MXN released.`);
  }
  if (r.alertas_resueltas.length > 0) {
    parts.push(`${r.alertas_resueltas.length} ${r.alertas_resueltas.length === 1 ? "alert" : "alerts"} closed as false positive.`);
  }
  return parts.join(" ");
}

/** Approve or reject a four-eyes request; a comment of 10–2000 characters is required either way. */
function DecisionDialog({
  solicitud,
  aprobar,
  token,
  onClose,
  onDone,
  onStale,
}: {
  solicitud: SolicitudAml;
  aprobar: boolean;
  token: string;
  onClose: () => void;
  onDone: (message: string, result: DecisionLevantamientoAml) => void;
  /** The request was decided or withdrawn meanwhile (409): the queue refreshes under the dialog. */
  onStale?: (message: string) => void;
}) {
  const [comentario, setComentario] = useState("");
  const [touched, setTouched] = useState(false);
  const { busy, error, run } = useSubmit({ onStale });
  const problem = validateNarrative(comentario, COMENTARIO_MIN, COMENTARIO_MAX);

  const submit = async () => {
    setTouched(true);
    if (problem) return;
    const result = await run((clave_idempotencia) =>
      api.amlDecideRequest(token, solicitud.id, { aprobar, comentario: comentario.trim(), clave_idempotencia }),
    );
    if (result) onDone(decisionMessage(result), result);
  };

  return (
    <Dialog
      open
      onClose={onClose}
      busy={busy}
      title={`${aprobar ? "Approve" : "Reject"} request #${solicitud.folio}`}
      description={aprobar ? APPROVE_EFFECT[solicitud.tipo] : REJECT_EFFECT}
      footer={
        <>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant={aprobar ? "primary" : "danger"} onClick={() => void submit()} loading={busy}>
            {aprobar ? <Check className="size-4" aria-hidden /> : <X className="size-4" aria-hidden />}
            {aprobar ? "Approve" : "Reject"}
          </Button>
        </>
      }
    >
      <form
        className="space-y-4 px-5 py-4"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className="space-y-1 rounded-lg bg-surface-2 p-3 text-xs">
          <p className="text-ink">
            <span className="font-semibold">{solicitud.cliente.nombre_completo}</span> · {REQUEST_TYPE_LABEL[solicitud.tipo]}
          </p>
          <p className="whitespace-pre-wrap text-ink-2">{solicitud.justificacion}</p>
        </div>
        <NarrativeField
          id={`decision-comment-${solicitud.id}`}
          label="Comment"
          value={comentario}
          onChange={setComentario}
          min={COMENTARIO_MIN}
          max={COMENTARIO_MAX}
          error={touched ? problem : null}
          hint="Why you agree or disagree. It is part of the permanent case record."
          disabled={busy}
        />
        {error ? <Alert tone="error">{error}</Alert> : null}
      </form>
    </Dialog>
  );
}

/**
 * The four-eyes queue: pending unfreeze and release requests, oldest first.
 * The requester, the restricted client and any officer holding a
 * false-positive proposal on that client cannot decide (`canDecideRequest`,
 * mirroring aml.decidir_levantamiento); auditors see the queue read-only.
 */
export function ApprovalsQueue({
  requests,
  freezes,
  alerts,
  role,
  me,
  token,
  onOpenAlert,
  onChanged,
  onStale,
}: {
  requests: SolicitudAml[] | null;
  /** ACTIVO restrictions, for the review deadline of each request. */
  freezes: CongelamientoAml[] | null;
  /** Loaded alerts (four-eyes needs the false-positive proposers of each client). */
  alerts: AlertaAml[];
  role: Rol | null;
  me: Uuid;
  token: string;
  onOpenAlert: (id: Uuid) => void;
  onChanged: (message: string, alerts: AlertaAml[]) => void;
  /** A decision found the request already decided or withdrawn (409): refresh the queue. */
  onStale?: () => void;
}) {
  const now = useNow();
  const [deciding, setDeciding] = useState<{ solicitud: SolicitudAml; aprobar: boolean } | null>(null);
  const officer = canAct(role);

  if (requests === null) {
    return (
      <div className="space-y-2 p-5" aria-busy="true">
        {Array.from({ length: 3 }, (_, i) => (
          <Skeleton key={i} className="h-24 w-full" />
        ))}
      </div>
    );
  }

  const pending = requests
    .filter((s) => s.estado === "PENDIENTE")
    .sort((x, y) => compareInstants(x.solicitada_en, y.solicitada_en) || x.folio - y.folio);
  if (pending.length === 0) {
    return (
      <div className="flex flex-col items-center gap-2 p-10 text-center text-sm text-muted">
        <Users className="size-5" aria-hidden />
        No requests are waiting for a second approval.
      </div>
    );
  }

  return (
    <div>
      <ul className="divide-y divide-border">
        {pending.map((s) => {
          const freeze = freezes?.find((f) => f.id === s.congelamiento_id) ?? null;
          const check = canDecideRequest(role, me, s, alerts);
          const clientAlerts = alerts.filter((a) => a.cliente.id === s.cliente.id);
          const proposals = clientAlerts.filter((a) => a.resolucion_propuesta === "FALSO_POSITIVO");
          // Approving a lift is refused while an open high-risk alert of the client has no proposal.
          const blocking =
            s.tipo === "LIBERAR_RETENCIONES"
              ? []
              : clientAlerts.filter(
                  (a) => isOpen(a) && (a.nivel_riesgo === "ALTO" || a.nivel_riesgo === "CRITICO") && a.resolucion_propuesta === null,
                );
          return (
            <li key={s.id} className="space-y-3 px-4 py-4 sm:px-5" data-testid="aml-request">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div className="min-w-0 space-y-1">
                  <p className="flex flex-wrap items-center gap-2 text-sm">
                    <span className="font-mono text-xs font-semibold text-ink">#{s.folio}</span>
                    <span className="font-semibold text-ink">{s.cliente.nombre_completo}</span>
                    <Badge tone={s.tipo === "LEVANTAR_Y_LIBERAR" ? "warning" : "info"}>{REQUEST_TYPE_LABEL[s.tipo]}</Badge>
                  </p>
                  <p className="text-xs text-muted">
                    Requested by {personLabel(s.solicitada_por, me)} · <MxTime at={s.solicitada_en} relative now={now} />
                    {freeze ? (
                      <>
                        {" "}
                        · restriction #{freeze.folio} review <SlaText slaVenceEn={freeze.sla_vence_en} now={now} />
                        {" "}· held <Money value={freeze.retenido_vigente} />
                      </>
                    ) : null}
                  </p>
                </div>
                {officer ? (
                  <div className="flex shrink-0 flex-col items-end gap-1">
                    <div className="flex gap-2">
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={!check.allowed}
                        onClick={() => setDeciding({ solicitud: s, aprobar: false })}
                      >
                        <X className="size-3.5" aria-hidden />
                        Reject
                      </Button>
                      <Button size="sm" disabled={!check.allowed} onClick={() => setDeciding({ solicitud: s, aprobar: true })}>
                        <Check className="size-3.5" aria-hidden />
                        Approve
                      </Button>
                    </div>
                    {check.reason ? <p className="text-[11px] text-muted">{check.reason}</p> : null}
                  </div>
                ) : null}
              </div>
              <p className="rounded-lg bg-surface-2 px-3 py-2 text-[13px] whitespace-pre-wrap text-ink">{s.justificacion}</p>
              {proposals.length > 0 ? (
                <p className="text-xs text-ink-2">
                  False positive proposed on{" "}
                  {proposals.map((a, i) => (
                    <span key={a.id}>
                      {i > 0 ? ", " : ""}
                      <button type="button" className="font-medium text-accent hover:underline" onClick={() => onOpenAlert(a.id)}>
                        alert #{a.folio}
                      </button>{" "}
                      by {personLabel(a.propuesta_por, me)}
                    </span>
                  ))}
                  .
                </p>
              ) : null}
              {blocking.length > 0 ? (
                <Alert tone="warning">
                  Approval will be refused until these open alerts are resolved or proposed as false positives:{" "}
                  {blocking.map((a, i) => (
                    <span key={a.id}>
                      {i > 0 ? ", " : ""}
                      <button type="button" className="font-semibold underline" onClick={() => onOpenAlert(a.id)}>
                        #{a.folio} ({LEVEL_LABEL[a.nivel_riesgo]})
                      </button>
                    </span>
                  ))}
                  .
                </Alert>
              ) : null}
            </li>
          );
        })}
      </ul>

      {deciding ? (
        <DecisionDialog
          key={`${deciding.solicitud.id}:${deciding.aprobar}`}
          solicitud={deciding.solicitud}
          aprobar={deciding.aprobar}
          token={token}
          onClose={() => setDeciding(null)}
          onDone={(message, result) => {
            setDeciding(null);
            onChanged(message, result.alertas_resueltas);
          }}
          onStale={onStale ? () => onStale() : undefined}
        />
      ) : null}
    </div>
  );
}
