"use client";

import { useEffect, useId, useRef, useState, type ChangeEvent, type FormEvent, type RefObject } from "react";
import { ArrowLeft, BadgeCheck, Check, Clock, Delete, Landmark, Loader2, ShieldCheck, Undo2, X } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { newIdempotencyKey } from "@/lib/api/endpoints";
import { formatAmount, formatMoney, toUnits } from "@/lib/decimal";
import { NUMPAD_KEYS, formatAmountInput, pressKey, sanitizeAmount, type NumpadKey } from "@/lib/market";
import {
  STATUS_LABELS,
  accountLabel,
  assessWithdrawal,
  bankName,
  maskedClabe,
  receiptTitle,
  withdrawalRequest,
  withdrawalSignature,
} from "@/lib/withdrawal";
import type { DecimalString } from "@/types/vault";
import type { LinkedAccount, Withdrawal, WithdrawalRequest } from "@/types/wealth";

/*
 * Withdrawal Drawer: the fiat off-ramp, a checkout like the trading one, in
 * the Sovereign Gold identity (square, black and charcoal, one gold action).
 *
 * 1. Account: where the money goes. Only linked bank accounts, all in the
 *    client's own name (the backend's whitelist, migration 0016); the holder
 *    is shown as the bank has it, with the mark that it is the client.
 * 2. Amount: a numpad, against the available cash only. Cash held by open
 *    limit and stop orders stays put (the backend checks `saldo_disponible`,
 *    never `saldo_comprometido`), and the screen says so when there is some.
 *    Max is the available cash rounded down to the cent.
 * 3. Confirm: the summary and a stark gold button. The amount leaves the
 *    available cash at once and the withdrawal is "processing" until the
 *    SPEI network settles it; the receipt carries the SPEI tracking key.
 *
 * - One idempotency key per withdrawal: a retry of the same account and
 *   amount reuses it, so a lost answer can never send the money twice;
 *   changing either draws a new one.
 * - Confirm may first ask "Confirm it's you" (step-up, roadmap item 22): a
 *   dialog above this one; once confirmed, the same withdrawal goes on with
 *   the same key. Closing it leaves the withdrawal unsent, here, to retry.
 * - A native <dialog> opened as a modal: focus stays inside, the page behind
 *   is inert, Escape closes (not while sending), and closing returns focus to
 *   the button that opened it. Each step moves the focus to its title (the
 *   amount step, to the amount, so a keyboard types right away).
 * - On a phone, a bottom sheet with a large numpad (the amount field asks for
 *   no on-screen keyboard); on a desktop, a centered panel. Enter moves on.
 * - The words on screen are those of a bank transfer only: a refusal for any
 *   other reason is the API's neutral message (lib/api/client.ts).
 */

export interface WithdrawalDrawerProps {
  open: boolean;
  onClose: () => void;
  /** The vault's available cash, MXN: all a withdrawal can take. */
  cash: DecimalString;
  /** Cash held by open limit and stop orders: it cannot be withdrawn. */
  held: DecimalString;
  /** Loads the linked accounts; rejects with the error to show. */
  loadAccounts: (signal: AbortSignal) => Promise<LinkedAccount[]>;
  /**
   * Sends the withdrawal to `account`: resolves with it, or rejects with the
   * error to show. It may first ask the person to confirm it's them (step-up).
   */
  onConfirm: (request: WithdrawalRequest, account: LinkedAccount) => Promise<Withdrawal>;
}

export function WithdrawalDrawer(props: WithdrawalDrawerProps) {
  // Mounted only while open: every opening starts at the first step, with an empty amount.
  return props.open ? <WithdrawalDialog {...props} /> : null;
}

type Step = "account" | "amount" | "review" | "done";
type EntryStep = Exclude<Step, "done">;

const STEPS: readonly { step: EntryStep; label: string; title: string }[] = [
  { step: "account", label: "Account", title: "Where it goes" },
  { step: "amount", label: "Amount", title: "How much" },
  { step: "review", label: "Confirm", title: "Review and confirm" },
];

const KEY_LABELS: Partial<Record<NumpadKey, string>> = { ".": "Decimal point", back: "Delete last digit" };

/** The amount's type: the input and the invisible copy that sizes it must match to the pixel. */
const AMOUNT_TYPE = "font-mono text-5xl font-semibold tracking-tight tabular-nums";

const QUIET_ACTION =
  "font-semibold tracking-[0.16em] text-synthetic-yellow uppercase hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none disabled:opacity-40";

const PRIMARY =
  "inline-flex h-14 min-w-0 flex-1 items-center justify-center gap-2 bg-synthetic-yellow text-base font-bold tracking-[0.18em] text-black uppercase transition-colors hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-40";

const NOT_LINKED_ANYMORE = "This bank account is no longer linked. Close Withdraw and open it again.";

function errorMessage(error: unknown): string {
  if (error instanceof ApiError && error.code === "NO_ENCONTRADO") return NOT_LINKED_ANYMORE;
  return describeError(error);
}

function WithdrawalDialog({ onClose, cash, held, loadAccounts, onConfirm }: Omit<WithdrawalDrawerProps, "open">) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const amountRef = useRef<HTMLInputElement>(null);
  const doneRef = useRef<HTMLButtonElement>(null);
  const attempt = useRef<{ signature: string; key: string } | null>(null);
  const titleId = useId();
  const amountId = useId();
  const amountHintId = useId();
  const [step, setStep] = useState<Step>("account");
  const [accounts, setAccounts] = useState<LinkedAccount[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [loadRound, setLoadRound] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [amount, setAmount] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [receipt, setReceipt] = useState<Withdrawal | null>(null);

  const selected = accounts?.find((account) => account.id === selectedId) ?? null;
  const assessment = assessWithdrawal(amount, cash);
  const heldSome = toUnits(held) > 0n;
  const title = step === "done" ? "Withdrawal" : (STEPS.find((s) => s.step === step)?.title ?? "");

  // A modal from the first frame; the page behind does not scroll meanwhile.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    dialog.showModal();
    const root = document.documentElement;
    const overflow = root.style.overflow;
    root.style.overflow = "hidden";
    return () => {
      root.style.overflow = overflow;
      if (dialog.open) dialog.close();
    };
  }, []);

  // The accounts, once per opening (and again on "Try again"). The first is preselected.
  useEffect(() => {
    const controller = new AbortController();
    loadAccounts(controller.signal).then(
      (list) => {
        setAccounts(list);
        setSelectedId((current) =>
          current !== null && list.some((a) => a.id === current) ? current : (list[0]?.id ?? null),
        );
      },
      (cause: unknown) => {
        if (!controller.signal.aborted) setLoadError(cause);
      },
    );
    return () => controller.abort();
  }, [loadAccounts, loadRound]);

  useEffect(() => {
    if (step === "amount") amountRef.current?.focus();
    else if (step === "done") doneRef.current?.focus();
    else headingRef.current?.focus();
  }, [step]);

  /** Closing goes through the dialog, which hands focus back; its "close" event unmounts us. */
  function requestClose() {
    if (!busy) dialogRef.current?.close();
  }

  function retryLoad() {
    setLoadError(null);
    setAccounts(null);
    setLoadRound((n) => n + 1);
  }

  function editAmount(next: string) {
    if (busy || next === amount) return;
    setAmount(next);
    setError(null);
  }

  function onTypeAmount(event: ChangeEvent<HTMLInputElement>) {
    const next = sanitizeAmount(event.target.value);
    if (next !== null) editAmount(next);
  }

  function back() {
    if (busy) return;
    setError(null);
    setStep(step === "review" ? "amount" : "account");
  }

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    if (step === "account") {
      if (selected) setStep("amount");
      return;
    }
    if (step === "amount") {
      if (assessment.ready) setStep("review");
      return;
    }
    if (step !== "review" || !selected || !assessment.ready) return;
    // The same withdrawal retried keeps its key; another account or amount is a new one.
    const signature = withdrawalSignature(selected.id, amount);
    if (attempt.current?.signature !== signature) attempt.current = { signature, key: newIdempotencyKey() };
    setBusy(true);
    setError(null);
    try {
      setReceipt(await onConfirm(withdrawalRequest(selected.id, amount, attempt.current.key), selected));
      setStep("done");
    } catch (cause) {
      setError(cause);
    } finally {
      setBusy(false);
    }
  }

  const canContinue = step === "account" ? selected !== null : step === "amount" ? assessment.ready : assessment.ready;

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onCancel={(event) => {
        // Escape: not while a withdrawal is in flight.
        if (busy) event.preventDefault();
      }}
      onClose={onClose}
      onClick={(event) => {
        // A click on the backdrop lands on the dialog element itself.
        if (event.target === dialogRef.current) requestClose();
      }}
      className={cx(
        // Phone: a sheet along the bottom edge. Wider: a panel in the middle.
        "inset-0 mx-0 mt-auto mb-0 h-fit max-h-[92dvh] w-full max-w-full overflow-y-auto p-0",
        "border-t border-border bg-surface text-ink sm:m-auto sm:max-w-md sm:border",
        "backdrop:bg-black/75 backdrop:backdrop-blur-[2px]",
        "transition duration-200 starting:translate-y-6 starting:opacity-0 motion-reduce:transition-none",
      )}
      data-testid="withdrawal-drawer"
      data-step={step}
    >
      <header className="border-b border-border px-5 pt-4 pb-3.5">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
              Withdraw · SPEI
            </p>
            <h2
              id={titleId}
              ref={headingRef}
              tabIndex={-1}
              className="mt-1 truncate text-base font-semibold tracking-tight outline-none"
            >
              {title}
            </h2>
          </div>
          <button
            type="button"
            onClick={requestClose}
            disabled={busy}
            aria-label="Close"
            className="grid size-10 shrink-0 place-items-center border border-border text-muted transition-colors hover:border-border-strong hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none disabled:opacity-40"
          >
            <X className="size-4" aria-hidden />
          </button>
        </div>
        {step === "done" ? null : <Progress step={step} />}
      </header>

      {step === "done" && receipt ? (
        <Receipt withdrawal={receipt} doneRef={doneRef} onDone={requestClose} />
      ) : (
        <form onSubmit={onSubmit} noValidate>
          {step === "account" ? (
            <div className="px-5 py-5" data-testid="withdraw-step-account">
              {loadError !== null ? (
                <div role="alert" className="border border-danger/40 bg-danger-soft px-3 py-3 text-sm text-danger">
                  <p>{errorMessage(loadError)}</p>
                  <button type="button" onClick={retryLoad} className={cx(QUIET_ACTION, "mt-2 text-xs")}>
                    Try again
                  </button>
                </div>
              ) : accounts === null ? (
                <AccountsSkeleton />
              ) : accounts.length === 0 ? (
                <div
                  className="border border-border bg-surface-2 px-4 py-6 text-center"
                  data-testid="withdraw-no-accounts"
                >
                  <Landmark className="mx-auto size-6 text-muted" aria-hidden />
                  <p className="mt-3 text-sm font-semibold">No linked bank account yet</p>
                  <p className="mx-auto mt-1 max-w-xs text-xs leading-relaxed text-muted">
                    Withdrawals go to a bank account in your own name. Linking one from the app is coming soon.
                  </p>
                </div>
              ) : (
                <fieldset>
                  <legend className="sr-only">Send to</legend>
                  <div className="space-y-2">
                    {accounts.map((account) => (
                      <AccountOption
                        key={account.id}
                        account={account}
                        checked={account.id === selectedId}
                        onSelect={(id) => {
                          setSelectedId(id);
                          setError(null);
                        }}
                        disabled={busy}
                      />
                    ))}
                  </div>
                </fieldset>
              )}
              <p className="mt-4 flex items-start gap-2 text-xs leading-relaxed text-muted">
                <ShieldCheck className="mt-0.5 size-3.5 shrink-0 text-synthetic-yellow" aria-hidden />
                <span>Withdrawals go only to bank accounts in your own name, matching your verified identity.</span>
              </p>
            </div>
          ) : null}

          {step === "amount" ? (
            <div data-testid="withdraw-step-amount">
              {selected ? (
                <div className="flex items-center justify-between gap-3 border-b border-border px-5 py-3 text-sm">
                  <span className="min-w-0 truncate text-muted">
                    To <span className="font-medium text-ink">{accountLabel(selected)}</span>
                  </span>
                  <button
                    type="button"
                    onClick={back}
                    disabled={busy}
                    className={cx(QUIET_ACTION, "shrink-0 text-[11px]")}
                  >
                    Change
                  </button>
                </div>
              ) : null}
              <div className="px-5 pt-6 pb-5 text-center">
                <label htmlFor={amountId} className="text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
                  Amount to withdraw
                </label>
                <div className="mx-auto mt-3 flex w-fit max-w-full items-baseline justify-center gap-1.5 px-2 pb-1">
                  <span aria-hidden className="text-3xl font-semibold text-synthetic-yellow">
                    $
                  </span>
                  {/* As in the trading checkout: the field is exactly as wide as its text. */}
                  <span className="grid min-w-0">
                    <span
                      aria-hidden
                      className={cx(AMOUNT_TYPE, "invisible col-start-1 row-start-1 pr-1 whitespace-pre")}
                    >
                      {formatAmountInput(amount)}
                    </span>
                    <input
                      ref={amountRef}
                      id={amountId}
                      value={amount === "" ? "" : formatAmountInput(amount)}
                      onChange={onTypeAmount}
                      placeholder="0"
                      // The numpad below is the keyboard on touch screens; a physical keyboard still types here.
                      inputMode="none"
                      autoComplete="off"
                      spellCheck={false}
                      size={1}
                      aria-describedby={amountHintId}
                      aria-invalid={assessment.problem === "exceeds" || undefined}
                      readOnly={busy}
                      className={cx(
                        AMOUNT_TYPE,
                        "col-start-1 row-start-1 w-0 min-w-full bg-transparent p-0 text-ink caret-synthetic-yellow outline-none placeholder:text-muted/60",
                      )}
                      data-testid="withdraw-amount"
                    />
                  </span>
                  <span className="text-xs font-semibold tracking-[0.16em] text-muted">MXN</span>
                </div>
                <p
                  id={amountHintId}
                  className="mt-3 flex flex-wrap items-center justify-center gap-x-2 text-xs"
                  data-testid="withdraw-amount-hint"
                >
                  {assessment.problem === "exceeds" ? (
                    <span className="font-medium text-danger">More than your available cash.</span>
                  ) : (
                    <span className="text-muted">
                      Available: <span data-testid="withdraw-available">{formatMoney(cash)}</span>
                    </span>
                  )}
                  {assessment.max !== "" ? (
                    <button
                      type="button"
                      onClick={() => editAmount(assessment.max)}
                      disabled={busy}
                      className={QUIET_ACTION}
                      data-testid="withdraw-max"
                    >
                      Max
                    </button>
                  ) : null}
                </p>
                {heldSome ? (
                  <p className="mx-auto mt-2 max-w-xs text-xs leading-relaxed text-muted" data-testid="withdraw-held">
                    {formatMoney(held)} held by your open orders stays with them until they fill or you cancel them.
                  </p>
                ) : null}
              </div>

              {/* Hairline grid: a 1 px gap over the border color draws the keys' edges. */}
              <div
                className="grid grid-cols-3 gap-px border-y border-border bg-border"
                role="group"
                aria-label="Numpad"
              >
                {NUMPAD_KEYS.map((key) => (
                  <button
                    key={key}
                    type="button"
                    onClick={() => editAmount(pressKey(amount, key))}
                    disabled={busy}
                    aria-label={KEY_LABELS[key]}
                    className={cx(
                      "grid h-14 place-items-center bg-surface font-mono text-2xl font-medium text-ink transition-colors sm:h-12",
                      "hover:bg-surface-2 active:bg-synthetic-yellow active:text-black",
                      "focus-visible:relative focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none focus-visible:ring-inset",
                      "disabled:opacity-40",
                    )}
                  >
                    {key === "back" ? <Delete className="size-6" aria-hidden /> : key}
                  </button>
                ))}
              </div>
            </div>
          ) : null}

          {step === "review" && selected ? (
            <div className="px-5 pt-6" data-testid="withdraw-step-review">
              <p className="text-center text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
                You withdraw
              </p>
              <p className="mt-2 flex items-baseline justify-center gap-1.5" data-testid="withdraw-review-amount">
                <span aria-hidden className="text-3xl font-semibold text-synthetic-yellow">
                  $
                </span>
                <span className={AMOUNT_TYPE}>{formatAmount(assessment.amount, 2)}</span>
                <span className="text-xs font-semibold tracking-[0.16em] text-muted">MXN</span>
              </p>
              <dl className="mt-6 space-y-2.5 border-y border-border py-4 text-sm" data-testid="withdraw-summary">
                <SummaryRow label="To" value={accountLabel(selected)} />
                <SummaryRow label="Account holder" value={selected.holder_name} />
                <SummaryRow label="Transfer" value="SPEI" />
                <SummaryRow label="Fee" value="$0.00" strong />
                <SummaryRow label="Available after" value={formatMoney(assessment.remaining)} testId="withdraw-after" />
              </dl>
              <p className="mt-4 text-center text-xs leading-relaxed text-muted">
                It leaves your available cash now and shows as processing until the SPEI network settles it. If the bank
                returns it, it comes back to your vault.
              </p>
            </div>
          ) : null}

          {error !== null ? (
            <p
              role="alert"
              className="mx-5 mt-4 border border-danger/40 bg-danger-soft px-3 py-2.5 text-sm text-danger"
            >
              {errorMessage(error)}
            </p>
          ) : null}

          {/* The actions stay in view while a long step scrolls beneath them. */}
          <div className="sticky bottom-0 mt-4 flex gap-2 border-t border-border bg-surface px-5 pt-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))]">
            {step === "account" ? null : (
              <button
                type="button"
                onClick={back}
                disabled={busy}
                aria-label="Back"
                className="grid size-14 shrink-0 place-items-center border border-border-strong text-ink transition-colors hover:bg-surface-2 focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none disabled:opacity-40"
                data-testid="withdraw-back"
              >
                <ArrowLeft className="size-5" aria-hidden />
              </button>
            )}
            {step === "review" ? (
              <button type="submit" disabled={!canContinue || busy} className={PRIMARY} data-testid="withdraw-confirm">
                {busy ? <Loader2 className="size-5 animate-spin" aria-hidden /> : null}
                {busy ? "Sending…" : "Confirm withdrawal"}
              </button>
            ) : (
              <button type="submit" disabled={!canContinue} className={PRIMARY} data-testid="withdraw-continue">
                Continue
              </button>
            )}
          </div>
        </form>
      )}
    </dialog>
  );
}

/* ------------------------------------------------------------------------ */

function Progress({ step }: { step: EntryStep }) {
  const index = STEPS.findIndex((s) => s.step === step);
  return (
    <ol className="mt-3.5 grid grid-cols-3 gap-1.5" aria-label="Withdrawal steps">
      {STEPS.map((s, i) => (
        <li key={s.step} aria-current={i === index ? "step" : undefined} className="min-w-0">
          <span aria-hidden className={cx("block h-0.5", i <= index ? "bg-synthetic-yellow" : "bg-surface-2")} />
          <span
            className={cx(
              "mt-1.5 block truncate text-[10px] font-semibold tracking-[0.18em] uppercase",
              i === index ? "text-ink" : i < index ? "text-synthetic-yellow" : "text-muted",
            )}
          >
            <span className="sr-only">{`Step ${i + 1} of ${STEPS.length}: `}</span>
            {s.label}
          </span>
        </li>
      ))}
    </ol>
  );
}

function AccountOption({
  account,
  checked,
  onSelect,
  disabled,
}: {
  account: LinkedAccount;
  checked: boolean;
  onSelect: (id: string) => void;
  disabled: boolean;
}) {
  return (
    <label
      className={cx(
        "flex cursor-pointer items-start gap-3.5 border p-4 transition-colors",
        "has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-synthetic-yellow",
        checked ? "border-synthetic-yellow bg-synthetic-yellow/[0.05]" : "border-border hover:border-border-strong",
      )}
      data-testid="withdraw-account"
      data-checked={checked}
    >
      <input
        type="radio"
        name="linked-account"
        value={account.id}
        checked={checked}
        onChange={() => onSelect(account.id)}
        disabled={disabled}
        className="sr-only"
      />
      <span
        aria-hidden
        className="grid size-10 shrink-0 place-items-center border border-border bg-surface-2 text-synthetic-yellow"
      >
        <Landmark className="size-5" />
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex items-baseline justify-between gap-3">
          <span className="truncate font-semibold">{bankName(account)}</span>
          <span className="shrink-0 font-mono text-sm text-ink-2 tabular-nums" data-testid="withdraw-account-clabe">
            {maskedClabe(account)}
          </span>
        </span>
        <span className="mt-1 block truncate text-sm text-ink-2" data-testid="withdraw-account-holder">
          {account.holder_name}
        </span>
        <span className="mt-2.5 inline-flex items-center gap-1.5 text-[10px] font-semibold tracking-[0.18em] text-gain uppercase">
          <BadgeCheck className="size-3.5" aria-hidden />
          In your name
        </span>
      </span>
      <span
        aria-hidden
        className={cx(
          "mt-0.5 grid size-5 shrink-0 place-items-center border",
          checked ? "border-synthetic-yellow bg-synthetic-yellow text-black" : "border-border-strong",
        )}
      >
        {checked ? <Check className="size-3.5" strokeWidth={3} /> : null}
      </span>
    </label>
  );
}

function AccountsSkeleton() {
  return (
    <div role="status">
      <span className="sr-only">Loading your linked accounts…</span>
      <div aria-hidden className="flex items-start gap-3.5 border border-border p-4 motion-safe:animate-pulse">
        <div className="size-10 bg-surface-2" />
        <div className="flex-1 space-y-2">
          <div className="h-4 w-40 bg-surface-2" />
          <div className="h-3.5 w-28 bg-surface-2" />
          <div className="h-3 w-20 bg-surface-2" />
        </div>
      </div>
    </div>
  );
}

function SummaryRow({
  label,
  value,
  strong = false,
  mono = false,
  testId,
}: {
  label: string;
  value: string;
  strong?: boolean;
  mono?: boolean;
  testId?: string;
}) {
  return (
    <div className="flex items-baseline justify-between gap-4" data-testid={testId}>
      <dt className="shrink-0 text-muted">{label}</dt>
      <dd
        className={cx(
          "min-w-0 text-right tabular-nums",
          mono && "font-mono text-xs break-all",
          strong ? "font-semibold text-ink" : "text-ink-2",
        )}
      >
        {value}
      </dd>
    </div>
  );
}

function Receipt({
  withdrawal,
  doneRef,
  onDone,
}: {
  withdrawal: Withdrawal;
  doneRef: RefObject<HTMLButtonElement | null>;
  onDone: () => void;
}) {
  const { status, amount, linked_account } = withdrawal;
  const to = accountLabel(linked_account);
  const sentence =
    status === "processing"
      ? `${formatMoney(amount)} is on its way to ${to}.`
      : status === "settled"
        ? `${formatMoney(amount)} reached ${to}.`
        : `${formatMoney(amount)} came back to your vault.`;
  return (
    <div
      className="px-5 pt-8 pb-[calc(1.25rem+env(safe-area-inset-bottom))] text-center"
      data-testid="withdraw-receipt"
      data-status={status}
    >
      <span
        aria-hidden
        className={cx(
          "mx-auto grid size-12 place-items-center",
          status === "settled"
            ? "bg-synthetic-yellow text-black"
            : status === "processing"
              ? "border border-synthetic-yellow/60 text-synthetic-yellow"
              : "border border-border-strong text-muted",
        )}
      >
        {status === "settled" ? (
          <Check className="size-6" strokeWidth={2.5} />
        ) : status === "processing" ? (
          <Clock className="size-6" />
        ) : (
          <Undo2 className="size-6" />
        )}
      </span>
      <p role="status" className="mt-4 text-lg font-semibold tracking-tight">
        {receiptTitle(withdrawal)}
      </p>
      <p className="mt-1 text-sm text-muted">{sentence}</p>
      <dl className="mt-6 space-y-2.5 border-y border-border py-4 text-left text-sm">
        <SummaryRow label="Amount" value={formatMoney(amount)} strong />
        <SummaryRow label="To" value={to} />
        <SummaryRow label="Status" value={STATUS_LABELS[status]} testId="withdraw-status" />
        <SummaryRow label="SPEI tracking key" value={withdrawal.tracking_key} mono testId="withdraw-tracking-key" />
      </dl>
      <p className="mt-4 text-xs leading-relaxed text-muted">
        {status === "processing"
          ? "Your bank can find it with the tracking key. If it is returned, the money comes back to your vault."
          : "The tracking key identifies the transfer at both banks."}
      </p>
      <button
        ref={doneRef}
        type="button"
        onClick={onDone}
        className="mt-6 h-14 w-full bg-synthetic-yellow text-base font-bold tracking-[0.18em] text-black uppercase transition-colors hover:bg-synthetic-yellow-hover focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none"
        data-testid="withdraw-done"
      >
        Done
      </button>
    </div>
  );
}
