"use client";

import { useEffect, useId, useRef, useState, type ChangeEvent, type FormEvent, type RefObject } from "react";
import { Check, Delete, Loader2, X } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { newIdempotencyKey } from "@/lib/api/endpoints";
import { formatMoney } from "@/lib/decimal";
import {
  NUMPAD_KEYS,
  amountForApi,
  amountUnits,
  estimateQuantity,
  estimateSale,
  formatAmountInput,
  maxAmount,
  orderProblem,
  pressKey,
  sanitizeAmount,
  type NumpadKey,
} from "@/lib/market";
import { formatShares } from "@/lib/vault";
import type { MarketAsset, OrderReceipt, OrderRequest, OrderSide } from "@/types/market";
import type { DecimalString } from "@/types/vault";

/*
 * Order Checkout Drawer: the amount, what it buys, and the confirmation, in
 * the Sovereign Gold identity: square, black and charcoal, one gold action.
 *
 * - A native <dialog> opened as a modal: focus stays inside, the page behind
 *   is inert, Escape closes, and closing returns focus to the button that
 *   opened it.
 * - On a phone it is a bottom sheet with a large numpad (the field asks for no
 *   on-screen keyboard); on a desktop, a centered panel where the keyboard
 *   types into the same field. Enter confirms.
 * - Amounts are exact (lib/market): the estimate rounds the quantity down, so
 *   it never promises more than the money buys. The summary states the
 *   commission, $0.00, in its own row.
 * - One idempotency key per order: a retry after an error reuses it, so a
 *   network failure can never place the same order twice.
 */

export interface OrderCheckoutDrawerProps {
  open: boolean;
  onClose: () => void;
  asset: Pick<MarketAsset, "ticker" | "name" | "price" | "quantity_decimals">;
  side: OrderSide;
  /** What the order can draw on, MXN: available cash to buy, the position's value to sell. */
  available: DecimalString;
  /** Units held, for a sale: an amount that reaches their whole value sells them all (the estimate says so). */
  held?: DecimalString;
  /** Places the order: resolves with the receipt, or rejects with the error to show. */
  onConfirm: (order: OrderRequest) => Promise<OrderReceipt>;
}

export function OrderCheckoutDrawer(props: OrderCheckoutDrawerProps) {
  // Mounted only while open: every opening starts from an empty amount.
  return props.open ? <CheckoutDialog {...props} /> : null;
}

type Phase = "entry" | "submitting" | "done";

const KEY_LABELS: Partial<Record<NumpadKey, string>> = { ".": "Decimal point", back: "Delete last digit" };

/** The amount's type: the input and the invisible copy that sizes it must match to the pixel. */
const AMOUNT_TYPE = "font-mono text-5xl font-semibold tracking-tight tabular-nums";

function CheckoutDialog({ onClose, asset, side, available, held, onConfirm }: Omit<OrderCheckoutDrawerProps, "open">) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const doneRef = useRef<HTMLButtonElement>(null);
  const attempt = useRef<{ amount: string; key: string } | null>(null);
  const titleId = useId();
  const amountId = useId();
  const hintId = useId();
  const [amount, setAmount] = useState("");
  const [phase, setPhase] = useState<Phase>("entry");
  const [error, setError] = useState<unknown>(null);
  const [receipt, setReceipt] = useState<OrderReceipt | null>(null);

  const buying = side === "buy";
  const verb = buying ? "Buy" : "Sell";
  const busy = phase === "submitting";
  const problem = orderProblem(amount, available);
  const units = amountUnits(amount);
  const quantity =
    !buying && held !== undefined
      ? estimateSale(units, held, asset.price, asset.quantity_decimals)
      : estimateQuantity(units, asset.price, asset.quantity_decimals);
  const max = maxAmount(available);

  // A modal from the first frame; the page behind does not scroll meanwhile.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    dialog.showModal();
    inputRef.current?.focus();
    const root = document.documentElement;
    const overflow = root.style.overflow;
    root.style.overflow = "hidden";
    return () => {
      root.style.overflow = overflow;
      if (dialog.open) dialog.close();
    };
  }, []);

  useEffect(() => {
    if (phase === "done") doneRef.current?.focus();
  }, [phase]);

  /** Closing goes through the dialog, which hands focus back; its "close" event unmounts us. */
  function requestClose() {
    if (!busy) dialogRef.current?.close();
  }

  function edit(next: string) {
    if (busy || next === amount) return;
    setAmount(next);
    setError(null);
  }

  function onType(event: ChangeEvent<HTMLInputElement>) {
    const next = sanitizeAmount(event.target.value);
    if (next !== null) edit(next);
  }

  async function confirm(event: FormEvent) {
    event.preventDefault();
    if (problem !== null || busy) return;
    const apiAmount = amountForApi(amount);
    // The same order retried keeps its key; a different amount is a new order.
    if (attempt.current?.amount !== apiAmount) attempt.current = { amount: apiAmount, key: newIdempotencyKey() };
    setPhase("submitting");
    setError(null);
    try {
      const result = await onConfirm({
        ticker: asset.ticker,
        side,
        amount: apiAmount,
        idempotency_key: attempt.current.key,
      });
      setReceipt(result);
      setPhase("done");
    } catch (cause) {
      setError(cause);
      setPhase("entry");
    }
  }

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onCancel={(event) => {
        // Escape: not while an order is in flight.
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
      data-testid="order-checkout"
    >
      <header className="flex items-start justify-between gap-4 border-b border-border px-5 py-4">
        <div className="min-w-0">
          <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
            {verb} · {asset.ticker}
          </p>
          <h2 id={titleId} className="mt-1 truncate text-base font-semibold tracking-tight">
            {asset.name}
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
      </header>

      {phase === "done" && receipt ? (
        <Receipt
          receipt={receipt}
          ticker={asset.ticker}
          buying={buying}
          decimals={asset.quantity_decimals}
          doneRef={doneRef}
          onDone={requestClose}
        />
      ) : (
        <form onSubmit={confirm} noValidate>
          <div className="px-5 pt-6 pb-5 text-center">
            <label htmlFor={amountId} className="text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
              {buying ? "Amount to invest" : "Amount to sell"}
            </label>
            <div className="mt-3 flex items-baseline justify-center gap-1.5">
              <span aria-hidden className="text-3xl font-semibold text-synthetic-yellow">
                $
              </span>
              {/*
                The field is exactly as wide as its text, so "MXN" sits right after
                the number: an invisible copy of the text sizes the grid cell and the
                input fills it (size={1} keeps the input from widening the cell).
              */}
              <span className="grid">
                <span aria-hidden className={cx(AMOUNT_TYPE, "invisible col-start-1 row-start-1 pr-1 whitespace-pre")}>
                  {formatAmountInput(amount)}
                </span>
                <input
                  ref={inputRef}
                  id={amountId}
                  value={amount === "" ? "" : formatAmountInput(amount)}
                  onChange={onType}
                  placeholder="0"
                  // The numpad below is the keyboard on touch screens; a physical keyboard still types here.
                  inputMode="none"
                  autoComplete="off"
                  spellCheck={false}
                  size={1}
                  aria-describedby={hintId}
                  aria-invalid={problem === "exceeds" || undefined}
                  readOnly={busy}
                  className={cx(
                    AMOUNT_TYPE,
                    "col-start-1 row-start-1 w-full min-w-0 bg-transparent p-0 text-ink caret-synthetic-yellow outline-none placeholder:text-muted/60",
                  )}
                  data-testid="order-amount"
                />
              </span>
              <span className="text-xs font-semibold tracking-[0.16em] text-muted">MXN</span>
            </div>
            <p id={hintId} className="mt-3 flex flex-wrap items-center justify-center gap-x-2 text-xs">
              {problem === "exceeds" ? (
                <span className="font-medium text-danger">
                  {buying ? "More than your available cash." : "More than your position is worth."}
                </span>
              ) : (
                <span className="text-muted">
                  {buying ? "Available" : "Position"}: {formatMoney(available)}
                </span>
              )}
              {max !== "" ? (
                <button
                  type="button"
                  onClick={() => edit(max)}
                  disabled={busy}
                  className="font-semibold tracking-[0.16em] text-synthetic-yellow uppercase hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
                >
                  Max
                </button>
              ) : null}
            </p>
          </div>

          {/* Hairline grid: a 1 px gap over the border color draws the keys' edges. */}
          <div className="grid grid-cols-3 gap-px border-y border-border bg-border" role="group" aria-label="Numpad">
            {NUMPAD_KEYS.map((key) => (
              <button
                key={key}
                type="button"
                onClick={() => edit(pressKey(amount, key))}
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

          <dl className="space-y-2.5 px-5 py-5 text-sm" data-testid="order-summary">
            <SummaryRow label="Price (estimate)" value={formatMoney(asset.price)} />
            <SummaryRow
              label="Quantity (estimate)"
              value={quantity === null ? "—" : `${formatShares(quantity, asset.quantity_decimals)} ${asset.ticker}`}
            />
            <SummaryRow label="Commission" value="$0.00" strong />
            <div className="border-t border-border pt-2.5">
              <SummaryRow label="Total" value={formatMoney(amountForApi(amount))} strong />
            </div>
          </dl>

          {error ? (
            <p role="alert" className="mx-5 mb-4 border border-danger/40 bg-danger-soft px-3 py-2.5 text-sm text-danger">
              {describeError(error)}
            </p>
          ) : null}

          <div className="px-5 pb-[calc(1.25rem+env(safe-area-inset-bottom))]">
            <button
              type="submit"
              disabled={problem !== null || busy}
              className={cx(
                "inline-flex h-14 w-full items-center justify-center gap-2 bg-synthetic-yellow text-base font-bold tracking-[0.18em] text-black uppercase transition-colors",
                "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
                "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
                "disabled:cursor-not-allowed disabled:opacity-40",
              )}
              data-testid="order-confirm"
            >
              {busy ? <Loader2 className="size-5 animate-spin" aria-hidden /> : null}
              {busy ? "Placing order…" : "Confirm Order"}
            </button>
            <p className="mt-3 text-center text-xs leading-relaxed text-muted">
              Market order: it fills at the next available price, which may differ slightly from the estimate.
            </p>
          </div>
        </form>
      )}
    </dialog>
  );
}

function SummaryRow({ label, value, strong = false }: { label: string; value: string; strong?: boolean }) {
  return (
    <div className="flex items-baseline justify-between gap-4">
      <dt className="text-muted">{label}</dt>
      <dd className={cx("text-right tabular-nums", strong ? "font-semibold text-ink" : "text-ink-2")}>{value}</dd>
    </div>
  );
}

function Receipt({
  receipt,
  ticker,
  buying,
  decimals,
  doneRef,
  onDone,
}: {
  receipt: OrderReceipt;
  ticker: string;
  buying: boolean;
  decimals: number;
  doneRef: RefObject<HTMLButtonElement | null>;
  onDone: () => void;
}) {
  const filled = receipt.status === "filled";
  return (
    <div className="px-5 pt-8 pb-[calc(1.25rem+env(safe-area-inset-bottom))] text-center" data-testid="order-receipt">
      <span aria-hidden className="mx-auto grid size-12 place-items-center bg-synthetic-yellow text-black">
        <Check className="size-6" strokeWidth={2.5} />
      </span>
      <p role="status" className="mt-4 text-lg font-semibold tracking-tight">
        {filled ? "Order filled" : "Order received"}
      </p>
      <p className="mt-1 text-sm text-muted">
        {filled
          ? `${buying ? "Bought" : "Sold"} ${formatShares(receipt.quantity, decimals)} ${ticker} at ${formatMoney(receipt.price)}.`
          : "It fills at the next available price."}
      </p>
      <dl className="mt-6 space-y-2.5 border-y border-border py-4 text-left text-sm">
        <SummaryRow label="Amount" value={formatMoney(receipt.amount)} />
        <SummaryRow label="Commission" value={formatMoney(receipt.commission)} strong />
        <SummaryRow label="Order" value={receipt.order_id} />
      </dl>
      <button
        ref={doneRef}
        type="button"
        onClick={onDone}
        className="mt-6 h-14 w-full bg-synthetic-yellow text-base font-bold tracking-[0.18em] text-black uppercase transition-colors hover:bg-synthetic-yellow-hover focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none"
      >
        Done
      </button>
    </div>
  );
}
