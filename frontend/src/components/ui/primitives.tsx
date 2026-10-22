import type { ButtonHTMLAttributes, HTMLAttributes, InputHTMLAttributes, ReactNode, Ref } from "react";
import { AlertTriangle, CheckCircle2, Info, Loader2, XCircle } from "lucide-react";

export function cx(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(" ");
}

// ---------------------------------------------------------------------------
// Card
// ---------------------------------------------------------------------------

export function Card({ className, children, ...rest }: HTMLAttributes<HTMLElement>) {
  return (
    <section className={cx("rounded-xl border border-border bg-surface shadow-sm", className)} {...rest}>
      {children}
    </section>
  );
}

export function CardHeader({
  icon,
  eyebrow,
  title,
  action,
  className,
}: {
  icon?: ReactNode;
  eyebrow?: string;
  title: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  return (
    <header className={cx("flex items-start justify-between gap-4 border-b border-border px-5 py-4", className)}>
      <div className="flex min-w-0 items-start gap-3">
        {icon ? (
          <div className="mt-0.5 flex size-9 shrink-0 items-center justify-center rounded-lg bg-surface-2 text-accent">
            {icon}
          </div>
        ) : null}
        <div className="min-w-0">
          {eyebrow ? (
            <p className="text-[11px] font-semibold tracking-[0.12em] text-muted uppercase">{eyebrow}</p>
          ) : null}
          <h2 className="truncate text-base font-semibold text-ink">{title}</h2>
        </div>
      </div>
      {action ? <div className="shrink-0">{action}</div> : null}
    </header>
  );
}

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

const BUTTON_VARIANTS: Record<ButtonVariant, string> = {
  primary: "bg-accent text-accent-ink hover:brightness-110 shadow-sm",
  secondary: "border border-border-strong bg-surface text-ink hover:bg-surface-2",
  ghost: "text-ink-2 hover:bg-surface-2 hover:text-ink",
  danger: "bg-danger text-white hover:brightness-110",
};

export function Button({
  variant = "primary",
  size = "md",
  loading = false,
  className,
  children,
  disabled,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  ref?: Ref<HTMLButtonElement>;
  variant?: ButtonVariant;
  size?: "sm" | "md" | "lg";
  loading?: boolean;
}) {
  const sizes = { sm: "h-8 px-3 text-xs", md: "h-10 px-4 text-sm", lg: "h-11 px-5 text-sm" };
  return (
    <button
      className={cx(
        "inline-flex items-center justify-center gap-2 rounded-lg font-medium transition",
        "focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none",
        "disabled:cursor-not-allowed disabled:opacity-50",
        BUTTON_VARIANTS[variant],
        sizes[size],
        className,
      )}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
      {children}
    </button>
  );
}

// ---------------------------------------------------------------------------
// Badge
// ---------------------------------------------------------------------------

export type Tone = "verified" | "accent" | "info" | "warning" | "danger" | "neutral";

const TONES: Record<Tone, string> = {
  verified: "bg-verified-soft text-verified border-verified/25",
  accent: "bg-accent-soft text-accent border-accent/25",
  info: "bg-info-soft text-info border-info/25",
  warning: "bg-warning-soft text-warning border-warning/25",
  danger: "bg-danger-soft text-danger border-danger/25",
  neutral: "bg-surface-2 text-ink-2 border-border",
};

export function Badge({
  tone = "neutral",
  icon,
  children,
  className,
}: {
  tone?: Tone;
  icon?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cx(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-semibold whitespace-nowrap",
        TONES[tone],
        className,
      )}
    >
      {icon}
      {children}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Form fields
// ---------------------------------------------------------------------------

/** The aria-describedby of the input inside a `Field`: its error, else its hint. */
export function fieldDescribedBy(htmlFor: string, error?: string | null, hint?: ReactNode): string | undefined {
  if (error) return `${htmlFor}-error`;
  return hint ? `${htmlFor}-hint` : undefined;
}

export function Field({
  label,
  htmlFor,
  hint,
  error,
  children,
}: {
  label: string;
  htmlFor: string;
  hint?: ReactNode;
  error?: string | null;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label htmlFor={htmlFor} className="block text-xs font-medium text-ink-2">
        {label}
      </label>
      {children}
      {error ? (
        <p id={`${htmlFor}-error`} className="text-xs text-danger">
          {error}
        </p>
      ) : hint ? (
        <p id={`${htmlFor}-hint`} className="text-xs text-muted">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export function Input({ className, invalid, ...rest }: InputHTMLAttributes<HTMLInputElement> & { invalid?: boolean }) {
  return (
    <input
      className={cx(
        "h-10 w-full rounded-lg border bg-surface px-3 text-sm text-ink placeholder:text-muted/70 transition",
        "focus:border-accent focus:ring-2 focus:ring-accent/25 focus:outline-none",
        invalid ? "border-danger" : "border-border-strong",
        className,
      )}
      aria-invalid={invalid || undefined}
      {...rest}
    />
  );
}

// ---------------------------------------------------------------------------
// Alerts, skeletons, misc
// ---------------------------------------------------------------------------

const ALERT_ICONS = {
  error: XCircle,
  success: CheckCircle2,
  info: Info,
  warning: AlertTriangle,
};

const ALERT_TONES = {
  error: "border-danger/30 bg-danger-soft text-danger",
  success: "border-verified/30 bg-verified-soft text-verified",
  info: "border-info/30 bg-info-soft text-info",
  warning: "border-warning/30 bg-warning-soft text-warning",
};

export function Alert({
  tone,
  title,
  children,
  className,
}: {
  tone: keyof typeof ALERT_ICONS;
  title?: string;
  children?: ReactNode;
  className?: string;
}) {
  const Icon = ALERT_ICONS[tone];
  return (
    <div
      role={tone === "error" ? "alert" : "status"}
      className={cx("flex gap-2.5 rounded-lg border px-3 py-2.5 text-sm", ALERT_TONES[tone], className)}
    >
      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden />
      <div className="min-w-0 space-y-0.5">
        {title ? <p className="font-semibold">{title}</p> : null}
        {/* No opacity: the warning tone on its soft background needs full strength for 4.5:1. */}
        {children ? <div className="text-[13px] leading-relaxed">{children}</div> : null}
      </div>
    </div>
  );
}

export function Skeleton({ className }: { className?: string }) {
  return <div className={cx("animate-pulse rounded-md bg-surface-2", className)} aria-hidden />;
}

export function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-[11px] font-medium tracking-wide text-muted uppercase">{label}</dt>
      <dd className="mt-1 truncate text-sm font-semibold text-ink tabular-nums">{value}</dd>
      {hint ? <dd className="mt-0.5 text-xs text-muted">{hint}</dd> : null}
    </div>
  );
}
