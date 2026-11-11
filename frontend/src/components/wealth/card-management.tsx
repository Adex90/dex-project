"use client";

import { useCallback, useEffect, useId, useState, type ComponentType } from "react";
import { KeyRound, Lock, LockOpen, Nfc, Settings2 } from "lucide-react";

import { BrandMark } from "@/components/brand";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { cx } from "@/components/ui/primitives";
import { formatShare } from "@/lib/allocation";
import { formatMoney } from "@/lib/decimal";
import { SAVEBACK, cardholderName, savebackPreview } from "@/lib/wealth";
import { CARD_USES, useCardStore, type CardUse } from "@/store/card";

/*
 * Card Management: the Synthetic Capital debit card, in the Sovereign Gold
 * identity. A preview: no card issuer is connected yet, so the card has no
 * number and no PIN, and nothing here is sent anywhere (store/card.ts).
 *
 * - The card face: pure black and square, the mark and wordmark in gold, a
 *   minimal chip, the contactless sign, the number masked, and the holder's
 *   name as a card carries it (`cardholderName`).
 * - Lock: a toggle. A locked card is frosted over and says so, and the
 *   button stays gold (pressed) until it is unlocked. Screen readers hear the
 *   new state. "Lock", never "freeze": the client's screens must not use the
 *   words of an AML hold, which is never disclosed (store/card.ts).
 * - Show PIN: opens the PIN's place for 10 seconds, the time a PIN shows
 *   for; a preview card has none yet, and says so.
 * - Settings: what the card may be used for (online, contactless, ATM,
 *   abroad), as switches, gold when on.
 * - Saveback: 1 % of card payments back, up to $300.00 a month, on simulated
 *   spending (lib/wealth.ts), as a gold bar toward the cap.
 * - Buttons are charcoal, gold on hover and when on. The section fits its
 *   column: stacked in a narrow one, the card beside its controls in a wide
 *   one (a container query).
 */

/** How long the PIN's place stays open, as a PIN would show. */
const PIN_SECONDS = 10;

export function CardManagement({ className }: { className?: string }) {
  const titleId = useId();
  const pinId = useId();
  const settingsId = useId();
  const { client } = useDashboard();
  const locked = useCardStore((s) => s.locked);
  const toggleLocked = useCardStore((s) => s.toggleLocked);
  const [pinOpen, setPinOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // Stable, so the PIN's countdown is not restarted by a render of this section.
  const closePin = useCallback(() => setPinOpen(false), []);
  const holder = client ? cardholderName(client) : "";

  return (
    <section
      aria-labelledby={titleId}
      className={cx("@container border border-border bg-surface p-5 sm:p-6", className)}
      data-testid="card-management"
    >
      <header className="flex flex-wrap items-center justify-between gap-3">
        <h2
          id={titleId}
          className="inline-flex items-center gap-2 text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase"
        >
          <span aria-hidden className="size-1.5 bg-synthetic-yellow" />
          Debit card
        </h2>
        <span className="border border-border-strong/60 px-2.5 py-1 text-[10px] font-semibold tracking-[0.2em] text-ink-2 uppercase">
          Preview · not issued yet
        </span>
      </header>

      <div className="mt-5 flex flex-col gap-6 @2xl:flex-row @2xl:items-start">
        <div className="w-full max-w-sm shrink-0 @2xl:w-80">
          <CardFace holder={holder} locked={locked} />
          <p
            role="status"
            className={cx("mt-3 flex items-center gap-2 text-xs", locked ? "text-ink" : "text-muted")}
            data-testid="card-status"
          >
            <span aria-hidden className={cx("size-2", locked ? "bg-ink-2" : "bg-gain")} />
            {locked ? "Locked: payments are declined until you unlock it." : "Active: payments allowed."}
          </p>
        </div>

        <div className="min-w-0 flex-1 space-y-4">
          <div className="grid grid-cols-3 gap-2">
            <Action
              icon={locked ? LockOpen : Lock}
              label={locked ? "Unlock" : "Lock"}
              on={locked}
              aria={{ "aria-pressed": locked }}
              onClick={toggleLocked}
              testId="card-lock"
            />
            <Action
              icon={KeyRound}
              label={pinOpen ? "Hide PIN" : "Show PIN"}
              on={pinOpen}
              aria={{ "aria-expanded": pinOpen, "aria-controls": pinId }}
              onClick={() => setPinOpen((open) => !open)}
              testId="card-pin"
            />
            <Action
              icon={Settings2}
              label="Settings"
              on={settingsOpen}
              aria={{ "aria-expanded": settingsOpen, "aria-controls": settingsId }}
              onClick={() => setSettingsOpen((open) => !open)}
              testId="card-settings"
            />
          </div>
          {pinOpen ? <PinPanel id={pinId} onDone={closePin} /> : null}
          {settingsOpen ? <SettingsPanel id={settingsId} /> : null}
          <Saveback seed={client?.id ?? "preview"} />
        </div>
      </div>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

/** The card itself: pure black, square, ID-1 proportions (85.60 × 53.98 mm). */
function CardFace({ holder, locked }: { holder: string; locked: boolean }) {
  return (
    <div
      className={cx(
        "@container relative aspect-[1.586] w-full overflow-hidden border border-border-strong/50 bg-black select-none",
        "transition-[filter] duration-500",
        locked && "grayscale",
      )}
      data-testid="card-face"
      data-locked={locked}
    >
      {/* A gold hairline along the top edge, as on the vault's balance. */}
      <span
        aria-hidden
        className="pointer-events-none absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-synthetic-yellow/80 to-transparent"
      />
      <div className="flex h-full flex-col justify-between p-[6.5cqw]">
        <div className="flex items-start justify-between gap-3">
          <span className="inline-flex items-center gap-[2.5cqw]">
            <BrandMark className="size-[8cqw] text-synthetic-yellow" />
            <span className="text-[3.6cqw] font-semibold tracking-[0.22em] text-synthetic-yellow uppercase">
              Synthetic Capital
            </span>
          </span>
          <span className="text-[3.2cqw] font-semibold tracking-[0.24em] text-ink-2 uppercase">Debit</span>
        </div>
        <div className="flex items-center justify-between">
          <Chip />
          <Nfc className="size-[7cqw] text-ink-2" aria-hidden />
        </div>
        <div>
          {/* Not issued: no number to show, not even the last digits. */}
          <p className="font-mono text-[5.4cqw] tracking-[0.16em] text-ink" data-testid="card-number">
            <span aria-hidden>•••• •••• •••• ••••</span>
            <span className="sr-only">Card number: not issued yet</span>
          </p>
          <p className="mt-[2.5cqw] flex items-end justify-between gap-3">
            <span className="truncate text-[3.6cqw] font-medium tracking-[0.16em] text-ink-2" data-testid="card-holder">
              {holder}
            </span>
            <span className="shrink-0 text-[3.2cqw] font-semibold tracking-[0.2em] text-muted uppercase">Virtual</span>
          </p>
        </div>
      </div>
      {locked ? (
        <div className="absolute inset-0 grid place-items-center bg-black/55 backdrop-blur-[2px]">
          <span className="inline-flex items-center gap-2 border border-ink/40 bg-black/70 px-3 py-1.5 text-xs font-semibold tracking-[0.24em] text-ink uppercase">
            <Lock className="size-4" aria-hidden />
            Locked
          </span>
        </div>
      ) : null}
    </div>
  );
}

/** A minimal chip: a gold outline and its contact lines. */
function Chip() {
  return (
    <svg viewBox="0 0 44 34" className="w-[13cqw] text-synthetic-yellow/80" aria-hidden>
      <rect
        x="0.75"
        y="0.75"
        width="42.5"
        height="32.5"
        fill="currentColor"
        fillOpacity="0.08"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      <path
        d="M0.75 11.5H14M0.75 22.5H14M30 11.5H43.25M30 22.5H43.25M14 0.75V33.25M30 0.75V33.25"
        stroke="currentColor"
        strokeWidth="1.2"
        fill="none"
      />
      <rect x="18" y="9" width="8" height="16" fill="none" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  );
}

/** A square action: charcoal at rest, gold on hover and while on (pressed or open). */
function Action({
  icon: Icon,
  label,
  on,
  aria,
  onClick,
  testId,
}: {
  icon: ComponentType<{ className?: string; "aria-hidden"?: boolean }>;
  label: string;
  on: boolean;
  aria: { "aria-pressed"?: boolean; "aria-expanded"?: boolean; "aria-controls"?: string };
  onClick: () => void;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      {...aria}
      className={cx(
        "flex min-h-16 flex-col items-center justify-center gap-1.5 border px-2 py-3 text-[11px] font-semibold tracking-[0.14em] uppercase transition-colors",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
        on
          ? "border-synthetic-yellow bg-synthetic-yellow text-black hover:bg-synthetic-yellow-hover"
          : "border-border bg-surface-2 text-ink-2 hover:border-synthetic-yellow hover:text-synthetic-yellow",
      )}
      data-testid={testId}
    >
      <Icon className="size-[18px]" aria-hidden />
      {label}
    </button>
  );
}

/** Where the PIN shows, for ten seconds; a preview card has no PIN yet. */
function PinPanel({ id, onDone }: { id: string; onDone: () => void }) {
  const [left, setLeft] = useState(PIN_SECONDS);
  useEffect(() => {
    if (left <= 0) {
      onDone();
      return;
    }
    const tick = setTimeout(() => setLeft((seconds) => seconds - 1), 1_000);
    return () => clearTimeout(tick);
  }, [left, onDone]);

  return (
    <div id={id} className="border border-border bg-bg p-4" data-testid="card-pin-panel">
      <div className="flex items-center justify-between gap-3 text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">
        <span>PIN</span>
        <span className="normal-case tabular-nums" data-testid="card-pin-countdown">
          Hides in {left} s
        </span>
      </div>
      <div className="mt-3 grid grid-cols-4 gap-2" aria-hidden>
        {[0, 1, 2, 3].map((cell) => (
          <span
            key={cell}
            className="grid h-12 place-items-center border border-border-strong/60 bg-surface font-mono text-xl text-muted"
          >
            –
          </span>
        ))}
      </div>
      <div className="mt-3 h-0.5 bg-surface-2">
        <div
          className="h-full bg-synthetic-yellow transition-[width] duration-1000 ease-linear motion-reduce:transition-none"
          style={{ width: `${(left / PIN_SECONDS) * 100}%` }}
        />
      </div>
      <p className="mt-3 text-xs text-muted">
        No PIN yet: you choose it when your card is issued. It then shows here for {PIN_SECONDS} seconds.
      </p>
    </div>
  );
}

/** What the card may be used for: square switches, gold when on. */
function SettingsPanel({ id }: { id: string }) {
  const uses = useCardStore((s) => s.uses);
  const setUse = useCardStore((s) => s.setUse);
  return (
    <div id={id} data-testid="card-settings-panel">
      <ul className="divide-y divide-border border border-border">
        {CARD_USES.map((use) => (
          <li key={use.key} className="flex items-center justify-between gap-4 bg-bg px-4 py-3">
            <span className="min-w-0">
              <span className="block text-sm font-medium text-ink">{use.label}</span>
              <span className="block text-xs text-muted">{use.detail}</span>
            </span>
            <Switch use={use.key} label={use.label} on={uses[use.key]} onChange={(on) => setUse(use.key, on)} />
          </li>
        ))}
      </ul>
      <p className="mt-2 text-xs text-muted">Preview: nothing is sent until the card is issued.</p>
    </div>
  );
}

function Switch({
  use,
  label,
  on,
  onChange,
}: {
  use: CardUse;
  label: string;
  on: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      onClick={() => onChange(!on)}
      className={cx(
        "relative h-6 w-11 shrink-0 border transition-colors",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none",
        on ? "border-synthetic-yellow bg-synthetic-yellow" : "border-border-strong bg-surface-2",
      )}
      data-testid="card-use"
      data-use={use}
    >
      <span
        aria-hidden
        className={cx(
          "absolute top-1/2 left-0.5 size-4 -translate-y-1/2 transition-transform",
          on ? "translate-x-5 bg-black" : "bg-ink-2",
        )}
      />
    </button>
  );
}

/** Saveback this month: a gold bar toward the cap, on simulated card spending. */
function Saveback({ seed }: { seed: string }) {
  const titleId = useId();
  const [now] = useState(() => Date.now());
  const { spent, earned, cap, progress } = savebackPreview(now, seed);
  return (
    <section aria-labelledby={titleId} className="border-t border-border pt-5" data-testid="saveback">
      <div className="flex items-center justify-between gap-3">
        <h3 id={titleId} className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">
          Saveback · this month
        </h3>
        <span className="text-xs font-semibold text-ink tabular-nums" data-testid="saveback-progress">
          {formatShare(progress)}
        </span>
      </div>
      <p className="mt-2 flex flex-wrap items-baseline gap-x-2 tabular-nums">
        <span className="text-2xl font-semibold tracking-tight text-ink" data-testid="saveback-earned">
          {formatMoney(earned)}
        </span>
        <span className="text-sm text-muted">of {formatMoney(cap)}</span>
      </p>
      <div
        role="progressbar"
        aria-label="Saveback this month"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.floor(progress / 100)}
        aria-valuetext={`${formatMoney(earned)} of ${formatMoney(cap)}`}
        className="mt-3 h-2 bg-surface-2"
      >
        <div className="h-full bg-synthetic-yellow" style={{ width: `${progress / 100}%` }} />
      </div>
      <p className="mt-2 text-xs text-muted">
        {SAVEBACK.ratePct}% of every card payment, up to {formatMoney(SAVEBACK.monthlyCap)} a month, into your vault.
        Preview: on simulated spending of {formatMoney(spent)}.
      </p>
    </section>
  );
}
