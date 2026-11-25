"use client";

import { useEffect, useRef, useState } from "react";
import { ArrowDownLeft, ArrowUpRight, Check, Copy } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { writeToClipboard } from "@/lib/clipboard";
import { CLARIFICATION_STATUS_LABELS, transferStatusLabel } from "@/lib/transfers";
import type { ClarificationStatus, TrackedTransfer } from "@/types/transfers";

/*
 * Pieces the Transfers page shares between its list, the inspector and the
 * clarifications: status badges, the direction mark and a copy button.
 *
 * Sovereign Gold: square hairline badges. A settled transfer is gold; one on
 * its way is zinc with a gold pulse; one that came back is orange (the money
 * went back where it came from: worth a look, never an error). Clarifications:
 * open in gold outline, in review in gold, resolved in green, declined in zinc.
 */

const BADGE =
  "inline-flex items-center gap-1.5 border px-1.5 py-px text-[10px] font-semibold tracking-[0.16em] whitespace-nowrap uppercase";

export function TransferStatusBadge({
  transfer,
  className,
}: {
  transfer: Pick<TrackedTransfer, "status" | "stage">;
  className?: string;
}) {
  const tone =
    transfer.status === "settled"
      ? "border-synthetic-yellow/50 bg-synthetic-yellow/10 text-synthetic-yellow"
      : transfer.status === "pending"
        ? "border-border-strong text-ink-2"
        : "border-warning/50 bg-warning-soft text-warning";
  return (
    <span className={cx(BADGE, tone, className)} data-testid="transfer-status" data-status={transfer.status}>
      {transfer.status === "pending" ? (
        <span aria-hidden className="size-1.5 bg-synthetic-yellow motion-safe:animate-pulse" />
      ) : null}
      {transferStatusLabel(transfer)}
    </span>
  );
}

const CLARIFICATION_TONES: Readonly<Record<ClarificationStatus, string>> = {
  open: "border-synthetic-yellow/60 text-synthetic-yellow",
  in_review: "border-synthetic-yellow/50 bg-synthetic-yellow/10 text-synthetic-yellow",
  resolved: "border-verified/40 bg-verified-soft text-verified",
  declined: "border-border text-muted",
};

export function ClarificationStatusBadge({ status, className }: { status: ClarificationStatus; className?: string }) {
  return (
    <span
      className={cx(BADGE, CLARIFICATION_TONES[status], className)}
      data-testid="clarification-status"
      data-status={status}
    >
      {CLARIFICATION_STATUS_LABELS[status]}
    </span>
  );
}

/** In or out: an arrow in a square, gold for money that came in. */
export function DirectionMark({
  transfer,
  className,
}: {
  transfer: Pick<TrackedTransfer, "direction">;
  className?: string;
}) {
  const incoming = transfer.direction === "incoming";
  const Icon = incoming ? ArrowDownLeft : ArrowUpRight;
  return (
    <span
      aria-hidden
      className={cx(
        "grid size-9 shrink-0 place-items-center border",
        incoming
          ? "border-synthetic-yellow/40 bg-synthetic-yellow/10 text-synthetic-yellow"
          : "border-border text-ink-2",
        className,
      )}
    >
      <Icon className="size-4" />
    </span>
  );
}

/** How long "Copied" stays before the button reads "Copy" again. */
const FEEDBACK_MS = 2000;

/**
 * Copies `value` in one tap and says so, to the eye and to screen readers.
 * `primary` is the solid gold button of the page's one main copy action.
 */
export function CopyButton({
  value,
  label,
  copiedLabel = "Copied",
  variant = "quiet",
  className,
  testId,
}: {
  value: string;
  /** "Copy tracking key". */
  label: string;
  copiedLabel?: string;
  variant?: "primary" | "quiet";
  className?: string;
  testId?: string;
}) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );

  async function copy() {
    const copied = await writeToClipboard(value);
    setState(copied ? "copied" : "failed");
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setState("idle"), FEEDBACK_MS);
  }

  const Icon = state === "copied" ? Check : Copy;
  return (
    <>
      <button
        type="button"
        onClick={() => void copy()}
        data-testid={testId}
        data-copied={state === "copied" ? "true" : undefined}
        className={cx(
          "inline-flex min-h-10 shrink-0 items-center justify-center gap-2 px-3 text-[11px] font-semibold tracking-[0.16em] uppercase transition-colors",
          "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
          variant === "primary"
            ? "bg-synthetic-yellow text-black hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed"
            : "border border-border text-ink-2 hover:border-synthetic-yellow/60 hover:text-synthetic-yellow",
          className,
        )}
      >
        <Icon className="size-3.5" aria-hidden />
        {state === "copied" ? copiedLabel : state === "failed" ? "Select and copy" : label}
      </button>
      <span className="sr-only" role="status">
        {state === "copied"
          ? `${copiedLabel}.`
          : state === "failed"
            ? "Couldn't copy. Select the text and copy it."
            : ""}
      </span>
    </>
  );
}
