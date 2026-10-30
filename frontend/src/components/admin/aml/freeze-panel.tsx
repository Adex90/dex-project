"use client";

import { useState } from "react";
import { LockKeyhole, LockOpen, Snowflake } from "lucide-react";

import { Dialog } from "@/components/admin/aml/dialog";
import { Choice, Facts, MxTime, NarrativeField, Section, SlaText, personLabel, useNow } from "@/components/admin/aml/parts";
import { useSubmit } from "@/components/admin/aml/use-submit";
import { Money } from "@/components/ui/data";
import { Alert, Badge, Button } from "@/components/ui/primitives";
import { api } from "@/lib/api/endpoints";
import type { AlertaAml, CongelamientoAml, Rol, TipoSolicitudAml, Uuid } from "@/lib/api/types";
import {
  DICTAMEN_MAX,
  DICTAMEN_MIN,
  FREEZE_ORIGIN_LABEL,
  FREEZE_STATE_LABEL,
  HOLD_STATE_LABEL,
  REQUEST_TYPE_LABEL,
  canAct,
  validateNarrative,
} from "@/lib/aml";

const FOUR_EYES_NOTE = "A second compliance officer must approve this request before it takes effect.";

/** Four-eyes request: lift the restriction (releasing or keeping the held funds) or release funds kept after a lift. */
export function UnfreezeRequestDialog({
  congelamiento,
  token,
  onClose,
  onDone,
  onStale,
}: {
  congelamiento: CongelamientoAml;
  token: string;
  onClose: () => void;
  onDone: (message: string) => void;
  /** The API refused because the restriction changed (409): the caller reloads it. */
  onStale?: (message: string) => void;
}) {
  const release = congelamiento.estado === "LEVANTADO";
  const [tipo, setTipo] = useState<TipoSolicitudAml>(release ? "LIBERAR_RETENCIONES" : "LEVANTAR_Y_LIBERAR");
  const [justificacion, setJustificacion] = useState("");
  const [touched, setTouched] = useState(false);
  const { busy, error, run } = useSubmit({ onStale });
  const problem = validateNarrative(justificacion, DICTAMEN_MIN, DICTAMEN_MAX);

  const submit = async () => {
    setTouched(true);
    if (problem) return;
    const result = await run((clave_idempotencia) =>
      api.amlRequestUnfreeze(token, congelamiento.id, { tipo, justificacion: justificacion.trim(), clave_idempotencia }),
    );
    if (result) onDone(`Request #${result.solicitud.folio} sent. ${FOUR_EYES_NOTE}`);
  };

  const held = congelamiento.retenido_vigente;
  return (
    <Dialog
      open
      onClose={onClose}
      busy={busy}
      title={release ? "Release kept funds" : "Request unfreeze"}
      description={
        release
          ? `Requests the release of the funds still held for ${congelamiento.cliente.nombre_completo} after the restriction was lifted.`
          : `Requests lifting the restriction on ${congelamiento.cliente.nombre_completo}'s account.`
      }
      footer={
        <>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button onClick={() => void submit()} loading={busy}>
            Send for approval
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
        {release ? (
          <p className="text-sm text-ink-2">
            Held funds: <Money value={held} className="font-semibold text-ink" />. Approval moves them back to the client&apos;s
            available balance.
          </p>
        ) : (
          <fieldset className="space-y-2">
            <legend className="mb-1 text-xs font-medium text-ink-2">
              Held funds (<Money value={held} />)
            </legend>
            <Choice
              name="tipo-levantamiento"
              value="LEVANTAR_Y_LIBERAR"
              checked={tipo === "LEVANTAR_Y_LIBERAR"}
              onChange={() => setTipo("LEVANTAR_Y_LIBERAR")}
              label="Lift and release held funds"
              hint="The account works normally again and the held funds become available."
            />
            <Choice
              name="tipo-levantamiento"
              value="LEVANTAR_CONSERVANDO_RETENCIONES"
              checked={tipo === "LEVANTAR_CONSERVANDO_RETENCIONES"}
              onChange={() => setTipo("LEVANTAR_CONSERVANDO_RETENCIONES")}
              label="Lift but keep funds held"
              hint="The account works normally again; releasing the held funds needs a separate approved request."
            />
          </fieldset>
        )}
        <NarrativeField
          id="unfreeze-justification"
          label="Justification"
          value={justificacion}
          onChange={setJustificacion}
          min={DICTAMEN_MIN}
          max={DICTAMEN_MAX}
          error={touched ? problem : null}
          hint="Why the restriction is no longer needed. It is part of the permanent case record."
          disabled={busy}
        />
        <Alert tone="info">{FOUR_EYES_NOTE}</Alert>
        {error ? <Alert tone="error">{error}</Alert> : null}
      </form>
    </Dialog>
  );
}

/** Manual freeze: one officer may tighten. Creates a manual alert when no open alert is given. */
export function ManualFreezeDialog({
  token,
  cliente,
  alerta,
  onClose,
  onDone,
  onStale,
}: {
  token: string;
  cliente: { id: Uuid; nombre_completo: string };
  /** The open alert this restriction belongs to, if any. */
  alerta: Pick<AlertaAml, "id" | "folio"> | null;
  onClose: () => void;
  onDone: (message: string, alerts: AlertaAml[]) => void;
  /** The API refused because the alert changed (409): the caller reloads it. */
  onStale?: (message: string) => void;
}) {
  const [motivo, setMotivo] = useState("");
  const [touched, setTouched] = useState(false);
  const { busy, error, run } = useSubmit({ onStale });
  const problem = validateNarrative(motivo, DICTAMEN_MIN, DICTAMEN_MAX);

  const submit = async () => {
    setTouched(true);
    if (problem) return;
    const result = await run((clave_idempotencia) =>
      api.amlFreezeClient(token, cliente.id, { motivo: motivo.trim(), alerta_id: alerta?.id ?? null, clave_idempotencia }),
    );
    if (!result) return;
    onDone(
      result.ya_congelado
        ? `${cliente.nombre_completo} was already restricted (restriction #${result.congelamiento.folio}); nothing changed.`
        : `Restriction #${result.congelamiento.folio} applied to ${cliente.nombre_completo}.`,
      result.alerta ? [result.alerta] : [],
    );
  };

  return (
    <Dialog
      open
      onClose={onClose}
      busy={busy}
      title="Freeze client"
      description={`This places a preventive restriction on ${cliente.nombre_completo}'s account: outgoing operations are refused and incoming funds are held. Lifting it requires approval by a second compliance officer.`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="danger" onClick={() => void submit()} loading={busy}>
            <LockKeyhole className="size-3.5" aria-hidden />
            Restrict client
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
        <p className="text-sm text-ink-2">
          {alerta
            ? `The restriction is linked to alert #${alerta.folio}.`
            : "No open alert is linked: a manual alert is opened for this restriction."}{" "}
          The client is not told why; they only see that operations are not available.
        </p>
        <NarrativeField
          id="freeze-reason"
          label="Reason"
          value={motivo}
          onChange={setMotivo}
          min={DICTAMEN_MIN}
          max={DICTAMEN_MAX}
          error={touched ? problem : null}
          hint="The facts that justify restricting the account now. It is part of the permanent case record."
          disabled={busy}
        />
        {error ? <Alert tone="error">{error}</Alert> : null}
      </form>
    </Dialog>
  );
}

/**
 * A restriction (CongelamientoAml): state, origin, SLA countdown, blocked
 * attempts, holds and the pending four-eyes request, with "Request unfreeze"
 * (ACTIVO, nothing pending) and "Release kept funds" (LEVANTADO with holds).
 */
export function FreezePanel({
  congelamiento: c,
  role,
  me,
  token,
  onChanged,
  onStale,
  onOpenAlert,
}: {
  congelamiento: CongelamientoAml;
  role: Rol | null;
  me: Uuid;
  token: string;
  /** After a request was created: refetch the case file, the queue and the summary. */
  onChanged: (message: string) => void;
  /** The API refused because the restriction changed (409): refetch it. */
  onStale?: (message: string) => void;
  /** Opens the alert that caused the restriction (Frozen clients tab). */
  onOpenAlert?: (id: Uuid) => void;
}) {
  const now = useNow();
  const [dialog, setDialog] = useState(false);
  const active = c.estado === "ACTIVO";
  const heldNow = c.retenciones.some((r) => r.estado === "VIGENTE");
  const ownRecord = c.cliente.id === me;
  const mayAct = canAct(role) && !ownRecord && !c.solicitud_pendiente;
  const canRequestUnfreeze = mayAct && active;
  const canReleaseKept = mayAct && !active && heldNow;
  const pending = c.solicitud_pendiente;

  return (
    <Section
      title={`Restriction #${c.folio}`}
      icon={<Snowflake className="size-3.5" aria-hidden />}
      action={
        canRequestUnfreeze ? (
          <Button variant="secondary" size="sm" onClick={() => setDialog(true)}>
            <LockOpen className="size-3.5" aria-hidden />
            Request unfreeze
          </Button>
        ) : canReleaseKept ? (
          <Button variant="secondary" size="sm" onClick={() => setDialog(true)}>
            <LockOpen className="size-3.5" aria-hidden />
            Release kept funds
          </Button>
        ) : null
      }
    >
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone={active ? "danger" : "neutral"}>{active ? "Preventive restriction" : FREEZE_STATE_LABEL[c.estado]}</Badge>
        <Badge tone="neutral">{FREEZE_ORIGIN_LABEL[c.origen]}</Badge>
        {c.confirmado ? <Badge tone="danger">Confirmed by a suspicious-activity report</Badge> : null}
        {active ? (
          <span className="text-xs">
            Review: <SlaText slaVenceEn={c.sla_vence_en} now={now} />
          </span>
        ) : null}
      </div>
      <Facts
        rows={[
          ["Client", c.cliente.nombre_completo],
          ["Since", <MxTime key="s" at={c.congelado_en} />],
          ["Restricted by", personLabel(c.congelado_por, me, "System (automatic)")],
          ["Reason", <span key="m" className="whitespace-pre-wrap">{c.motivo}</span>],
          [
            "Blocked attempts",
            <span key="b">
              {c.intentos_bloqueados}
              {c.ultimo_intento_en ? (
                <span className="text-muted">
                  {" "}
                  · last <MxTime at={c.ultimo_intento_en} />
                </span>
              ) : null}
            </span>,
          ],
          ["Held now", <Money key="h" value={c.retenido_vigente} />],
          ...(c.estado === "LEVANTADO"
            ? ([
                ["Lifted by", personLabel(c.levantado_por, me)],
                ["Lifted", c.levantado_en ? <MxTime key="l" at={c.levantado_en} /> : null],
              ] as [string, React.ReactNode][])
            : []),
          [
            "Alert",
            c.alerta_id ? (
              onOpenAlert ? (
                <button key="a" type="button" className="font-medium text-accent hover:underline" onClick={() => onOpenAlert(c.alerta_id!)}>
                  Open the alert
                </button>
              ) : (
                <span key="a" className="font-mono text-xs">
                  {c.alerta_id}
                </span>
              )
            ) : null,
          ],
        ]}
      />

      {c.retenciones.length > 0 ? (
        <div className="overflow-x-auto rounded-lg border border-border">
          <table className="w-full min-w-[480px] text-left text-xs">
            <caption className="sr-only">Held funds of this restriction</caption>
            <thead>
              <tr className="border-b border-border bg-surface-2/60 text-[11px] text-muted">
                <th scope="col" className="py-1.5 pr-3 pl-3 font-medium">Held</th>
                <th scope="col" className="py-1.5 pr-3 text-right font-medium">Amount</th>
                <th scope="col" className="py-1.5 pr-3 font-medium">State</th>
                <th scope="col" className="py-1.5 pr-3 font-medium">Released</th>
              </tr>
            </thead>
            <tbody>
              {c.retenciones.map((r) => (
                <tr key={r.id} className="border-b border-border/60 last:border-b-0">
                  <td className="py-1.5 pr-3 pl-3 text-ink-2">
                    <MxTime at={r.creado_en} />
                  </td>
                  <td className="py-1.5 pr-3 text-right font-semibold">
                    <Money value={r.monto} />
                  </td>
                  <td className="py-1.5 pr-3">
                    <Badge tone={r.estado === "VIGENTE" ? "warning" : "verified"}>{HOLD_STATE_LABEL[r.estado]}</Badge>
                  </td>
                  <td className="py-1.5 pr-3 text-ink-2">{r.liberada_en ? <MxTime at={r.liberada_en} /> : "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="text-xs text-muted">No funds have been held under this restriction.</p>
      )}

      {pending ? (
        <div className="space-y-1.5 rounded-lg border border-info/30 bg-info-soft p-3 text-sm">
          <p className="font-semibold text-info">
            Pending request #{pending.folio}: {REQUEST_TYPE_LABEL[pending.tipo]}
          </p>
          <p className="text-xs text-ink-2">
            Requested by {personLabel(pending.solicitada_por, me)} · <MxTime at={pending.solicitada_en} />. Awaiting a second
            compliance officer.
          </p>
          <p className="text-[13px] whitespace-pre-wrap text-ink">{pending.justificacion}</p>
        </div>
      ) : null}

      {ownRecord && canAct(role) ? (
        <p className="text-xs text-muted">You cannot act on restrictions that concern your own client record.</p>
      ) : null}

      {dialog ? (
        <UnfreezeRequestDialog
          congelamiento={c}
          token={token}
          onClose={() => setDialog(false)}
          onDone={(message) => {
            setDialog(false);
            onChanged(message);
          }}
          onStale={onStale}
        />
      ) : null}
    </Section>
  );
}
