"use client";

import { Fragment, useEffect, useState, type ReactNode, type TextareaHTMLAttributes } from "react";

import { Badge, cx, type Tone } from "@/components/ui/primitives";
import type { EstadoAlertaAml, IsoDateTime, NivelRiesgoAml, PersonaRefAml, Uuid } from "@/lib/api/types";
import {
  LEVEL_LABEL,
  STATE_LABEL,
  daysToDeadline,
  formatMexicoCity,
  formatScore,
  levelTone,
  relativeTime,
  scorePercent,
  slaRemaining,
  type Thresholds,
} from "@/lib/aml";

/** Re-renders every `intervalMs` so countdowns and relative times stay current. */
export function useNow(intervalMs = 30_000): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = setInterval(() => setNow(new Date()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

export function LevelBadge({ level, className }: { level: NivelRiesgoAml; className?: string }) {
  return (
    <Badge tone={levelTone(level)} className={className}>
      {LEVEL_LABEL[level]}
    </Badge>
  );
}

const STATE_TONES: Record<EstadoAlertaAml, Tone> = {
  PENDIENTE: "warning",
  EN_REVISION: "info",
  CONFIRMADO: "danger",
  FALSO_POSITIVO: "verified",
};

export function AlertStateBadge({ estado }: { estado: EstadoAlertaAml }) {
  return <Badge tone={STATE_TONES[estado]}>{STATE_LABEL[estado]}</Badge>;
}

const METER_FILL: Record<NivelRiesgoAml, string> = {
  BAJO: "bg-muted",
  MEDIO: "bg-info",
  ALTO: "bg-warning",
  CRITICO: "bg-danger",
};

/** 0–100 score bar with ticks at the current level thresholds. */
export function ScoreMeter({
  score,
  level,
  thresholds,
  className,
}: {
  score: string;
  level: NivelRiesgoAml;
  thresholds: Thresholds;
  className?: string;
}) {
  const ticks = [thresholds.medio, thresholds.alto, thresholds.critico];
  return (
    <span className={cx("inline-flex items-center gap-2", className)}>
      <span
        role="meter"
        aria-label="Risk score"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={scorePercent(score)}
        aria-valuetext={`${formatScore(score)} of 100, ${LEVEL_LABEL[level]}`}
        className="relative h-1.5 w-16 shrink-0 overflow-hidden rounded-full bg-surface-2 ring-1 ring-border"
      >
        <span className={cx("absolute inset-y-0 left-0 rounded-full", METER_FILL[level])} style={{ width: `${scorePercent(score)}%` }} />
        {ticks.map((t) => (
          <span key={t} className="absolute inset-y-0 w-px bg-ink/50" style={{ left: `${scorePercent(t)}%` }} aria-hidden />
        ))}
      </span>
      <span className="font-mono text-xs font-semibold text-ink tabular-nums">{formatScore(score)}</span>
    </span>
  );
}

/** "You" for the signed-in officer, the name otherwise; `fallback` when nobody (e.g. "System"). */
export function personLabel(person: PersonaRefAml | null, me: Uuid, fallback = "—"): string {
  if (!person) return fallback;
  return person.id === me ? "You" : person.nombre_completo;
}

/** A time in Mexico City, relative or absolute, with the absolute time on hover. */
export function MxTime({
  at,
  relative = false,
  style = "datetime",
  now,
}: {
  at: IsoDateTime;
  relative?: boolean;
  style?: "datetime" | "date" | "time";
  now?: Date;
}) {
  const absolute = formatMexicoCity(at, style);
  return (
    <time dateTime={at} title={`${formatMexicoCity(at, "datetime")} (Mexico City)`} className="whitespace-nowrap">
      {relative ? relativeTime(at, now) : absolute}
    </time>
  );
}

/** UIF report deadline in Mexico City calendar days: red under 7 days, "Overdue" below 0. */
export function DeadlineText({ fechaLimite, now }: { fechaLimite: IsoDateTime; now: Date }) {
  const days = daysToDeadline(fechaLimite, now);
  const text = days < 0 ? "Overdue" : days === 0 ? "Due today" : `${days} ${days === 1 ? "day" : "days"}`;
  return (
    <span
      className={cx("whitespace-nowrap tabular-nums", days < 7 ? "font-semibold text-danger" : "text-ink-2")}
      title={`Report due ${formatMexicoCity(fechaLimite, "datetime")} (Mexico City)`}
    >
      {text}
    </span>
  );
}

/** Freeze review countdown ("12 h left", "Overdue by 5 h"). */
export function SlaText({ slaVenceEn, now }: { slaVenceEn: IsoDateTime; now: Date }) {
  const { overdue, hours } = slaRemaining(slaVenceEn, now);
  const text = overdue ? `Overdue by ${hours < 1 ? "< 1" : hours} h` : hours < 1 ? "< 1 h left" : `${hours} h left`;
  return (
    <span
      className={cx("whitespace-nowrap tabular-nums", overdue ? "font-semibold text-danger" : hours < 12 ? "text-warning" : "text-ink-2")}
      title={`Review due ${formatMexicoCity(slaVenceEn, "datetime")} (Mexico City)`}
    >
      {text}
    </span>
  );
}

export function Textarea({ className, invalid, ...rest }: TextareaHTMLAttributes<HTMLTextAreaElement> & { invalid?: boolean }) {
  return (
    <textarea
      className={cx(
        "min-h-28 w-full rounded-lg border bg-surface px-3 py-2 text-sm leading-relaxed text-ink placeholder:text-muted/70 transition",
        "focus:border-accent focus:ring-2 focus:ring-accent/25 focus:outline-none",
        invalid ? "border-danger" : "border-border-strong",
        className,
      )}
      aria-invalid={invalid || undefined}
      {...rest}
    />
  );
}

/** Free-text field (dictamen, justification, comment) with a live character counter. */
export function NarrativeField({
  id,
  label,
  value,
  onChange,
  min,
  max,
  hint,
  error,
  placeholder,
  disabled,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  min: number;
  max: number;
  hint?: ReactNode;
  error?: string | null;
  placeholder?: string;
  disabled?: boolean;
}) {
  const count = [...value.trim()].length;
  const short = count < min;
  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between gap-3">
        <label htmlFor={id} className="block text-xs font-medium text-ink-2">
          {label}
        </label>
        <span
          id={`${id}-count`}
          className={cx("text-[11px] tabular-nums", count > max ? "font-semibold text-danger" : short ? "text-muted" : "text-verified")}
          aria-live="polite"
        >
          {count} / {max}
          {short ? <span className="sr-only"> characters; at least {min} required</span> : <span className="sr-only"> characters</span>}
        </span>
      </div>
      <Textarea
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        invalid={Boolean(error)}
        placeholder={placeholder}
        disabled={disabled}
        aria-describedby={[error ? `${id}-error` : hint ? `${id}-hint` : null, `${id}-count`].filter(Boolean).join(" ")}
      />
      {error ? (
        <p id={`${id}-error`} className="text-xs text-danger">
          {error}
        </p>
      ) : hint ? (
        <p id={`${id}-hint`} className="text-xs text-muted">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

/** A titled block of the case file. */
export function Section({
  title,
  icon,
  action,
  children,
  className,
}: {
  title: string;
  icon?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cx("space-y-3 border-b border-border px-5 py-4 last:border-b-0", className)}>
      <div className="flex items-center justify-between gap-3">
        <h3 className="flex items-center gap-2 text-[11px] font-semibold tracking-[0.12em] text-muted uppercase">
          {icon}
          {title}
        </h3>
        {action}
      </div>
      {children}
    </section>
  );
}

/** Label/value grid. Values that are null render as a dash. */
export function Facts({ rows, className }: { rows: [string, ReactNode][]; className?: string }) {
  return (
    <dl className={cx("grid gap-x-6 gap-y-1.5 text-[13px] sm:grid-cols-[max-content_1fr]", className)}>
      {rows.map(([label, value]) => (
        <Fragment key={label}>
          <dt className="text-muted">{label}</dt>
          <dd className="min-w-0 break-words text-ink">{value ?? <span className="text-muted">—</span>}</dd>
        </Fragment>
      ))}
    </dl>
  );
}

/** Radio option with a label and an optional explanation. */
export function Choice({
  name,
  value,
  checked,
  onChange,
  label,
  hint,
  disabled,
}: {
  name: string;
  value: string;
  checked: boolean;
  onChange: (value: string) => void;
  label: string;
  hint?: ReactNode;
  disabled?: boolean;
}) {
  return (
    <label
      className={cx(
        "flex cursor-pointer items-start gap-2.5 rounded-lg border px-3 py-2 text-sm transition",
        checked ? "border-accent bg-accent-soft/60" : "border-border hover:bg-surface-2",
        disabled && "cursor-not-allowed opacity-60",
      )}
    >
      <input
        type="radio"
        name={name}
        value={value}
        checked={checked}
        disabled={disabled}
        onChange={() => onChange(value)}
        className="mt-0.5 accent-accent"
      />
      <span className="min-w-0">
        <span className="block font-medium text-ink">{label}</span>
        {hint ? <span className="block text-xs leading-relaxed text-muted">{hint}</span> : null}
      </span>
    </label>
  );
}

export function Checkbox({
  id,
  checked,
  onChange,
  label,
  hint,
}: {
  id: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: string;
  hint?: ReactNode;
}) {
  return (
    <div className="flex items-start gap-2.5 text-sm">
      <input id={id} type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} className="mt-0.5 accent-accent" />
      <label htmlFor={id} className="min-w-0 cursor-pointer">
        <span className="block font-medium text-ink">{label}</span>
        {hint ? <span className="block text-xs leading-relaxed text-muted">{hint}</span> : null}
      </label>
    </div>
  );
}
