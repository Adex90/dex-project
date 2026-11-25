"use client";

import { useEffect, useId, useRef } from "react";
import { ArrowRight, MessagesSquare } from "lucide-react";

import { CARD, QUIET_BUTTON } from "@/components/transfers/transfer-list";
import { ClarificationStatusBadge } from "@/components/transfers/transfer-parts";
import { cx } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { formatOrderTime } from "@/lib/order-history";
import {
  MAX_ACTIVE_CLARIFICATIONS,
  REASON_LABELS,
  STEP_LABELS,
  activeClarifications,
  clarificationStatusText,
  isClosed,
} from "@/lib/transfers";
import { useTransfersStore } from "@/store/transfers";
import type { SpeiClarification } from "@/types/transfers";

/*
 * The client's clarifications about their SPEI transfers, newest first: the
 * case number, why it was opened, where it is (Open, In review, Resolved,
 * Declined), its history from the immutable security log and, once closed,
 * the note from the team that closed it. Each opens its transfer.
 *
 * Neutral words throughout: a clarification is received, reviewed and
 * closed; nothing says why an account could be looked at.
 */

export function ClarificationList({
  onOpenTransfer,
  onShowTransfers,
  onRetry,
  focusTitle = false,
}: {
  onOpenTransfer: (transferId: string) => void;
  onShowTransfers: () => void;
  onRetry: () => void;
  /** Take the focus once shown (opened from the inspector, which is gone). */
  focusTitle?: boolean;
}) {
  const titleId = useId();
  const titleRef = useRef<HTMLHeadingElement>(null);
  const clarifications = useTransfersStore((s) => s.clarifications);
  const status = useTransfersStore((s) => s.clarificationsStatus);
  const error = useTransfersStore((s) => s.clarificationsError);
  const active = activeClarifications(clarifications);
  const shown = clarifications.length > 0 || status === "ready";

  useEffect(() => {
    if (focusTitle && shown) titleRef.current?.focus();
  }, [focusTitle, shown]);

  if (clarifications.length === 0 && (status === "idle" || status === "loading")) {
    return (
      <div className={cx(CARD, "px-5 py-6 sm:px-6")} role="status" data-testid="clarifications-loading">
        <span className="sr-only">Loading your clarifications…</span>
        <div aria-hidden className="space-y-3 motion-safe:animate-pulse">
          <div className="h-4 w-40 bg-surface-2" />
          <div className="h-16 w-full bg-surface-2" />
          <div className="h-16 w-full bg-surface-2" />
        </div>
      </div>
    );
  }
  if (clarifications.length === 0 && status === "error") {
    return (
      <section className={cx(CARD, "px-5 py-4 sm:px-6")} data-testid="clarifications-error">
        <p role="alert" className="text-sm text-danger">
          Could not load your clarifications. {describeError(error)}
        </p>
        <button type="button" onClick={onRetry} className={cx(QUIET_BUTTON, "mt-3")}>
          Retry
        </button>
      </section>
    );
  }

  return (
    <section aria-labelledby={titleId} className={CARD} data-testid="clarifications">
      <header className="flex min-w-0 items-center gap-3 border-b border-border px-5 py-4 sm:px-6">
        <span
          aria-hidden
          className="grid size-10 shrink-0 place-items-center bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
        >
          <MessagesSquare className="size-[18px]" />
        </span>
        <div className="min-w-0">
          <h3 id={titleId} ref={titleRef} tabIndex={-1} className="text-base font-semibold tracking-tight outline-none">
            Clarifications
          </h3>
          <p className="text-xs text-muted" data-testid="clarifications-count">
            {active === 0 ? "None open right now" : `${active} open of ${MAX_ACTIVE_CLARIFICATIONS} at a time`}
          </p>
        </div>
      </header>

      {error !== null && clarifications.length > 0 ? (
        <p role="alert" className="border-b border-border px-5 py-2.5 text-xs text-danger sm:px-6">
          Showing the last clarifications received. {describeError(error)}
        </p>
      ) : null}

      {clarifications.length === 0 ? (
        <div className="flex flex-col items-start gap-3 px-5 py-6 sm:px-6" data-testid="clarifications-empty">
          <p className="text-sm text-ink-2">
            No clarifications. If a transfer is delayed, doesn&apos;t arrive or comes back, open one from the
            transfer&apos;s details and follow it here.
          </p>
          <button type="button" onClick={onShowTransfers} className={QUIET_BUTTON}>
            See your transfers
          </button>
        </div>
      ) : (
        <ul className="divide-y divide-border">
          {clarifications.map((clarification) => (
            <li key={clarification.clarification_id}>
              <ClarificationCard clarification={clarification} onOpenTransfer={onOpenTransfer} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function ClarificationCard({
  clarification,
  onOpenTransfer,
}: {
  clarification: SpeiClarification;
  onOpenTransfer: (transferId: string) => void;
}) {
  const closed = isClosed(clarification.status);
  return (
    <article
      className="px-5 py-4 sm:px-6"
      data-testid="clarification"
      data-number={clarification.number}
      data-status={clarification.status}
      aria-label={`Clarification ${clarification.number}`}
    >
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2">
        <p className="flex min-w-0 items-baseline gap-2">
          <span className="font-mono text-sm font-semibold text-synthetic-yellow">#{clarification.number}</span>
          <span className="truncate text-sm font-medium text-ink" data-testid="clarification-reason-label">
            {REASON_LABELS[clarification.reason]}
          </span>
        </p>
        <ClarificationStatusBadge status={clarification.status} />
      </div>
      <p className="mt-1 text-xs text-muted">
        {clarification.direction === "incoming" ? "Deposit" : "Outgoing transfer"} ·{" "}
        <span className="font-mono tracking-wide text-ink-2">{clarification.tracking_key}</span>
      </p>
      <p className="mt-2 text-xs leading-relaxed text-ink-2" data-testid="clarification-status-text">
        {clarificationStatusText(clarification.status)}
      </p>

      {clarification.description !== null ? (
        <blockquote className="mt-3 border-l-2 border-border pl-3 text-xs leading-relaxed whitespace-pre-line text-muted">
          <span className="sr-only">You wrote: </span>
          {clarification.description}
        </blockquote>
      ) : null}

      {clarification.resolution_note !== null ? (
        <div
          className={cx(
            "mt-3 border px-3 py-2.5",
            closed ? "border-synthetic-yellow/40 bg-synthetic-yellow/5" : "border-border bg-surface-2",
          )}
          data-testid="clarification-note"
        >
          <p className="text-[11px] font-semibold tracking-[0.16em] text-synthetic-yellow uppercase">
            Note from our team
          </p>
          <p className="mt-1 text-sm leading-relaxed whitespace-pre-line text-ink-2">{clarification.resolution_note}</p>
        </div>
      ) : null}

      <ol className="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-muted" data-testid="clarification-history">
        {(clarification.history.length > 0
          ? clarification.history
          : [{ status: "open" as const, at: clarification.opened_at }]
        ).map((step, i) => (
          <li key={`${step.status}-${i}`} className="inline-flex items-center gap-1.5 tabular-nums">
            <span aria-hidden className={cx("size-1.5", i === 0 ? "bg-muted" : "bg-synthetic-yellow")} />
            <span className="font-semibold text-ink-2">{STEP_LABELS[step.status]}</span>
            {formatOrderTime(step.at)}
          </li>
        ))}
      </ol>

      <button
        type="button"
        onClick={() => onOpenTransfer(clarification.transfer_id)}
        className="mt-3 inline-flex min-h-10 items-center gap-1.5 text-[11px] font-semibold tracking-[0.16em] text-synthetic-yellow uppercase hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
        aria-haspopup="dialog"
        data-testid="clarification-open-transfer"
      >
        View transfer
        <ArrowRight className="size-3.5" aria-hidden />
      </button>
    </article>
  );
}
