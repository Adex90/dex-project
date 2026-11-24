"use client";

import {
  FileSignature,
  History,
  KeyRound,
  LogIn,
  LogOut,
  ShieldAlert,
  ShieldCheck,
  Timer,
  Users,
  type LucideIcon,
} from "lucide-react";

import { Card, CardHeader, Skeleton, cx } from "@/components/ui/primitives";
import { describeEvent, formatWhen, timeAgo, type EventTone } from "@/lib/security";
import type { SecurityEvent } from "@/types/security";

/*
 * Recent security activity (roadmap item 20): sign-ins and failed attempts,
 * sign-outs and revoked devices, password and two-step changes, the
 * inactivity timeout, every new version of the legal beneficiaries, every
 * W-8BEN certification, and every "Confirm it's you" before a withdrawal or a
 * change of beneficiaries, passed or failed. The server keeps it in an
 * append-only log: the database refuses to edit or delete an entry. This
 * shows the latest 50.
 */

const ICONS: Record<string, LucideIcon> = {
  INICIO_SESION: LogIn,
  INICIO_FALLIDO: ShieldAlert,
  CUENTA_BLOQUEADA: ShieldAlert,
  SESION_CERRADA: LogOut,
  SESION_REVOCADA: LogOut,
  SESIONES_REVOCADAS: LogOut,
  CONTRASENA_CAMBIADA: KeyRound,
  CAMBIO_CONTRASENA_RECHAZADO: ShieldAlert,
  RESTABLECIMIENTO_SOLICITADO: KeyRound,
  CONTRASENA_RESTABLECIDA: KeyRound,
  PREFERENCIAS_ACTUALIZADAS: Timer,
  BENEFICIARIOS_DESIGNADOS: Users,
  W8BEN_CERTIFICADO: FileSignature,
  STEP_UP_VERIFICADO: ShieldCheck,
  STEP_UP_FALLIDO: ShieldAlert,
  MFA_CODIGO_RECHAZADO: ShieldAlert,
};

const TONES: Record<EventTone, string> = {
  neutral: "border-border bg-surface-2 text-ink-2",
  accent: "border-synthetic-yellow/40 bg-synthetic-yellow/10 text-synthetic-yellow",
  warning: "border-warning/40 bg-warning-soft text-warning",
  danger: "border-danger/40 bg-danger-soft text-danger",
};

export function SecurityLog({ events }: { events: SecurityEvent[] | null }) {
  return (
    <Card data-testid="security-log">
      <CardHeader
        icon={<History className="size-[18px]" aria-hidden />}
        eyebrow="Audit trail"
        title="Recent security activity"
      />
      {events === null ? (
        <div className="space-y-3 p-5" aria-busy="true" aria-label="Loading your security activity">
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
        </div>
      ) : events.length === 0 ? (
        <p className="px-5 py-6 text-sm text-muted">No security activity yet.</p>
      ) : (
        <ol className="divide-y divide-border" aria-label="Security activity, newest first">
          {events.map((event) => {
            const words = describeEvent(event);
            const Icon = ICONS[event.kind] ?? ShieldCheck;
            return (
              <li
                key={event.id}
                className="flex items-start gap-3 px-5 py-3"
                data-testid="security-event"
                data-kind={event.kind}
              >
                <div
                  className={cx("mt-0.5 flex size-8 shrink-0 items-center justify-center border", TONES[words.tone])}
                >
                  <Icon className="size-4" aria-hidden />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
                    <p className="text-sm font-medium text-ink" data-testid="security-event-title">
                      {words.title}
                    </p>
                    <time dateTime={event.at} title={formatWhen(event.at)} className="text-xs text-muted tabular-nums">
                      {timeAgo(event.at)}
                    </time>
                  </div>
                  {words.detail ? <p className="truncate text-xs text-ink-2">{words.detail}</p> : null}
                  <p className="text-[11px] text-muted tabular-nums">
                    {formatWhen(event.at)}
                    {event.ip ? ` · IP ${event.ip}` : ""}
                    {event.fromThisSession ? (
                      <span className="font-semibold text-synthetic-yellow"> · This device</span>
                    ) : null}
                  </p>
                </div>
              </li>
            );
          })}
        </ol>
      )}
      <p className="border-t border-border px-5 py-3 text-[11px] leading-relaxed text-muted">
        This log is append-only: entries cannot be edited or deleted.
      </p>
    </Card>
  );
}
