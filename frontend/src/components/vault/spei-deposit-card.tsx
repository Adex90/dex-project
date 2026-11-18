"use client";

import { useEffect, useId, useRef, useState } from "react";
import { AlertTriangle, ArrowDownToLine, Check, Copy } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { formatClabe, isValidClabe } from "@/lib/clabe";
import { writeToClipboard } from "@/lib/clipboard";

/*
 * SPEI Deposit Card: where the client sees, and copies in one tap, the CLABE
 * that receives transfers into the vault.
 *
 * "Sovereign Gold" identity (globals.css): a charcoal card, the CLABE on a
 * black inset, and the one primary action, Copy CLABE, in solid Synthetic
 * Capital Yellow with black text (10.9:1).
 *
 * The CLABE is grouped for reading (bank · plaza · account · check digit)
 * but copied as the 18 bare digits banks expect. It is checked against its
 * check digit before the copy button appears: a corrupted CLABE would send
 * money nowhere, so the card warns instead of offering it.
 */

/** How long "Copied!" stays before the button returns to "Copy". */
const FEEDBACK_MS = 2000;

type CopyState = "idle" | "copied" | "failed";

export interface SpeiDepositCardProps {
  /** The client's 18-digit CLABE, unformatted. */
  clabe: string;
  /** The institution that holds the CLABE, as the sender's bank shows it. */
  bankName: string;
  className?: string;
}

export function SpeiDepositCard({ clabe, bankName, className }: SpeiDepositCardProps) {
  const titleId = useId();
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const resetTimer = useRef<number | null>(null);
  const valid = isValidClabe(clabe);

  // Never leave a pending reset behind when the card unmounts.
  useEffect(
    () => () => {
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    },
    [],
  );

  async function copyClabe() {
    const copied = await writeToClipboard(clabe);
    setCopyState(copied ? "copied" : "failed");
    if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    resetTimer.current = window.setTimeout(() => setCopyState("idle"), FEEDBACK_MS);
  }

  return (
    <section
      aria-labelledby={titleId}
      className={cx("rounded-2xl border border-border bg-surface p-5 text-ink sm:p-6", className)}
    >
      <div className="flex items-start gap-3">
        <span
          aria-hidden
          className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
        >
          <ArrowDownToLine className="size-[18px]" />
        </span>
        <div className="min-w-0">
          <h2 id={titleId} className="text-base font-semibold tracking-tight">
            Deposit via SPEI
          </h2>
          <p className="mt-1 text-sm text-muted">
            Send pesos from any bank in Mexico. Transfers usually arrive within seconds, 24/7.
          </p>
        </div>
      </div>

      <div className="mt-6 rounded-xl border border-border bg-bg p-4">
        <p className="text-[11px] font-medium tracking-[0.18em] text-muted uppercase">CLABE</p>
        {/* Never truncated: the full number must be readable before anyone sends money to it. */}
        <p
          className="mt-2 font-mono text-lg tracking-[0.06em] text-ink tabular-nums select-all sm:text-xl"
          data-testid="spei-clabe"
        >
          {formatClabe(clabe)}
        </p>
        {valid ? <CopyButton copied={copyState === "copied"} onCopy={copyClabe} /> : null}

        {valid && copyState === "failed" ? (
          <p className="mt-3 text-xs text-danger">
            Couldn&apos;t copy automatically. Select the number above and copy it.
          </p>
        ) : null}
        {!valid ? (
          <p className="mt-3 flex gap-2 text-xs leading-relaxed text-danger" role="alert">
            <AlertTriangle className="mt-px size-4 shrink-0" aria-hidden />
            This CLABE failed its check-digit validation. Don&apos;t send money to it; contact support.
          </p>
        ) : null}
      </div>

      <dl className="mt-5 flex items-baseline justify-between gap-4 text-sm">
        <dt className="text-muted">Bank</dt>
        <dd className="text-right font-medium text-ink" data-testid="spei-bank">
          {bankName}
        </dd>
      </dl>

      <p className="mt-4 border-t border-border pt-4 text-xs leading-relaxed text-muted">
        Enter this CLABE as the destination account in your bank&apos;s app. Pesos (MXN) over SPEI only.
      </p>

      {/* Announced to screen readers: the visible "Copied!" alone may not be. */}
      <p className="sr-only" role="status">
        {copyState === "copied"
          ? "CLABE copied to the clipboard."
          : copyState === "failed"
            ? "Couldn't copy the CLABE. Select the number and copy it."
            : ""}
      </p>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function CopyButton({ copied, onCopy }: { copied: boolean; onCopy: () => void }) {
  return (
    <button
      type="button"
      onClick={onCopy}
      data-testid="spei-copy"
      className={cx(
        // Full width and 44 px tall: one obvious, comfortable tap target at any card width.
        "mt-4 inline-flex min-h-11 w-full items-center justify-center gap-2 rounded-xl px-4 text-sm font-semibold transition",
        // Solid brand yellow, black text: the primary action. Pressing darkens it; the
        // confirmation keeps the yellow and swaps the icon and the words.
        "bg-synthetic-yellow text-black hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none",
        "active:scale-[0.98] motion-reduce:transition-none motion-reduce:active:scale-100",
      )}
    >
      {copied ? <Check className="size-4" aria-hidden /> : <Copy className="size-4" aria-hidden />}
      {copied ? "Copied!" : "Copy CLABE"}
    </button>
  );
}

/** Placeholder with the same footprint, for while the account loads. */
export function SpeiDepositCardSkeleton({ className }: { className?: string }) {
  return (
    <div className={cx("rounded-2xl border border-border bg-surface p-5 sm:p-6", className)} role="status">
      <span className="sr-only">Loading deposit details…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="flex items-start gap-3">
          <div className="size-10 rounded-xl bg-surface-2" />
          <div className="flex-1 space-y-2 pt-1">
            <div className="h-4 w-36 rounded-sm bg-surface-2" />
            <div className="h-3 w-3/4 rounded-sm bg-surface-2" />
          </div>
        </div>
        <div className="mt-6 h-[92px] rounded-xl bg-surface-2" />
        <div className="mt-5 h-4 rounded-sm bg-surface-2" />
      </div>
    </div>
  );
}
