"use client";

import { useState } from "react";
import Link from "next/link";
import {
  Check,
  CheckCircle2,
  Clock,
  Copy,
  LayoutDashboard,
  Loader2,
  RefreshCw,
  RotateCcw,
  ShieldAlert,
  ShieldCheck,
  ShieldOff,
  XCircle,
} from "lucide-react";

import { Alert, Badge, Button, Stat, cx } from "@/components/ui/primitives";
import type { EstadoKyc, ResultadoKyc, ResultadoVerificacion, VistaExpediente } from "@/lib/api/types";
import { COMPLIANCE_EMAIL } from "@/lib/config";
import { ESTADO_KYC_LABELS, describeKycMessage } from "@/lib/kyc";

import { STATUS_TONES, canRetry, formatPercent, splitDetails, statusHeadline, type StatusTone } from "./logic";
import { CheckRow } from "./parts";

const BADGE_TONES: Record<StatusTone, string> = {
  verified: "border-verified/30 bg-verified-soft text-verified",
  info: "border-info/30 bg-info-soft text-info",
  warning: "border-warning/30 bg-warning-soft text-warning",
  danger: "border-danger/30 bg-danger-soft text-danger",
};

const BADGE_ICONS: Record<EstadoKyc, typeof ShieldCheck> = {
  PENDIENTE: Clock,
  EN_REVISION: Loader2,
  VERIFICADO: ShieldCheck,
  RECHAZADO: ShieldAlert,
};

const RESULT_LABELS: Record<ResultadoKyc, { label: string; tone: "verified" | "info" | "danger" }> = {
  APROBADO: { label: "Approved", tone: "verified" },
  REVISION: { label: "Sent to manual review", tone: "info" },
  RECHAZADO: { label: "Not approved", tone: "danger" },
};

const dateTime = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeStyle: "short" });
const clock = new Intl.DateTimeFormat("en-US", { timeStyle: "medium" });

/**
 * The prominent KYC state badge (stable test hook: data-testid + data-status).
 * `suspended`: the client is SUSPENDIDO (its file reads RECHAZADO).
 */
export function KycStatusBadge({ estado, suspended = false }: { estado: EstadoKyc; suspended?: boolean }) {
  const Icon = suspended ? ShieldOff : BADGE_ICONS[estado];
  return (
    <span
      data-testid="kyc-status-badge"
      data-status={suspended ? "SUSPENDIDO" : estado}
      className={cx(
        "inline-flex items-center gap-2 rounded-full border px-4 py-1.5 text-sm font-semibold",
        BADGE_TONES[suspended ? "danger" : STATUS_TONES[estado]],
      )}
    >
      <Icon className={cx("size-4", estado === "EN_REVISION" && "animate-spin [animation-duration:2.5s]")} aria-hidden />
      {suspended ? "Suspended" : ESTADO_KYC_LABELS[estado]}
    </span>
  );
}

/** Mailto link to the compliance team (the single contact channel). */
export function ComplianceLink() {
  return (
    <a href={`mailto:${COMPLIANCE_EMAIL}`} className="font-semibold break-all underline underline-offset-2">
      {COMPLIANCE_EMAIL}
    </a>
  );
}

/** The full client reference (never truncated: it is quoted to compliance) with a copy button. */
function Reference({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      /* clipboard unavailable (insecure context) */
    }
  };
  return (
    <span className="mt-1.5 flex items-start gap-1.5 font-mono text-xs">
      <span className="min-w-0 break-all" data-testid="kyc-reference">
        {value}
      </span>
      <button
        type="button"
        onClick={() => void copy()}
        className="shrink-0 rounded p-0.5 transition hover:bg-surface-2 focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
        aria-label={copied ? "Reference copied" : "Copy reference"}
        title={copied ? "Copied" : "Copy reference"}
      >
        {copied ? <Check className="size-3" aria-hidden /> : <Copy className="size-3" aria-hidden />}
      </button>
    </span>
  );
}

/** A message about the last submission that must survive the move to this page. */
export interface SubmitNotice {
  message: string;
  /** No more submissions are possible for now (KYC_SIN_INTENTOS, KYC_IDENTIDAD_BLOQUEADA). */
  lock: boolean;
}

/** Step 4: state of the file, attempts, last result and what to do next. */
export function StepStatus({
  file,
  immediate,
  notice,
  suspended,
  polling,
  onTryAgain,
  onResume,
  onDashboard,
}: {
  file: VistaExpediente;
  /** Result returned by POST /kyc/verificar in this session (shown while the file's view lacks it). */
  immediate: ResultadoVerificacion | null;
  /** Why the last submission was refused (e.g. the conflict lockout), kept by the page. */
  notice: SubmitNotice | null;
  /** The client is SUSPENDIDO (GET /yo); its file reads RECHAZADO. */
  suspended: boolean;
  polling: { active: boolean; checking: boolean; lastChecked: Date | null; onCheckNow: () => void };
  onTryAgain: () => void;
  onResume: () => void;
  onDashboard: () => void;
}) {
  const enCurso = file.verificacion_en_curso;
  // The answer of POST /kyc/verificar wins while the file's view does not have
  // that submission's result yet (the reload after it failed or lags behind).
  const fallback =
    immediate && immediate.verificacion_id === enCurso && file.ultimo_resultado?.verificacion_id !== enCurso
      ? immediate
      : null;
  const awaitingProvider =
    file.estado_kyc === "EN_REVISION" &&
    !fallback &&
    (!file.ultimo_resultado || (enCurso !== null && file.ultimo_resultado.verificacion_id !== enCurso));
  // While a new submission waits for its result, the previous attempt's verdict would only confuse.
  const last = fallback || awaitingProvider ? null : file.ultimo_resultado;
  const headline = statusHeadline(file, { awaitingProvider, suspended });
  const details = splitDetails(last ? last.detalles_fallo : fallback?.detalles);
  const score = formatPercent(last ? last.score_similitud : fallback?.score_similitud);
  const threshold = last && last.umbral_aplicado !== null ? formatPercent(last.umbral_aplicado, 0) : null;
  const result = last?.resultado ?? fallback?.resultado ?? null;
  const simulated = last?.proveedor === "simulado";
  const simulatedNote = simulated ? "(simulated)" : undefined;
  const locked = notice?.lock ?? false;

  return (
    <div className="space-y-5">
      <div className="space-y-3 text-center" aria-live="polite" aria-atomic="true">
        <KycStatusBadge estado={file.estado_kyc} suspended={suspended} />
        <div className="space-y-1">
          <p className="text-base font-semibold text-ink">{headline.title}</p>
          <p className="text-sm text-muted">{headline.body}</p>
        </div>
      </div>

      {/* The API withholds the similarity score from clients (null), so the stat
          only appears when one is actually disclosed instead of a permanent "—". */}
      <dl
        className={cx(
          "grid gap-3 rounded-lg border border-border bg-surface-2/60 px-4 py-3",
          score !== null ? "grid-cols-3" : "grid-cols-2",
        )}
      >
        <Stat label="Attempts used" value={`${file.intentos} of ${file.max_intentos}`} />
        <Stat label="Remaining" value={file.intentos_restantes} />
        {score !== null ? (
          <Stat label="Face match" value={score} hint={threshold ? `Minimum ${threshold}` : undefined} />
        ) : null}
      </dl>

      {notice ? (
        <Alert
          tone={notice.lock ? "error" : "warning"}
          title={notice.lock ? "Remote verification is locked" : "Your last submission was not processed"}
        >
          <p data-testid="kyc-submit-notice">{notice.message}</p>
          {notice.lock ? (
            <p className="mt-1">
              Write to <ComplianceLink /> from the email linked to your account and quote this reference:
            </p>
          ) : null}
          {notice.lock ? <Reference value={file.cliente_id} /> : null}
        </Alert>
      ) : null}

      {file.estado_kyc === "EN_REVISION" ? (
        <Alert tone="info" title={awaitingProvider ? "Waiting for the verification provider" : "Human review in progress"}>
          <p>
            {awaitingProvider
              ? "Your photos were received. The verification provider will send its result shortly."
              : "Your photos passed the automatic checks, but a compliance analyst has to confirm the result, for example when the face match is borderline or a detail needs a second look."}{" "}
            Money movements stay disabled until your identity is verified.
          </p>
          <p className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
            {polling.checking ? (
              <span className="inline-flex items-center gap-1">
                <Loader2 className="size-3 animate-spin" aria-hidden /> Checking for updates…
              </span>
            ) : polling.lastChecked ? (
              <span>
                Checked at {clock.format(polling.lastChecked)}
                {polling.active ? "; checking again every 10 seconds while this page is open." : "."}
              </span>
            ) : null}
            <button
              type="button"
              onClick={polling.onCheckNow}
              disabled={polling.checking}
              className="inline-flex items-center gap-1 font-semibold underline underline-offset-2 disabled:opacity-50"
            >
              <RefreshCw className="size-3" aria-hidden />
              Check now
            </button>
          </p>
        </Alert>
      ) : null}

      {file.estado_kyc === "RECHAZADO" ? (
        <Alert tone="error" title="Contact our compliance team">
          {file.motivo_rechazo ? <p>Reason: {describeKycMessage(file.motivo_rechazo)}</p> : null}
          <p className="mt-1">
            {suspended
              ? "Only compliance can lift a suspension."
              : "Only compliance can reopen a rejected file, which also restores your attempts."}{" "}
            Write to <ComplianceLink /> from the email linked to your account and quote this reference:
          </p>
          <Reference value={file.cliente_id} />
        </Alert>
      ) : null}

      {simulated ? (
        <Alert tone="info" title="Demo environment">
          This verification was simulated: no document or biometric check was actually performed.
        </Alert>
      ) : null}

      {last || fallback ? (
        <section aria-labelledby="last-result-title" className="rounded-lg border border-border">
          <header className="flex items-center justify-between gap-3 border-b border-border px-4 py-2.5">
            <h3 id="last-result-title" className="text-sm font-semibold text-ink">
              Last verification
            </h3>
            {result ? (
              <Badge tone={RESULT_LABELS[result].tone}>{RESULT_LABELS[result].label}</Badge>
            ) : null}
          </header>
          <div className="space-y-3 px-4 py-2">
            {last ? (
              <ul className="divide-y divide-border">
                <CheckRow
                  ok={last.prueba_vida_exitosa}
                  label="Liveness (a live person in front of the camera)"
                  detail={simulatedNote}
                />
                <CheckRow ok={last.documento_valido} label="Credential (authentic and current)" detail={simulatedNote} />
                {/* coincide_identidad covers the identity only (the INE reading
                    matches your details); the face match is part of the overall result. */}
                <CheckRow
                  ok={last.coincide_identidad}
                  label="Identity (your INE matches your details)"
                  detail={simulatedNote}
                />
                {/* The bank does not disclose the exact similarity score; the
                    overall result above already says whether the face matched. */}
                {score !== null && last.umbral_aplicado !== null ? (
                  <CheckRow
                    ok={last.score_similitud !== null && Number(last.score_similitud) >= Number(last.umbral_aplicado)}
                    label="Face match with your INE photo"
                    detail={score}
                  />
                ) : null}
              </ul>
            ) : null}
            {details.errors.length > 0 ? (
              <div>
                <p className="flex items-center gap-1 text-xs font-semibold text-danger">
                  <XCircle className="size-3.5" aria-hidden /> Problems found
                </p>
                <ul className="mt-1 list-disc space-y-0.5 pl-5 text-[13px] text-ink-2">
                  {details.errors.map((e) => (
                    <li key={e}>{e}</li>
                  ))}
                </ul>
              </div>
            ) : null}
            {details.warnings.length > 0 ? (
              <div>
                <p className="flex items-center gap-1 text-xs font-semibold text-warning">
                  <ShieldAlert className="size-3.5" aria-hidden /> Needs a second look
                </p>
                <ul className="mt-1 list-disc space-y-0.5 pl-5 text-[13px] text-ink-2">
                  {details.warnings.map((w) => (
                    <li key={w}>{w}</li>
                  ))}
                </ul>
              </div>
            ) : null}
            {last ? (
              <p className="pb-1 text-xs text-muted">
                {dateTime.format(new Date(last.timestamp))} · provider: {last.proveedor}
              </p>
            ) : null}
          </div>
        </section>
      ) : null}

      <div className="space-y-2">
        {file.estado_kyc === "VERIFICADO" ? (
          <Button type="button" size="lg" className="w-full" onClick={onDashboard}>
            <CheckCircle2 className="size-4" aria-hidden />
            Go to dashboard
          </Button>
        ) : null}
        {file.estado_kyc === "PENDIENTE" && !locked ? (
          canRetry(file) ? (
            <Button type="button" size="lg" className="w-full" onClick={onTryAgain}>
              <RotateCcw className="size-4" aria-hidden />
              Try again
            </Button>
          ) : (
            <Button type="button" size="lg" className="w-full" onClick={onResume}>
              Continue verification
            </Button>
          )
        ) : null}
        {file.estado_kyc !== "VERIFICADO" ? (
          <Link
            href="/dashboard"
            className="flex h-10 items-center justify-center gap-2 rounded-lg text-sm font-medium text-ink-2 transition hover:bg-surface-2 hover:text-ink focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
          >
            <LayoutDashboard className="size-4" aria-hidden />
            View my account
          </Link>
        ) : null}
      </div>
    </div>
  );
}
