"use client";

import { useId } from "react";
import Link from "next/link";
import { ArrowLeftRight, ChevronRight, MessageSquareText } from "lucide-react";

import { ClarificationStatusBadge, DirectionMark, TransferStatusBadge } from "@/components/transfers/transfer-parts";
import { Money } from "@/components/ui/data";
import { cx } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { formatOrderTime } from "@/lib/order-history";
import { KIND_LABELS, transferTitle } from "@/lib/transfers";
import { useTransfersStore } from "@/store/transfers";
import type { TrackedTransfer } from "@/types/transfers";

/*
 * The client's SPEI transfers, in and out, newest first: what each one was,
 * where it is (Processing, Settled, Returned) and its tracking key. A row
 * opens the transfer in the inspector (transfer-inspector.tsx): its steps,
 * its Banxico CEP and the clarification form.
 *
 * Pages of 25 with "Load more"; the first page reloads whenever the vault
 * moves, so a deposit that arrives or a withdrawal that settles shows at once
 * without losing the older pages already shown.
 */

export const QUIET_BUTTON = cx(
  "inline-flex h-10 items-center gap-1.5 border border-border px-4",
  "text-[11px] font-semibold tracking-[0.18em] text-muted uppercase transition-colors",
  "hover:border-synthetic-yellow/60 hover:text-synthetic-yellow disabled:cursor-wait disabled:opacity-60",
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
);

export const CARD = "border border-border bg-surface";

export function TransferList({
  onOpen,
  onRetry,
  onLoadMore,
  openId,
}: {
  onOpen: (transferId: string) => void;
  onRetry: () => void;
  onLoadMore: () => void;
  /** The transfer open in the inspector, marked in the list. */
  openId: string | null;
}) {
  const titleId = useId();
  const transfers = useTransfersStore((s) => s.transfers);
  const cursor = useTransfersStore((s) => s.cursor);
  const status = useTransfersStore((s) => s.status);
  const loadingMore = useTransfersStore((s) => s.loadingMore);
  const error = useTransfersStore((s) => s.error);

  if (transfers.length === 0 && (status === "idle" || status === "loading")) return <ListSkeleton />;
  if (transfers.length === 0 && status === "error") {
    return (
      <section className={cx(CARD, "px-5 py-4 sm:px-6")} data-testid="transfers-error">
        <p role="alert" className="text-sm text-danger">
          Could not load your transfers. {describeError(error)}
        </p>
        <button type="button" onClick={onRetry} className={cx(QUIET_BUTTON, "mt-3")}>
          Retry
        </button>
      </section>
    );
  }

  return (
    <section aria-labelledby={titleId} className={CARD} data-testid="transfers-list">
      <header className="flex min-w-0 items-center gap-3 border-b border-border px-5 py-4 sm:px-6">
        <span
          aria-hidden
          className="grid size-10 shrink-0 place-items-center bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
        >
          <ArrowLeftRight className="size-[18px]" />
        </span>
        <div className="min-w-0">
          <h3 id={titleId} className="text-base font-semibold tracking-tight">
            SPEI transfers
          </h3>
          <p className="text-xs text-muted">Deposits, withdrawals and payments, newest first</p>
        </div>
      </header>

      {transfers.length === 0 ? (
        <div className="flex flex-col items-start gap-3 px-5 py-6 sm:px-6" data-testid="transfers-empty">
          <p className="text-sm text-ink-2">
            No SPEI transfers yet. Deposits to your CLABE and withdrawals to your bank account show here, with their
            tracking key and Banxico receipt.
          </p>
          <Link href="/dashboard/account" className={QUIET_BUTTON}>
            Go to your account
          </Link>
        </div>
      ) : (
        <ul className="divide-y divide-border">
          {transfers.map((transfer) => (
            <li key={transfer.transfer_id}>
              <TransferRow transfer={transfer} open={transfer.transfer_id === openId} onOpen={onOpen} />
            </li>
          ))}
        </ul>
      )}

      {transfers.length > 0 ? (
        <footer className="flex flex-wrap items-center justify-between gap-3 border-t border-border px-5 py-3 sm:px-6">
          {error !== null ? (
            <p role="alert" className="text-xs text-danger">
              Some transfers could not be loaded. {describeError(error)}
            </p>
          ) : (
            <p className="text-xs text-muted" data-testid="transfers-shown">
              {cursor === null
                ? `${transfers.length} ${transfers.length === 1 ? "transfer" : "transfers"}: that's all of them`
                : `Showing the latest ${transfers.length}`}
            </p>
          )}
          {cursor !== null ? (
            <button
              type="button"
              onClick={onLoadMore}
              disabled={loadingMore}
              className={QUIET_BUTTON}
              data-testid="transfers-more"
            >
              {loadingMore ? "Loading…" : "Load more"}
            </button>
          ) : null}
        </footer>
      ) : null}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function TransferRow({
  transfer,
  open,
  onOpen,
}: {
  transfer: TrackedTransfer;
  open: boolean;
  onOpen: (transferId: string) => void;
}) {
  const incoming = transfer.direction === "incoming";
  // A transfer that came back moved no money in the end: no sign, in zinc.
  const back = transfer.status === "returned";
  return (
    <button
      type="button"
      onClick={() => onOpen(transfer.transfer_id)}
      aria-haspopup="dialog"
      aria-current={open ? "true" : undefined}
      data-testid="transfer-row"
      data-transfer-id={transfer.transfer_id}
      data-direction={transfer.direction}
      data-status={transfer.status}
      className={cx(
        "group relative flex w-full min-w-0 items-start gap-3 px-5 py-3.5 text-left transition-colors sm:px-6",
        "hover:bg-surface-2 focus-visible:bg-surface-2 focus-visible:outline-none",
        // The yellow edge: on hover, on focus and on the one open in the inspector.
        "before:absolute before:inset-y-2 before:left-0 before:w-[3px] before:bg-synthetic-yellow before:opacity-0 before:transition-opacity",
        "hover:before:opacity-100 focus-visible:before:opacity-100",
        open && "bg-surface-2 before:opacity-100",
      )}
    >
      <DirectionMark transfer={transfer} />
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 items-baseline justify-between gap-3">
          {/* Two lines on a phone (the amount takes its share of the row), one from sm up. */}
          <span
            className="line-clamp-2 min-w-0 font-medium break-words text-ink sm:line-clamp-1"
            data-testid="transfer-title"
          >
            {transferTitle(transfer)}
          </span>
          <Money
            value={transfer.amount}
            sign={back ? undefined : incoming ? "+" : "-"}
            className={cx(
              "shrink-0 text-sm font-semibold",
              back ? "text-muted" : incoming ? "text-synthetic-yellow" : "text-ink",
            )}
          />
        </span>
        <span className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
          <span className="tabular-nums">
            {KIND_LABELS[transfer.kind]} · {formatOrderTime(transfer.initiated_at)}
          </span>
          <TransferStatusBadge transfer={transfer} />
          {transfer.clarification ? (
            <span className="inline-flex items-center gap-1.5" data-testid="transfer-clarification">
              <MessageSquareText className="size-3.5 text-muted" aria-hidden />
              <span className="sr-only">Clarification</span>#{transfer.clarification.number}
              <ClarificationStatusBadge status={transfer.clarification.status} />
            </span>
          ) : null}
        </span>
        <span className="mt-1 block truncate font-mono text-[11px] tracking-wide text-muted" data-testid="transfer-key">
          <span className="sr-only">Tracking key </span>
          {transfer.tracking_key}
        </span>
      </span>
      <ChevronRight
        aria-hidden
        className="mt-2.5 size-4 shrink-0 text-muted transition-colors group-hover:text-synthetic-yellow"
      />
    </button>
  );
}

function ListSkeleton() {
  return (
    <div className={CARD} role="status" data-testid="transfers-loading">
      <span className="sr-only">Loading your transfers…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="flex items-center gap-3 border-b border-border px-5 py-4 sm:px-6">
          <div className="size-10 bg-surface-2" />
          <div className="space-y-2">
            <div className="h-4 w-28 bg-surface-2" />
            <div className="h-3 w-44 bg-surface-2" />
          </div>
        </div>
        {Array.from({ length: 4 }, (_, i) => (
          <div key={i} className="flex items-start gap-3 border-b border-border px-5 py-4 last:border-b-0 sm:px-6">
            <div className="size-9 shrink-0 bg-surface-2" />
            <div className="flex-1 space-y-2">
              <div className="flex justify-between gap-3">
                <div className="h-4 w-40 bg-surface-2" />
                <div className="h-4 w-20 bg-surface-2" />
              </div>
              <div className="h-3 w-52 bg-surface-2" />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
