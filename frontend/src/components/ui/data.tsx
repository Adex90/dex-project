"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";

import { formatMoney } from "@/lib/decimal";
import { cx } from "./primitives";

/** Amount with currency suffix; exact (BigInt-based) formatting. */
export function Money({
  value,
  decimals = 2,
  sign,
  className,
  currencyClassName,
}: {
  value: string;
  decimals?: number;
  /** Prefix "+" or "−" explicitly (ledger rows). */
  sign?: "+" | "-";
  className?: string;
  currencyClassName?: string;
}) {
  return (
    <span className={cx("tabular-nums", className)}>
      {sign === "-" ? "−" : sign === "+" ? "+" : ""}
      {formatMoney(value, decimals)}
      <span className={cx("ml-1 text-[0.7em] font-medium text-muted", currencyClassName)}>MXN</span>
    </span>
  );
}

export function truncateHash(hash: string, head = 10, tail = 6): string {
  return hash.length <= head + tail + 1 ? hash : `${hash.slice(0, head)}…${hash.slice(-tail)}`;
}

/** Monospace hash, truncated, with the full value on hover and a copy button. */
export function HashText({
  value,
  head = 10,
  tail = 6,
  label = "hash",
  className,
}: {
  value: string;
  head?: number;
  tail?: number;
  label?: string;
  className?: string;
}) {
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
    <span className={cx("inline-flex max-w-full items-center gap-1 font-mono text-xs", className)}>
      <span className="truncate" title={value}>
        {truncateHash(value, head, tail)}
      </span>
      <button
        type="button"
        onClick={copy}
        className="shrink-0 rounded p-0.5 text-muted transition hover:bg-surface-2 hover:text-ink focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
        aria-label={copied ? `${label} copied` : `Copy ${label}`}
        title={copied ? "Copied" : `Copy ${label}`}
      >
        {copied ? <Check className="size-3 text-verified" aria-hidden /> : <Copy className="size-3" aria-hidden />}
      </button>
    </span>
  );
}

const dateTime = new Intl.DateTimeFormat("en-US", {
  year: "numeric",
  month: "short",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
});
const dateOnly = new Intl.DateTimeFormat("en-US", { year: "numeric", month: "short", day: "2-digit", timeZone: "UTC" });

export function formatDateTime(iso: string): string {
  return dateTime.format(new Date(iso));
}

/** "YYYY-MM-DD" (a calendar date, no time zone shift). */
export function formatDate(isoDate: string): string {
  return dateOnly.format(new Date(`${isoDate}T00:00:00Z`));
}
