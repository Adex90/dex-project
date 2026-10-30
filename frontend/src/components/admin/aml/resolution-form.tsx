"use client";

import { useState } from "react";
import { ClipboardCheck, Hand, Undo2 } from "lucide-react";

import { Dialog } from "@/components/admin/aml/dialog";
import { Checkbox, Choice, NarrativeField, Section, personLabel } from "@/components/admin/aml/parts";
import { useSubmit } from "@/components/admin/aml/use-submit";
import { Alert, Button } from "@/components/ui/primitives";
import { api } from "@/lib/api/endpoints";
import type {
  AccionResolucionAml,
  AlertaAml,
  ResolucionAlertaAml,
  ResolverAlertaInput,
  Rol,
  SolicitudAml,
  TipoReporteAml,
  Uuid,
} from "@/lib/api/types";
import {
  DICTAMEN_MAX,
  DICTAMEN_MIN,
  REPORT_TYPE_LABEL,
  REQUEST_TYPE_LABEL,
  allowedActions,
  canAct,
  formatMexicoCity,
  validateNarrative,
  validateResolution,
} from "@/lib/aml";

type Decision = "FALSO_POSITIVO" | "CONFIRMAR" | "ESCALAR";
type Lift = "LEVANTAR_Y_LIBERAR" | "LEVANTAR_CONSERVANDO_RETENCIONES";

const FOUR_EYES_NOTE = "Lifting the restriction requires approval by a second compliance officer.";

const DECISION_COPY: Record<Decision, { label: string; hint: string }> = {
  FALSO_POSITIVO: {
    label: "Clear as false positive",
    hint: "The activity has a legitimate explanation. The alert is closed and this pattern stops alerting up to now.",
  },
  CONFIRMAR: {
    label: "Confirm suspicious activity (UIF report)",
    hint: "Closes the alert as confirmed, raises the client to high risk and starts the UIF report deadline.",
  },
  ESCALAR: {
    label: "Escalate",
    hint: "Raises the alert to critical for senior review. It stays open and assigned to you.",
  },
};

/** The one-sentence effect stated by the confirmation dialog. */
function confirmationText(input: ResolverAlertaInput, a: AlertaAml, restricted: boolean, pending: SolicitudAml | null): string[] {
  const lines: string[] = [];
  switch (input.accion) {
    case "CONFIRMAR":
      lines.push(
        input.urgente
          ? "This confirms the suspicious activity: the alert is closed, the client is raised to high risk and the UIF report is due within 24 hours."
          : `This confirms the suspicious activity: the alert is closed, the client is raised to high risk and the UIF report is due by ${formatMexicoCity(a.fecha_limite_reporte, "date")}.`,
      );
      if (restricted) lines.push("The restriction stays in place and is marked as confirmed; any pending unfreeze request is withdrawn.");
      break;
    case "FALSO_POSITIVO":
      if (!restricted) {
        lines.push("This closes the alert as a false positive. A closed alert cannot be reopened.");
      } else if (pending) {
        lines.push(`This proposes clearing the alert as a false positive and adds it to pending request #${pending.folio}.`);
        lines.push(FOUR_EYES_NOTE);
      } else {
        lines.push(
          input.tipo_levantamiento === "LEVANTAR_CONSERVANDO_RETENCIONES"
            ? "This proposes clearing the alert as a false positive and requests lifting the restriction while keeping the funds held."
            : "This proposes clearing the alert as a false positive and requests lifting the restriction and releasing the held funds.",
        );
        lines.push(`${FOUR_EYES_NOTE} Until then the alert stays in review and the client stays restricted.`);
      }
      break;
    case "ESCALAR":
      lines.push("This escalates the alert to critical.");
      break;
    default:
      break;
  }
  if (input.congelar) {
    lines.push(
      "The client's account is restricted now: outgoing operations are refused and incoming funds are held. The client is not told why.",
    );
  }
  return lines;
}

function successMessage(r: ResolucionAlertaAml, accion: AccionResolucionAml): string {
  const folio = `#${r.alerta.folio}`;
  if (r.idempotente) return `This action on alert ${folio} was already recorded.`;
  const restricted = r.congelamiento && r.congelamiento.estado === "ACTIVO" ? ` The client is restricted (restriction #${r.congelamiento.folio}).` : "";
  switch (accion) {
    case "TOMAR":
      return `Alert ${folio} is assigned to you.`;
    case "LIBERAR":
      return `Alert ${folio} is back in the pending queue.`;
    case "ESCALAR":
      return `Alert ${folio} escalated to critical.${restricted}`;
    case "CONFIRMAR":
      return `Suspicious activity confirmed on alert ${folio}. Record the UIF folio once the report is filed.${restricted}`;
    case "FALSO_POSITIVO":
      if (r.alerta.estado === "FALSO_POSITIVO") return `Alert ${folio} cleared as a false positive.`;
      return r.solicitud
        ? `False positive proposed on alert ${folio}. Request #${r.solicitud.folio} awaits a second compliance officer.`
        : `False positive proposed on alert ${folio}.`;
  }
}

/**
 * Take / Release / Escalate / Confirm suspicious activity / Clear as false
 * positive (CUMPLIMIENTO only). The actions offered come from
 * `allowedActions`, which mirrors the database rules; the database decides.
 * Confirming, clearing and every restriction ask for confirmation first.
 */
export function ResolutionForm({
  alerta,
  role,
  me,
  token,
  restricted,
  pendingRequest,
  onDone,
  onStale,
}: {
  alerta: AlertaAml;
  role: Rol | null;
  me: Uuid;
  token: string;
  /** The client has an ACTIVO restriction (the case file's `congelamiento`). */
  restricted: boolean;
  /** Its pending four-eyes request, if any (a false-positive proposal joins it). */
  pendingRequest: SolicitudAml | null;
  onDone: (message: string, result: ResolucionAlertaAml) => void;
  /** The API refused because the alert changed (409): the case file reloads and shows this message. */
  onStale?: (message: string) => void;
}) {
  const quick = useSubmit({ onStale });
  const decide = useSubmit({ onStale });
  const [decision, setDecision] = useState<Decision | null>(null);
  const [dictamen, setDictamen] = useState("");
  const [tipoReporte, setTipoReporte] = useState<TipoReporteAml>(alerta.tipo_reporte);
  const [urgente, setUrgente] = useState(false);
  const [congelar, setCongelar] = useState(false);
  const [lift, setLift] = useState<Lift>("LEVANTAR_Y_LIBERAR");
  const [touched, setTouched] = useState(false);
  const [confirming, setConfirming] = useState<ResolverAlertaInput | null>(null);

  if (!canAct(role)) return null;
  const actions = allowedActions(role, me, alerta);

  let note: string | null = null;
  if (alerta.cliente.id === me) note = "You cannot act on alerts or restrictions that concern your own client record.";
  else if (alerta.estado === "EN_REVISION" && alerta.asignada_a?.id !== me) {
    note = `In review by ${personLabel(alerta.asignada_a, me, "another officer")}. Only the assigned officer can resolve it.`;
  }
  if (actions.length === 0) {
    return note ? (
      <Section title="Resolution" icon={<ClipboardCheck className="size-3.5" aria-hidden />}>
        <p className="text-sm text-ink-2">{note}</p>
      </Section>
    ) : null;
  }

  const decisions = (["FALSO_POSITIVO", "CONFIRMAR", "ESCALAR"] as const).filter((d) => actions.includes(d));
  const busy = quick.busy || decide.busy;

  const runQuick = async (accion: "TOMAR" | "LIBERAR") => {
    const result = await quick.run((clave_idempotencia) => api.amlResolve(token, alerta.id, { accion, clave_idempotencia }));
    if (result) onDone(successMessage(result, accion), result);
  };

  const buildInput = (d: Decision): ResolverAlertaInput => {
    const base = { accion: d, dictamen: dictamen.trim(), clave_idempotencia: "" };
    if (d === "ESCALAR") return { ...base, congelar: !restricted && congelar };
    if (d === "CONFIRMAR") return { ...base, tipo_reporte: tipoReporte, urgente, congelar: !restricted && congelar };
    // A false-positive proposal on a restricted client creates the unfreeze request (unless one is pending).
    return restricted && !pendingRequest ? { ...base, tipo_levantamiento: lift } : base;
  };

  const input = decision ? buildInput(decision) : null;
  const errors = input ? validateResolution(input, alerta, restricted) : [];
  const dictamenProblem = validateNarrative(dictamen, DICTAMEN_MIN, DICTAMEN_MAX);
  const otherErrors = errors.filter((e) => !e.startsWith("Dictamen:"));

  const send = async (body: ResolverAlertaInput) => {
    const result = await decide.run((clave_idempotencia) => api.amlResolve(token, alerta.id, { ...body, clave_idempotencia }));
    setConfirming(null);
    if (!result) return;
    setDecision(null);
    setDictamen("");
    setUrgente(false);
    setCongelar(false);
    setTouched(false);
    onDone(successMessage(result, body.accion), result);
  };

  const submit = () => {
    setTouched(true);
    if (!input || errors.length > 0) return;
    const needsConfirmation = input.accion === "CONFIRMAR" || input.accion === "FALSO_POSITIVO" || input.congelar === true;
    if (needsConfirmation) setConfirming(input);
    else void send(input);
  };

  return (
    <Section title="Resolution" icon={<ClipboardCheck className="size-3.5" aria-hidden />}>
      {actions.includes("TOMAR") ? (
        <div className="flex flex-wrap items-center gap-3">
          <Button onClick={() => void runQuick("TOMAR")} loading={quick.busy} disabled={busy}>
            <Hand className="size-4" aria-hidden />
            Take for review
          </Button>
          <p className="text-xs text-muted">Assigns the alert to you so you can resolve it.</p>
        </div>
      ) : null}
      {quick.error ? <Alert tone="error">{quick.error}</Alert> : null}

      {decisions.length > 0 ? (
        <form
          className="space-y-4"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          {alerta.resolucion_propuesta === "FALSO_POSITIVO" ? (
            <Alert tone="info">
              A false-positive proposal is awaiting a second compliance officer. You can still escalate the alert, or confirm it,
              which withdraws the proposal and the pending unfreeze request.
            </Alert>
          ) : null}
          <fieldset className="space-y-2">
            <legend className="mb-1 text-xs font-medium text-ink-2">Decision</legend>
            {decisions.map((d) => (
              <Choice
                key={d}
                name={`decision-${alerta.id}`}
                value={d}
                checked={decision === d}
                onChange={() => setDecision(d)}
                label={DECISION_COPY[d].label}
                hint={DECISION_COPY[d].hint}
                disabled={busy}
              />
            ))}
          </fieldset>

          {decision === "CONFIRMAR" ? (
            <div className="space-y-3 rounded-lg border border-border p-3">
              <fieldset className="space-y-2">
                <legend className="mb-1 text-xs font-medium text-ink-2">Report type</legend>
                <div className="grid gap-2 sm:grid-cols-2">
                  {(["INUSUAL", "INTERNA_PREOCUPANTE"] as const).map((t) => (
                    <Choice
                      key={t}
                      name={`report-type-${alerta.id}`}
                      value={t}
                      checked={tipoReporte === t}
                      onChange={() => setTipoReporte(t)}
                      label={REPORT_TYPE_LABEL[t]}
                      disabled={busy}
                    />
                  ))}
                </div>
              </fieldset>
              <Checkbox
                id={`urgent-${alerta.id}`}
                checked={urgente}
                onChange={setUrgente}
                label="Urgent: 24-hour report"
                hint="For cases that require reporting to the UIF within 24 hours."
              />
              {!restricted ? (
                <Checkbox
                  id={`restrict-confirm-${alerta.id}`}
                  checked={congelar}
                  onChange={setCongelar}
                  label="Restrict client"
                  hint="Refuses the client's outgoing operations and holds incoming funds."
                />
              ) : null}
            </div>
          ) : null}

          {decision === "ESCALAR" && !restricted ? (
            <div className="rounded-lg border border-border p-3">
              <Checkbox
                id={`restrict-escalate-${alerta.id}`}
                checked={congelar}
                onChange={setCongelar}
                label="Restrict client"
                hint="Refuses the client's outgoing operations and holds incoming funds."
              />
            </div>
          ) : null}

          {decision === "FALSO_POSITIVO" && restricted ? (
            <div className="space-y-3 rounded-lg border border-border p-3">
              <p className="text-sm text-ink-2">{FOUR_EYES_NOTE}</p>
              {pendingRequest ? (
                <p className="text-xs text-muted">
                  Request #{pendingRequest.folio} ({REQUEST_TYPE_LABEL[pendingRequest.tipo]}) is already pending; this proposal joins it.
                </p>
              ) : (
                <fieldset className="space-y-2">
                  <legend className="mb-1 text-xs font-medium text-ink-2">Held funds</legend>
                  <div className="grid gap-2 sm:grid-cols-2">
                    <Choice
                      name={`lift-${alerta.id}`}
                      value="LEVANTAR_Y_LIBERAR"
                      checked={lift === "LEVANTAR_Y_LIBERAR"}
                      onChange={() => setLift("LEVANTAR_Y_LIBERAR")}
                      label="Release held funds"
                      disabled={busy}
                    />
                    <Choice
                      name={`lift-${alerta.id}`}
                      value="LEVANTAR_CONSERVANDO_RETENCIONES"
                      checked={lift === "LEVANTAR_CONSERVANDO_RETENCIONES"}
                      onChange={() => setLift("LEVANTAR_CONSERVANDO_RETENCIONES")}
                      label="Keep funds held"
                      disabled={busy}
                    />
                  </div>
                </fieldset>
              )}
            </div>
          ) : null}

          {decision ? (
            <NarrativeField
              id={`dictamen-${alerta.id}`}
              label="Dictamen"
              value={dictamen}
              onChange={setDictamen}
              min={DICTAMEN_MIN}
              max={DICTAMEN_MAX}
              error={touched ? dictamenProblem : null}
              hint="Your assessment of the facts. It is part of the permanent case record and is never shown to the client."
              disabled={busy}
            />
          ) : null}

          {touched && otherErrors.length > 0 ? (
            <Alert tone="error">
              <ul className="list-disc pl-4">
                {otherErrors.map((e) => (
                  <li key={e}>{e}</li>
                ))}
              </ul>
            </Alert>
          ) : null}
          {decide.error && !confirming ? <Alert tone="error">{decide.error}</Alert> : null}

          <div className="flex flex-wrap items-center justify-between gap-2">
            {actions.includes("LIBERAR") ? (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => void runQuick("LIBERAR")}
                loading={quick.busy}
                disabled={busy}
                title="Unassign the alert and return it to the pending queue"
              >
                <Undo2 className="size-3.5" aria-hidden />
                Release
              </Button>
            ) : (
              <span />
            )}
            <Button type="submit" disabled={!decision || busy} loading={decide.busy && !confirming}>
              {decision ? `${DECISION_COPY[decision].label.replace(/ \(UIF report\)$/, "")}` : "Choose a decision"}
            </Button>
          </div>
        </form>
      ) : null}

      {confirming ? (
        <Dialog
          open
          onClose={() => setConfirming(null)}
          busy={decide.busy}
          title={confirming.accion === "FALSO_POSITIVO" ? "Clear as false positive?" : confirming.accion === "CONFIRMAR" ? "Confirm suspicious activity?" : "Escalate and restrict?"}
          footer={
            <>
              <Button variant="ghost" onClick={() => setConfirming(null)} disabled={decide.busy} data-autofocus>
                Cancel
              </Button>
              <Button
                variant={confirming.accion === "FALSO_POSITIVO" ? "primary" : "danger"}
                onClick={() => void send(confirming)}
                loading={decide.busy}
              >
                {confirming.accion === "FALSO_POSITIVO" ? "Clear alert" : confirming.accion === "CONFIRMAR" ? "Confirm" : "Escalate"}
              </Button>
            </>
          }
        >
          <div className="space-y-2 px-5 py-4 text-sm leading-relaxed text-ink-2">
            {confirmationText(confirming, alerta, restricted, pendingRequest).map((line) => (
              <p key={line}>{line}</p>
            ))}
            {decide.error ? <Alert tone="error">{decide.error}</Alert> : null}
          </div>
        </Dialog>
      ) : null}
    </Section>
  );
}
