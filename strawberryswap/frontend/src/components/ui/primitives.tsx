"use client";

import { useId, useRef, type KeyboardEvent, type ReactNode } from "react";
import { IconChevron } from "./icons";

/* ------------------------------------------------------------------------------------------------ surfaces */

/** Floating glass card (rounded-3xl, backdrop blur, soft glow). */
export function Card({ children, className = "", as: Tag = "section", ...rest }: { children: ReactNode; className?: string; as?: "section" | "div" | "article" | "aside" } & Record<string, unknown>) {
  return (
    <Tag className={`card ${className}`} {...rest}>
      {children}
    </Tag>
  );
}

export function CardHeader({ icon, title, subtitle, right }: { icon?: ReactNode; title: ReactNode; subtitle?: ReactNode; right?: ReactNode }) {
  return (
    <header className="mb-4 flex items-start justify-between gap-3">
      <div className="flex min-w-0 items-center gap-3">
        {icon ? <span className="grid h-10 w-10 shrink-0 place-items-center rounded-2xl bg-plum-700/80 text-fuchsia shadow-inset">{icon}</span> : null}
        <div className="min-w-0">
          <h2 className="card-title truncate">{title}</h2>
          {subtitle ? <p className="text-sm text-lavender">{subtitle}</p> : null}
        </div>
      </div>
      {right ? <div className="flex shrink-0 items-center gap-2">{right}</div> : null}
    </header>
  );
}

export function PageHeader({ title, subtitle, right }: { title: string; subtitle: ReactNode; right?: ReactNode }) {
  return (
    <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 className="text-3xl font-extrabold tracking-tight sm:text-4xl">{title}</h1>
        <p className="mt-1 max-w-2xl text-lavender">{subtitle}</p>
      </div>
      {right}
    </div>
  );
}

/* ------------------------------------------------------------------------------------------------ async states */

/** Plum/magenta skeleton block with a glowing pulse. Purely decorative: wrap regions in <Loading> for screen readers. */
export function Skeleton({ className = "h-5 w-24" }: { className?: string }) {
  return <span aria-hidden className={`skeleton ${className}`} />;
}

/** Marks a region as loading for assistive tech while its skeletons render. */
export function Loading({ label, children, className = "" }: { label: string; children: ReactNode; className?: string }) {
  return (
    <div aria-busy="true" className={className}>
      <span className="sr-only">{label}</span>
      {children}
    </div>
  );
}

/** Content arriving after a skeleton fades in instead of popping. Size the skeleton like the content to avoid layout shift. */
export function Reveal({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <div className={`animate-fade-in ${className}`}>{children}</div>;
}

/* ------------------------------------------------------------------------------------------------ figures */

type Tone = "default" | "mint" | "berry" | "blush" | "fuchsia";
const toneText: Record<Tone, string> = { default: "text-ink", mint: "text-mint", berry: "text-berry-soft", blush: "text-blush", fuchsia: "text-fuchsia" };

/**
 * One figure in a mini-card. `value === undefined` while loading draws a skeleton of the same height (no layout shift).
 */
export function StatTile({ label, value, sub, tone = "default", loading = false }: { label: ReactNode; value: ReactNode; sub?: ReactNode; tone?: Tone; loading?: boolean }) {
  return (
    <div className="card-inset min-w-0">
      <div className="eyebrow">{label}</div>
      <div className="mt-1 h-8">
        {loading ? (
          <Skeleton className="mt-1 h-6 w-28" />
        ) : (
          <Reveal>
            <span className={`num block truncate text-xl font-semibold leading-8 ${toneText[tone]}`}>{value}</span>
          </Reveal>
        )}
      </div>
      <div className="mt-0.5 h-4 truncate text-xs text-haze">{loading ? null : sub}</div>
    </div>
  );
}

/** Label-left / value-right row. */
export function Row({ label, children }: { label: ReactNode; children: ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-4 py-1.5 text-sm">
      <span className="shrink-0 whitespace-nowrap text-lavender">{label}</span>
      <span className="min-w-0 truncate text-right text-ink">{children}</span>
    </div>
  );
}

/* ------------------------------------------------------------------------------------------------ notices */

export function Notice({ tone, children, className = "" }: { tone: "warn" | "err" | "ok" | "info"; children: ReactNode; className?: string }) {
  const style = {
    warn: "border-blush/30 bg-blush/10 text-blush",
    err: "border-berry/40 bg-berry/10 text-berry-soft",
    ok: "border-mint/30 bg-mint/10 text-mint",
    info: "border-lavender/15 bg-plum-700/50 text-lavender",
  }[tone];
  return (
    <p role={tone === "err" ? "alert" : "status"} className={`break-words rounded-2xl border px-4 py-3 text-sm leading-snug ${style} ${className}`}>
      {children}
    </p>
  );
}

/* ------------------------------------------------------------------------------------------------ tabs / segmented */

/**
 * Segmented pills. With `panelsId` it is an ARIA tablist (arrow keys move between tabs); without, a plain button group.
 */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
  label,
  idPrefix,
  className = "",
}: {
  options: readonly { value: T; label: ReactNode }[];
  value: T;
  onChange: (v: T) => void;
  label: string;
  idPrefix?: string;
  className?: string;
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKey = (e: KeyboardEvent, i: number) => {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    e.preventDefault();
    const next = (i + (e.key === "ArrowRight" ? 1 : options.length - 1)) % options.length;
    const opt = options.at(next);
    if (!opt) return;
    onChange(opt.value);
    refs.current.at(next)?.focus();
  };
  return (
    <div role={idPrefix ? "tablist" : "group"} aria-label={label} className={`inline-flex rounded-full bg-plum-950/70 p-1 shadow-inset ${className}`}>
      {options.map((o, i) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            ref={(el) => {
              refs.current[i] = el;
            }}
            type="button"
            role={idPrefix ? "tab" : undefined}
            id={idPrefix ? `${idPrefix}-tab-${o.value}` : undefined}
            aria-controls={idPrefix ? `${idPrefix}-panel-${o.value}` : undefined}
            aria-selected={idPrefix ? on : undefined}
            aria-pressed={idPrefix ? undefined : on}
            tabIndex={idPrefix && !on ? -1 : 0}
            onKeyDown={idPrefix ? (e) => onKey(e, i) : undefined}
            onClick={() => onChange(o.value)}
            className={`whitespace-nowrap rounded-full px-3 py-2 text-xs font-semibold transition duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia sm:px-4 sm:text-sm ${
              on ? "bg-gradient-to-r from-berry-deep to-fuchsia-deep text-white shadow-glow-fuchsia" : "text-lavender hover:text-ink"
            }`}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

export function TabPanel({ idPrefix, value, active, children }: { idPrefix: string; value: string; active: boolean; children: ReactNode }) {
  if (!active) return null;
  return (
    <div role="tabpanel" id={`${idPrefix}-panel-${value}`} aria-labelledby={`${idPrefix}-tab-${value}`} className="animate-fade-in">
      {children}
    </div>
  );
}

/* ------------------------------------------------------------------------------------------------ tokens */

const PALETTE: [string, string][] = [
  ["#ff2a85", "#6b33ad"],
  ["#00c46a", "#0b0a2c"],
  ["#ff8fc8", "#c8006a"],
  ["#6b33ad", "#ff1a53"],
];

function hash(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return h;
}

/** Circular token icon drawn from the on-chain symbol (no logo files, no third-party image hosts). */
export function TokenIcon({ symbol, size = 28 }: { symbol: string | undefined; size?: number }) {
  const s = (symbol ?? "?").toUpperCase();
  const gid = `tok-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const isBerry = s.includes("BERRY");
  const isUsd = s.startsWith("USD");
  const [a, b] = PALETTE.at(hash(s) % PALETTE.length) ?? ["#ff2a85", "#6b33ad"];
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden focusable={false} className="shrink-0 rounded-full shadow-[0_0_10px_-2px_rgba(255,42,133,0.6)]">
      <defs>
        <linearGradient id={gid} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" stopColor={isBerry ? "#ff2a62" : isUsd ? "#00c46a" : a} />
          <stop offset="100%" stopColor={isBerry ? "#7d0026" : isUsd ? "#0b0a2c" : b} />
        </linearGradient>
      </defs>
      <circle cx="16" cy="16" r="16" fill={`url(#${gid})`} />
      {isBerry ? (
        <g>
          <path d="M16 11.5c4.4-1.2 8 1 8 5.2 0 4.6-4.2 8.6-8 10.3-3.8-1.7-8-5.7-8-10.3 0-4.2 3.6-6.4 8-5.2Z" fill="#ffd1e3" opacity="0.95" />
          <path d="M16 12c-1.6-2.6-4.4-3.2-6-2 2 .8 3.8 1.6 6 2Zm0 0c1.6-2.6 4.4-3.2 6-2-2 .8-3.8 1.6-6 2Zm0 0c-.6-2.6 0-4.6 1.2-5.6" stroke="#00ff88" strokeWidth="1.4" fill="#00ff88" strokeLinecap="round" />
          <g fill="#e0003d">
            <circle cx="13.5" cy="16" r="0.9" />
            <circle cx="18.5" cy="16" r="0.9" />
            <circle cx="16" cy="19.5" r="0.9" />
            <circle cx="12.8" cy="20.5" r="0.8" />
            <circle cx="19.2" cy="20.5" r="0.8" />
          </g>
        </g>
      ) : (
        <text x="16" y="20.5" textAnchor="middle" fontSize={isUsd ? 14 : 11} fontWeight="800" fill="#f7eefe" fontFamily="inherit">
          {isUsd ? "$" : s.slice(0, 2)}
        </text>
      )}
    </svg>
  );
}

/** A native <select> dressed as a token pill: icon left, caret right. Native on purpose (keyboard, screen readers, tests). */
export function TokenSelect<V extends string | number>({
  value,
  options,
  onChange,
  ariaLabel,
  disabled,
}: {
  value: V;
  options: readonly { value: V; symbol: string }[];
  onChange: (v: V) => void;
  ariaLabel: string;
  disabled?: boolean;
}) {
  const current = options.find((o) => o.value === value);
  return (
    <span className="relative inline-flex shrink-0 items-center">
      <span className="pointer-events-none absolute left-1.5">
        <TokenIcon symbol={current?.symbol} size={30} />
      </span>
      <select
        className="token-select"
        value={String(value)}
        aria-label={ariaLabel}
        disabled={disabled}
        onChange={(e) => {
          const picked = options.find((o) => String(o.value) === e.target.value);
          if (picked) onChange(picked.value);
        }}
      >
        {options.map((o) => (
          <option key={String(o.value)} value={String(o.value)} className="bg-plum-900 text-ink">
            {o.symbol}
          </option>
        ))}
      </select>
      <IconChevron className="pointer-events-none absolute right-3 h-4 w-4 text-lavender" />
    </span>
  );
}

/* ------------------------------------------------------------------------------------------------ sparkline */

/**
 * Tiny line chart. Green when the series ends at or above its start, red below. `pending` draws a dashed baseline:
 * used when there is no feed, so the card never shows invented numbers.
 */
export function Sparkline({ points, pending = false, className = "h-10 w-full" }: { points: number[]; pending?: boolean; className?: string }) {
  const gid = `spark-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const W = 120;
  const H = 40;
  if (pending || points.length === 0) {
    return (
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className={className} aria-hidden focusable={false}>
        <path d={`M2 ${H / 2}H${W - 2}`} stroke="#8f78b3" strokeOpacity="0.6" strokeWidth="1.5" strokeDasharray="4 5" fill="none" vectorEffect="non-scaling-stroke" />
      </svg>
    );
  }
  const series = points.length === 1 ? [points[0]!, points[0]!] : points;
  const lo = Math.min(...series);
  const hi = Math.max(...series);
  const span = hi - lo || 1;
  const x = (i: number) => 2 + (i / (series.length - 1)) * (W - 4);
  const y = (v: number) => (hi === lo ? H / 2 : 4 + (1 - (v - lo) / span) * (H - 8));
  const line = series.map((v, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(2)} ${y(v).toFixed(2)}`).join("");
  const up = (series.at(-1) ?? 0) >= (series.at(0) ?? 0);
  const color = up ? "#00ff88" : "#ff1a53";
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className={className} aria-hidden focusable={false}>
      <defs>
        <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.35" />
          <stop offset="100%" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={`${line}L${x(series.length - 1).toFixed(2)} ${H}L${x(0).toFixed(2)} ${H}Z`} fill={`url(#${gid})`} />
      <path d={line} stroke={color} strokeWidth="2" fill="none" strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}
