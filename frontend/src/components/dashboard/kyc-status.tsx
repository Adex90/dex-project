"use client";

import { useEffect, useState, type ReactNode } from "react";
import Link from "next/link";
import {
  ArrowRight,
  Hourglass,
  Lock,
  ShieldAlert,
  ShieldCheck,
  ShieldOff,
  ShieldX,
  type LucideIcon,
} from "lucide-react";

import { formatDateTime } from "@/components/ui/data";
import { Alert, Badge, cx, type Tone } from "@/components/ui/primitives";
import { ONBOARDING_PATH } from "@/hooks/use-client-home";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { Cliente, EstatusKyc, VistaExpediente } from "@/lib/api/types";
import { ESTADO_KYC_LABELS, describeKycMessage } from "@/lib/kyc";

/**
 * KYC gating on the dashboard. Until the client's identity is VERIFICADO the
 * database itself (row-level security) refuses deposits, transfers,
 * withdrawals and fixed-term investments; these components say so up front
 * and lock the controls instead of letting the customer hit a 422.
 */

const STATUS: Record<EstatusKyc, { label: string; tone: Tone; icon: LucideIcon }> = {
  PENDIENTE: { label: ESTADO_KYC_LABELS.PENDIENTE, tone: "warning", icon: ShieldAlert },
  EN_REVISION: { label: ESTADO_KYC_LABELS.EN_REVISION, tone: "info", icon: Hourglass },
  VERIFICADO: { label: ESTADO_KYC_LABELS.VERIFICADO, tone: "verified", icon: ShieldCheck },
  RECHAZADO: { label: ESTADO_KYC_LABELS.RECHAZADO, tone: "danger", icon: ShieldX },
  // Compliance measure that predates the KYC file (its file reads RECHAZADO).
  SUSPENDIDO: { label: "Suspended", tone: "danger", icon: ShieldOff },
};

/** True when money movements are locked by KYC (the status is known and is not VERIFICADO). */
export function isKycLocked(client: Pick<Cliente, "estatus_kyc"> | null | undefined): boolean {
  return Boolean(client && client.estatus_kyc !== "VERIFICADO");
}

/** The API refused an operation because a party's identity is not verified. */
export function kycBlockCode(error: unknown): "KYC_NO_VERIFICADO" | "DESTINO_KYC_NO_VERIFICADO" | null {
  if (!(error instanceof ApiError)) return null;
  return error.code === "KYC_NO_VERIFICADO" || error.code === "DESTINO_KYC_NO_VERIFICADO" ? error.code : null;
}

const linkClass = "font-medium underline underline-offset-2 hover:no-underline";

/** Identity verification status for the dashboard header. Links to onboarding until verified. */
export function KycHeaderBadge({ status }: { status: EstatusKyc }) {
  const { label, tone, icon: Icon } = STATUS[status];
  const badge = (
    <Badge tone={tone} icon={<Icon className="size-3" aria-hidden />}>
      <span className="sr-only">Identity verification: </span>
      {label}
    </Badge>
  );
  const title = `Identity verification: ${label}`;
  if (status === "VERIFICADO") {
    return (
      <span className="inline-flex" data-testid="kyc-header-badge" data-status={status} title={title}>
        {badge}
      </span>
    );
  }
  return (
    <Link
      href={ONBOARDING_PATH}
      className="inline-flex rounded-full focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
      data-testid="kyc-header-badge"
      data-status={status}
      title={`${title}. Open verification`}
    >
      {badge}
    </Link>
  );
}

const BANNER_TONES: Record<"warning" | "info" | "danger", { box: string; icon: string; cta: string }> = {
  warning: {
    box: "border-warning/35 bg-warning-soft",
    icon: "bg-warning/15 text-warning",
    cta: "bg-accent text-accent-ink shadow-sm hover:brightness-110",
  },
  info: {
    box: "border-info/35 bg-info-soft",
    icon: "bg-info/15 text-info",
    cta: "border border-border-strong bg-surface text-ink hover:bg-surface-2",
  },
  danger: {
    box: "border-danger/35 bg-danger-soft",
    icon: "bg-danger/15 text-danger",
    cta: "border border-border-strong bg-surface text-ink hover:bg-surface-2",
  },
};

interface BannerCopy {
  tone: keyof typeof BANNER_TONES;
  title: string;
  body: string;
  cta: string;
}

function bannerCopy(status: Exclude<EstatusKyc, "VERIFICADO">): BannerCopy {
  switch (status) {
    case "PENDIENTE":
      return {
        tone: "warning",
        title: "Verify your identity to unlock your account",
        body:
          "Deposits, transfers and fixed-term investments are locked until your identity verification is complete. " +
          "It takes a few minutes: your personal details, both sides of your INE voting credential and a short selfie.",
        cta: "Complete verification",
      };
    case "EN_REVISION":
      return {
        tone: "info",
        title: "Your identity is being reviewed",
        body:
          "We received your documents and a compliance officer is reviewing them. Deposits, transfers and " +
          "fixed-term investments unlock as soon as your identity is verified; this page updates on its own.",
        cta: "View verification status",
      };
    case "RECHAZADO":
      return {
        tone: "danger",
        title: "We could not verify your identity",
        body:
          "Deposits, transfers and fixed-term investments remain locked on this account. " +
          "Contact support if you believe this is a mistake.",
        cta: "View verification details",
      };
    case "SUSPENDIDO":
      return {
        tone: "danger",
        title: "Your account is suspended",
        body:
          "Operations on this account are suspended for compliance reasons: deposits, transfers and fixed-term " +
          "investments are locked. Contact support for details.",
        cta: "View verification details",
      };
  }
}

/** First blocking finding of the last biometric result, in English when recognized. */
function lastFailure(file: VistaExpediente): string | null {
  const detail = file.ultimo_resultado?.detalles_fallo.find((d) => d.tipo === "ERROR");
  return detail ? describeKycMessage(detail.mensaje) : null;
}

/** What the KYC file adds to the banner: the reason of a rejection, attempts left, submission time. */
function fileDetails(status: EstatusKyc, file: VistaExpediente | null): ReactNode[] {
  if (!file) return [];
  const lines: ReactNode[] = [];
  if (status === "RECHAZADO" || status === "SUSPENDIDO") {
    const reason = file.motivo_rechazo ? describeKycMessage(file.motivo_rechazo) : lastFailure(file);
    if (reason) {
      lines.push(
        <span key="reason" data-testid="kyc-rejection-reason">
          <span className="font-semibold">Reason:</span> {reason}
        </span>,
      );
    }
  } else if (status === "PENDIENTE") {
    if (file.intentos > 0 && file.ultimo_resultado?.resultado === "RECHAZADO") {
      const reason = lastFailure(file);
      lines.push(
        <span key="retry">
          <span className="font-semibold">Your last attempt was not approved{reason ? ":" : "."}</span>
          {reason ? ` ${reason}` : null}
        </span>,
      );
    }
    if (file.intentos > 0) {
      const left = file.intentos_restantes;
      lines.push(
        <span key="left">
          You have {left} of {file.max_intentos} verification {file.max_intentos === 1 ? "attempt" : "attempts"} left.
        </span>,
      );
    } else if (file.curp) {
      lines.push(<span key="next">Your personal details are saved. Next: photos of your INE and a selfie.</span>);
    }
  } else if (status === "EN_REVISION" && file.enviado_en) {
    lines.push(<span key="sent">Submitted {formatDateTime(file.enviado_en)}.</span>);
  }
  return lines;
}

/**
 * Prominent notice shown while the client is not VERIFICADO: what is locked,
 * why, and the way to unlock it. Reads GET /kyc/expediente for the details
 * (rejection reason, attempts left); the status itself comes from the tablero.
 */
export function KycBanner({ client, token }: { client: Cliente; token: string }) {
  const status = client.estatus_kyc;
  const [file, setFile] = useState<VistaExpediente | null>(null);

  useEffect(() => {
    if (status === "VERIFICADO") return;
    const controller = new AbortController();
    api.kycFile(token, controller.signal).then(
      (f) => setFile(f),
      () => setFile(null), // details are optional; the status is already known
    );
    return () => controller.abort();
  }, [token, status]);

  if (status === "VERIFICADO") return null;
  const copy = bannerCopy(status);
  const tone = BANNER_TONES[copy.tone];
  const Icon = STATUS[status].icon;
  // A file fetched before the status changed would describe the old state.
  const details = fileDetails(status, file && file.estado_kyc === expectedFileState(status) ? file : null);

  return (
    <section
      data-testid="kyc-banner"
      data-status={status}
      aria-labelledby="kyc-banner-title"
      className={cx("rounded-xl border p-4 shadow-sm sm:p-5", tone.box)}
    >
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start">
        <div className={cx("flex size-10 shrink-0 items-center justify-center rounded-lg", tone.icon)}>
          <Icon className="size-5" aria-hidden />
        </div>
        <div className="min-w-0 flex-1 space-y-2">
          <h2 id="kyc-banner-title" className="text-base font-semibold text-ink">
            {copy.title}
          </h2>
          <p className="text-sm leading-relaxed text-ink-2">{copy.body}</p>
          {details.length ? (
            <div className="space-y-1 text-sm leading-relaxed text-ink-2">
              {details.map((line, i) => (
                <p key={i}>{line}</p>
              ))}
            </div>
          ) : null}
          <ul className="flex flex-wrap gap-1.5 pt-1" aria-label="Locked until your identity is verified">
            {["Deposits", "Transfers", "Fixed-term investments"].map((feature) => (
              <li key={feature}>
                <Badge tone="neutral" icon={<Lock className="size-3" aria-hidden />}>
                  {feature}
                </Badge>
              </li>
            ))}
          </ul>
          <p className="text-xs text-muted">
            You can still see your balances and ledger, and collect matured contracts.
          </p>
        </div>
        <Link
          href={ONBOARDING_PATH}
          data-testid="kyc-banner-cta"
          className={cx(
            "inline-flex h-10 shrink-0 items-center justify-center gap-2 self-start rounded-lg px-4 text-sm font-medium transition",
            "focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none",
            tone.cta,
          )}
        >
          {copy.cta} <ArrowRight className="size-4" aria-hidden />
        </Link>
      </div>
    </section>
  );
}

/** State of the KYC file behind each mirrored `estatus_kyc` (SUSPENDIDO keeps a RECHAZADO file). */
function expectedFileState(status: EstatusKyc): VistaExpediente["estado_kyc"] {
  return status === "SUSPENDIDO" ? "RECHAZADO" : status;
}

/**
 * Explanation next to a control that is locked by KYC. Give the control
 * `aria-describedby={id}` so assistive technology reads why.
 */
export function KycLockNote({ id, children }: { id: string; children: ReactNode }) {
  return (
    <p id={id} className="flex items-start gap-1.5 text-xs leading-relaxed text-muted">
      <Lock className="mt-0.5 size-3 shrink-0" aria-hidden />
      <span>
        {children}{" "}
        <Link href={ONBOARDING_PATH} className={cx(linkClass, "text-accent")}>
          Complete verification
        </Link>
      </span>
    </p>
  );
}

/**
 * Error of a money movement. A KYC refusal (422 KYC_NO_VERIFICADO or
 * DESTINO_KYC_NO_VERIFICADO) is explained as such, with a way forward,
 * instead of a bare error.
 */
export function MovementErrorAlert({ error }: { error: unknown }) {
  const code = kycBlockCode(error);
  if (code === "KYC_NO_VERIFICADO") {
    return (
      <Alert tone="warning" title="Identity verification required">
        {describeError(error)}{" "}
        <Link href={ONBOARDING_PATH} className={linkClass}>
          Complete verification
        </Link>
      </Alert>
    );
  }
  if (code === "DESTINO_KYC_NO_VERIFICADO") {
    return (
      <Alert tone="warning" title="Recipient not verified">
        {describeError(error)}
      </Alert>
    );
  }
  return <Alert tone="error">{describeError(error)}</Alert>;
}
