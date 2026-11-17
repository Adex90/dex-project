"use client";

import type { ReactNode } from "react";
import { FileSignature, Loader2, Pencil, ShieldCheck } from "lucide-react";

import { Alert, Button, cx } from "@/components/ui/primitives";
import type { Declaraciones, DocumentoContrato, VistaExpediente } from "@/lib/api/types";
import { COMPLIANCE_EMAIL } from "@/lib/config";

import { AGREEMENTS } from "./agreements";
import {
  IDENTITY_CONFLICT_RETRY,
  sourceOfFundsLabel,
  type IneDraft,
  type Step,
  type VerificationAction,
} from "./logic";
import type { LivenessCapture } from "./step-liveness";

const birthDate = new Intl.DateTimeFormat("en-US", { year: "numeric", month: "long", day: "numeric", timeZone: "UTC" });

/** Why the last submission was refused, and where to fix it. */
export interface SubmitProblem {
  message: string;
  action: VerificationAction;
}

/**
 * Step 4: everything the customer is about to submit, each part with a way
 * back to edit it, then the three agreements (each read and accepted on its
 * own) and the gold "Submit for Verification". The agreements' version is the
 * one in force (`contratos_vigentes`), and the backend records the instant.
 */
export function StepReview({
  file,
  ine,
  capture,
  declarations,
  accepted,
  onAccept,
  onEdit,
  onSubmit,
  submitting,
  problem,
  identityConflict,
}: {
  file: VistaExpediente;
  ine: IneDraft;
  capture: LivenessCapture;
  declarations: Declaraciones;
  accepted: ReadonlySet<DocumentoContrato>;
  onAccept: (id: DocumentoContrato, accepted: boolean) => void;
  onEdit: (step: Step) => void;
  onSubmit: () => void;
  submitting: boolean;
  problem: SubmitProblem | null;
  /** The declared CURP / RFC / voter key were refused as in use elsewhere and have not changed since. */
  identityConflict: boolean;
}) {
  const name = [file.nombres, file.apellido_paterno, file.apellido_materno].filter(Boolean).join(" ");
  const d = file.domicilio;
  const address = [
    [d.calle, d.numero_exterior].filter(Boolean).join(" ") + (d.numero_interior ? `, int. ${d.numero_interior}` : ""),
    d.colonia,
    d.municipio,
    d.estado,
    d.codigo_postal,
  ]
    .filter(Boolean)
    .join(", ");
  const allAccepted = AGREEMENTS.every((a) => accepted.has(a.id));
  const blocked = identityConflict || !allAccepted;

  return (
    <div className="space-y-6">
      <Summary title="Identity" onEdit={() => onEdit(1)} disabled={submitting}>
        <Row label="Full legal name" value={name} />
        <Row label="CURP" value={file.curp ?? "—"} mono />
        <Row label="RFC" value={file.rfc ?? "—"} mono />
        <Row
          label="Date of birth"
          value={file.fecha_nacimiento ? birthDate.format(new Date(`${file.fecha_nacimiento}T00:00:00Z`)) : "—"}
        />
        <Row label="Nationality" value="Mexican" />
        <Row label="Address" value={address || "—"} />
      </Summary>

      <Summary title="Official ID" onEdit={() => onEdit(2)} disabled={submitting}>
        <Row label="Document" value="INE (voting credential)" />
        <Row label="Voter key" value={ine.clave_elector || "—"} mono />
        <Row label="Valid through" value={ine.vigencia ? `December 31, ${ine.vigencia}` : "—"} />
        <div className="px-4 py-3">
          <dt className="sr-only">Photos</dt>
          <dd className="grid grid-cols-2 gap-2">
            {(["frente", "reverso"] as const).map((side) =>
              ine[side] ? (
                // eslint-disable-next-line @next/next/no-img-element -- local data URL, nothing to optimize
                <img
                  key={side}
                  src={ine[side]}
                  alt={side === "frente" ? "Front of your INE, as submitted" : "Back of your INE, as submitted"}
                  className="aspect-[86/54] w-full border border-border-strong object-cover"
                />
              ) : null,
            )}
          </dd>
        </div>
      </Summary>

      <Summary title="Liveness and funds" onEdit={() => onEdit(3)} disabled={submitting}>
        <Row label="Liveness check" value={`Complete · ${capture.frames.length} frames`} />
        <Row label="Occupation" value={declarations.ocupacion} />
        <Row label="Source of funds" value={sourceOfFundsLabel(declarations.origen_recursos)} />
        <Row
          label="Politically exposed person"
          value={declarations.declara_pep ? "Yes · approved by a person on our team" : "No"}
        />
      </Summary>

      <section aria-labelledby="agreements-title" className="space-y-3" data-testid="agreements">
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <h3 id="agreements-title" className="flex items-center gap-2 text-sm font-semibold text-ink">
            <FileSignature className="size-4 text-synthetic-yellow" aria-hidden />
            Agreements
          </h3>
          <span
            className="font-mono text-[11px] tracking-[0.12em] text-muted uppercase"
            data-testid="agreements-version"
          >
            Version {file.contratos_vigentes}
          </span>
        </div>
        <ul className="space-y-2">
          {AGREEMENTS.map((a) => {
            const id = `acuerdo-${a.id.toLowerCase().replace(/_/g, "-")}`;
            const checked = accepted.has(a.id);
            return (
              <li
                key={a.id}
                className={cx("border-2 transition-colors", checked ? "border-synthetic-yellow" : "border-border")}
                data-testid="agreement"
                data-accepted={checked}
              >
                <details className="group border-b border-border px-4 py-3">
                  <summary className="cursor-pointer list-none text-sm font-semibold text-ink marker:hidden focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none">
                    <span className="flex items-center justify-between gap-3">
                      {a.title}
                      <span className="text-[11px] font-semibold tracking-[0.14em] text-synthetic-yellow uppercase group-open:hidden">
                        Read
                      </span>
                      <span className="hidden text-[11px] font-semibold tracking-[0.14em] text-muted uppercase group-open:inline">
                        Close
                      </span>
                    </span>
                  </summary>
                  <ul className="mt-3 list-disc space-y-1 pl-4 text-xs leading-relaxed text-ink-2">
                    {a.points.map((p) => (
                      <li key={p}>{p}</li>
                    ))}
                    {a.id === "AVISO_PRIVACIDAD" ? (
                      <li>
                        Full privacy notice and ARCO requests:{" "}
                        <a
                          href={`mailto:${COMPLIANCE_EMAIL}`}
                          className="text-synthetic-yellow underline underline-offset-2"
                        >
                          {COMPLIANCE_EMAIL}
                        </a>
                        .
                      </li>
                    ) : null}
                  </ul>
                </details>
                <label htmlFor={id} className="flex cursor-pointer items-start gap-2.5 px-4 py-3 text-sm text-ink">
                  <input
                    id={id}
                    type="checkbox"
                    checked={checked}
                    disabled={submitting}
                    onChange={(e) => onAccept(a.id, e.target.checked)}
                    className="mt-0.5 size-4 shrink-0 accent-synthetic-yellow"
                  />
                  <span>{a.accept}</span>
                </label>
              </li>
            );
          })}
        </ul>
        <p className="flex items-start gap-2 text-xs leading-relaxed text-muted">
          <ShieldCheck className="mt-0.5 size-3.5 shrink-0 text-synthetic-yellow" aria-hidden />
          <span>
            Accepting is your electronic signature: we record the version, the time, and the device you submit from.
          </span>
        </p>
      </section>

      {problem ? (
        <Alert tone="error" title="Verification not submitted">
          <p>{problem.message}</p>
          {problem.action === "id" || problem.action === "profile" || problem.action === "declarations" ? (
            <Button
              type="button"
              variant="secondary"
              size="sm"
              className="mt-2"
              onClick={() => onEdit(problem.action === "id" ? 2 : problem.action === "profile" ? 1 : 3)}
            >
              {problem.action === "id"
                ? "Edit ID details and photos"
                : problem.action === "profile"
                  ? "Edit personal data"
                  : "Edit your answers"}
            </Button>
          ) : null}
          {problem.action === "selfie" || problem.action === "consent" ? (
            <Button type="button" variant="secondary" size="sm" className="mt-2" onClick={() => onEdit(3)}>
              Retake the liveness check
            </Button>
          ) : null}
          {problem.action === "identity" ? (
            <div className="mt-2 flex flex-wrap gap-2">
              <Button type="button" variant="secondary" size="sm" onClick={() => onEdit(2)}>
                Edit ID details
              </Button>
              <Button type="button" variant="secondary" size="sm" onClick={() => onEdit(1)}>
                Edit personal data
              </Button>
            </div>
          ) : null}
        </Alert>
      ) : identityConflict ? (
        <Alert tone="warning" title="Your identity details are already in use">
          {IDENTITY_CONFLICT_RETRY}
        </Alert>
      ) : null}

      {submitting ? (
        <div
          className="flex items-start gap-3 border border-synthetic-yellow/40 px-4 py-3 text-sm text-ink"
          role="status"
        >
          <Loader2 className="mt-0.5 size-4 shrink-0 animate-spin text-synthetic-yellow" aria-hidden />
          <div>
            <p className="font-semibold">Verifying your identity…</p>
            <p className="text-[13px] text-ink-2">
              Uploading your INE photos and {capture.frames.length} frames, checking liveness and matching your face
              with your credential. Do not close this page.
            </p>
          </div>
        </div>
      ) : null}

      <button
        type="button"
        onClick={onSubmit}
        disabled={blocked || submitting}
        className={cx(
          "inline-flex h-14 w-full items-center justify-center gap-2 bg-synthetic-yellow text-base font-bold tracking-[0.18em] text-black uppercase transition-colors",
          "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
          "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
          "disabled:cursor-not-allowed disabled:opacity-40",
        )}
        data-testid="submit-verification"
      >
        {submitting ? <Loader2 className="size-5 animate-spin" aria-hidden /> : null}
        {submitting ? "Submitting…" : "Submit for Verification"}
      </button>
      {!allAccepted && !submitting ? (
        <p className="text-center text-xs text-muted">Accept the three agreements above to submit.</p>
      ) : null}
    </div>
  );
}

function Summary({
  title,
  onEdit,
  disabled,
  children,
}: {
  title: string;
  onEdit: () => void;
  disabled: boolean;
  children: ReactNode;
}) {
  return (
    <section className="border border-border" data-testid="review-section">
      <div className="flex items-center justify-between gap-3 border-b border-border bg-surface-2 px-4 py-2.5">
        <h3 className="text-[11px] font-semibold tracking-[0.2em] text-ink-2 uppercase">{title}</h3>
        <button
          type="button"
          onClick={onEdit}
          disabled={disabled}
          className="inline-flex items-center gap-1.5 text-[11px] font-semibold tracking-[0.16em] text-synthetic-yellow uppercase hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none disabled:opacity-40"
        >
          <Pencil className="size-3" aria-hidden />
          Edit
          <span className="sr-only"> {title.toLowerCase()}</span>
        </button>
      </div>
      <dl className="divide-y divide-border">{children}</dl>
    </section>
  );
}

function Row({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="grid gap-1 px-4 py-2.5 text-sm sm:grid-cols-[11rem_1fr] sm:gap-4">
      <dt className="text-muted">{label}</dt>
      <dd className={cx("min-w-0 break-words text-ink", mono && "font-mono tracking-wide")}>{value}</dd>
    </div>
  );
}
