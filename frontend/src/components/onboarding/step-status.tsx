"use client";

import Link from "next/link";
import { CheckCircle2, Clock, LayoutDashboard, Loader2, RefreshCw, RotateCcw, ShieldAlert, ShieldCheck, XCircle } from "lucide-react";

import { HashText } from "@/components/ui/data";
import { Alert, Badge, Button, Stat, cx } from "@/components/ui/primitives";
import type { EstadoKyc, ResultadoKyc, ResultadoVerificacion, VistaExpediente } from "@/lib/api/types";
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

/** The prominent KYC state badge (stable test hook: data-testid + data-status). */
export function KycStatusBadge({ estado }: { estado: EstadoKyc }) {
  const Icon = BADGE_ICONS[estado];
  return (
    <span
      data-testid="kyc-status-badge"
      data-status={estado}
      className={cx(
        "inline-flex items-center gap-2 rounded-full border px-4 py-1.5 text-sm font-semibold",
        BADGE_TONES[STATUS_TONES[estado]],
      )}
    >
      <Icon className={cx("size-4", estado === "EN_REVISION" && "animate-spin [animation-duration:2.5s]")} aria-hidden />
      {ESTADO_KYC_LABELS[estado]}
    </span>
  );
}

/** Step 4: state of the file, attempts, last result and what to do next. */
export function StepStatus({
  file,
  immediate,
  polling,
  onTryAgain,
  onResume,
  onDashboard,
}: {
  file: VistaExpediente;
  /** Result returned by POST /kyc/verificar in this session (shown if the file has no result yet). */
  immediate: ResultadoVerificacion | null;
  polling: { active: boolean; checking: boolean; lastChecked: Date | null; onCheckNow: () => void };
  onTryAgain: () => void;
  onResume: () => void;
  onDashboard: () => void;
}) {
  const headline = statusHeadline(file);
  const last = file.ultimo_resultado;
  const fallback = !last && immediate ? immediate : null;
  const details = splitDetails(last ? last.detalles_fallo : fallback?.detalles);
  const score = formatPercent(last ? last.score_similitud : fallback?.score_similitud);
  const threshold = last && last.umbral_aplicado !== null ? formatPercent(last.umbral_aplicado, 0) : null;
  const result = last?.resultado ?? fallback?.resultado ?? null;
  const awaitingProvider =
    file.estado_kyc === "EN_REVISION" && (!last || (file.verificacion_en_curso !== null && last.verificacion_id !== file.verificacion_en_curso));

  return (
    <div className="space-y-5">
      <div className="space-y-3 text-center" aria-live="polite" aria-atomic="true">
        <KycStatusBadge estado={file.estado_kyc} />
        <div className="space-y-1">
          <p className="text-base font-semibold text-ink">{headline.title}</p>
          <p className="text-sm text-muted">{headline.body}</p>
        </div>
      </div>

      <dl className="grid grid-cols-3 gap-3 rounded-lg border border-border bg-surface-2/60 px-4 py-3">
        <Stat label="Attempts used" value={`${file.intentos} of ${file.max_intentos}`} />
        <Stat label="Remaining" value={file.intentos_restantes} />
        <Stat label="Face match" value={score ?? "—"} hint={threshold ? `Minimum ${threshold}` : undefined} />
      </dl>

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
            Only compliance can reopen a rejected file, which also restores your attempts. Write to them from the email
            linked to your account and quote this reference:
          </p>
          <HashText value={file.cliente_id} head={8} tail={4} label="reference" className="mt-1.5 text-danger" />
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
                <CheckRow ok={last.prueba_vida_exitosa} label="Liveness (a live person in front of the camera)" />
                <CheckRow ok={last.documento_valido} label="Credential (authentic and current)" />
                <CheckRow ok={last.coincide_identidad} label="Identity data matches your file" />
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
        {file.estado_kyc === "PENDIENTE" ? (
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
