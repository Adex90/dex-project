"use client";

import { useEffect, useId, useRef, useState, type ChangeEvent, type FormEvent, type RefObject } from "react";
import { Check, Clock, Delete, Loader2, X } from "lucide-react";

import { SegmentedControl } from "@/components/ui/segmented-control";
import { cx } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { newIdempotencyKey } from "@/lib/api/endpoints";
import { formatMoney } from "@/lib/decimal";
import {
  LIVE_ORDER_TYPES,
  NUMPAD_KEYS,
  PLAN_FREQUENCIES,
  PRICE_DECIMALS,
  amountForApi,
  assessOrder,
  buildOrder,
  draftSignature,
  formatAmountInput,
  formatPrice,
  monthlyEquivalent,
  orderTypesFor,
  pressKey,
  priceEntry,
  sanitizeAmount,
  smallestUnit,
  type NumpadKey,
  type OrderAssessment,
  type OrderDraft,
} from "@/lib/market";
import { formatShares } from "@/lib/vault";
import type { MarketAsset, OrderReceipt, OrderRequest, OrderSide, OrderType, PlanFrequency } from "@/types/market";
import type { DecimalString } from "@/types/vault";

/*
 * Order Checkout Drawer: the order's type, its amount (and price, or
 * frequency), what it buys, and the confirmation, in the Sovereign Gold
 * identity: square, black and charcoal, one gold action.
 *
 * - The type comes first: Market, Limit, Stop and, to buy, Savings plan. A
 *   limit or stop adds its price, typed on the same numpad as the amount
 *   (the field it types into is underlined in gold). A savings plan asks how
 *   often instead, and shows what it adds up to in a month.
 * - Only `liveTypes` can be placed (today, market orders: lib/market.ts).
 *   The others can be set up and previewed, estimates included; their button
 *   stays disabled and a note says why, so nothing is ever sent for a type
 *   the backend does not execute.
 * - A native <dialog> opened as a modal: focus stays inside, the page behind
 *   is inert, Escape closes, and closing returns focus to the button that
 *   opened it.
 * - On a phone it is a bottom sheet with a large numpad (the fields ask for
 *   no on-screen keyboard); on a desktop, a centered panel where the keyboard
 *   types into the focused field. Enter confirms. When the order is taller
 *   than the sheet, it scrolls under the gold button, which stays in view.
 * - Amounts are exact (lib/market): the estimate rounds the quantity down, so
 *   it never promises more than the money buys. The commission, $0.00, has
 *   its own row.
 * - One idempotency key per order: a retry after an error reuses it, so a
 *   network failure can never place the same order twice; changing anything
 *   about the order draws a new one.
 */

export interface OrderCheckoutDrawerProps {
  open: boolean;
  onClose: () => void;
  asset: Pick<MarketAsset, "ticker" | "name" | "price" | "quantity_decimals">;
  side: OrderSide;
  /** The vault's available cash, MXN: what a buy draws on. */
  cash: DecimalString;
  /** Units held ("0" for none): what a sale draws on, valued at the order's price. */
  held: DecimalString;
  /** The types that can be placed; the others can only be previewed. */
  liveTypes?: readonly OrderType[];
  /** Places a live order: resolves with the receipt, or rejects with the error to show. */
  onConfirm: (order: OrderRequest) => Promise<OrderReceipt>;
}

export function OrderCheckoutDrawer(props: OrderCheckoutDrawerProps) {
  // Mounted only while open: every opening starts from a market order and an empty amount.
  return props.open ? <CheckoutDialog {...props} /> : null;
}

type Phase = "entry" | "submitting" | "done";
type Field = "amount" | "price";

const KEY_LABELS: Partial<Record<NumpadKey, string>> = { ".": "Decimal point", back: "Delete last digit" };

/** The amount's type: the input and the invisible copy that sizes it must match to the pixel. */
const AMOUNT_TYPE = "font-mono text-5xl font-semibold tracking-tight tabular-nums";

const TYPE_LABELS: Record<OrderType, string> = {
  market: "Market",
  limit: "Limit",
  stop: "Stop",
  savings_plan: "Savings plan",
};

/** In a sentence: "Limit orders are not live yet." */
const TYPE_PLURALS: Record<OrderType, string> = {
  market: "Market orders",
  limit: "Limit orders",
  stop: "Stop orders",
  savings_plan: "Savings plans",
};

const FREQUENCY_LABELS: Record<PlanFrequency, string> = {
  weekly: "Weekly",
  biweekly: "Every 2 weeks",
  monthly: "Monthly",
};

const FREQUENCY_PHRASES: Record<PlanFrequency, string> = {
  weekly: "every week",
  biweekly: "every two weeks",
  monthly: "every month",
};

const QUIET_ACTION =
  "font-semibold tracking-[0.16em] text-synthetic-yellow uppercase hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none";

function CheckoutDialog({
  onClose,
  asset,
  side,
  cash,
  held,
  liveTypes = LIVE_ORDER_TYPES,
  onConfirm,
}: Omit<OrderCheckoutDrawerProps, "open">) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const amountRef = useRef<HTMLInputElement>(null);
  const doneRef = useRef<HTMLButtonElement>(null);
  const attempt = useRef<{ signature: string; key: string } | null>(null);
  const titleId = useId();
  const amountId = useId();
  const amountHintId = useId();
  const priceId = useId();
  const priceHintId = useId();
  const notLiveId = useId();
  const [type, setType] = useState<OrderType>("market");
  const [amount, setAmount] = useState("");
  const [price, setPrice] = useState("");
  const [frequency, setFrequency] = useState<PlanFrequency>("monthly");
  const [active, setActive] = useState<Field>("amount");
  const [phase, setPhase] = useState<Phase>("entry");
  const [error, setError] = useState<unknown>(null);
  const [receipt, setReceipt] = useState<OrderReceipt | null>(null);

  const buying = side === "buy";
  const verb = buying ? "Buy" : "Sell";
  const busy = phase === "submitting";
  const priced = type === "limit" || type === "stop";
  const plan = type === "savings_plan";
  const live = liveTypes.includes(type);
  const draft: OrderDraft = { type, side, amount, price, frequency };
  const order = assessOrder(draft, { current: asset.price, decimals: asset.quantity_decimals, cash, held });
  // With a price to type too, the numpad types into the field underlined in gold.
  const target: Field = priced ? active : "amount";

  // A modal from the first frame; the page behind does not scroll meanwhile.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    dialog.showModal();
    amountRef.current?.focus();
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

  function editAmount(next: string) {
    if (busy || next === amount) return;
    setAmount(next);
    setError(null);
  }

  function editPrice(next: string) {
    if (busy || next === price) return;
    setPrice(next);
    setError(null);
  }

  /** A limit or a stop needs its price next: the numpad moves to it (the focus stays on the type). */
  function chooseType(next: OrderType) {
    if (busy) return;
    setType(next);
    setActive(next === "limit" || next === "stop" ? "price" : "amount");
    setError(null);
  }

  function press(key: NumpadKey) {
    if (target === "price") editPrice(pressKey(price, key, PRICE_DECIMALS));
    else editAmount(pressKey(amount, key));
  }

  function onTypeAmount(event: ChangeEvent<HTMLInputElement>) {
    const next = sanitizeAmount(event.target.value);
    if (next !== null) editAmount(next);
  }

  function onTypePrice(event: ChangeEvent<HTMLInputElement>) {
    const next = sanitizeAmount(event.target.value, PRICE_DECIMALS);
    if (next !== null) editPrice(next);
  }

  async function confirm(event: FormEvent) {
    event.preventDefault();
    if (!live || !order.ready || busy) return;
    // The same order retried keeps its key; anything changed is a new order.
    const signature = draftSignature(draft);
    if (attempt.current?.signature !== signature) attempt.current = { signature, key: newIdempotencyKey() };
    setPhase("submitting");
    setError(null);
    try {
      setReceipt(await onConfirm(buildOrder(draft, asset.ticker, attempt.current.key)));
      setPhase("done");
    } catch (cause) {
      setError(cause);
      setPhase("entry");
    }
  }

  const quantity =
    order.quantity === null ? "—" : `${formatShares(order.quantity, asset.quantity_decimals)} ${asset.ticker}`;
  const amountProblem = amountHint(order, buying, priced, asset.ticker, asset.quantity_decimals);

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
          <div className="border-b border-border px-5 py-3">
            <SegmentedControl
              legend="Order type"
              options={orderTypesFor(side).map((value) => ({ value, label: TYPE_LABELS[value] }))}
              value={type}
              onChange={chooseType}
              disabled={busy}
              testId="order-type"
            />
          </div>

          <div className={cx("px-5 text-center", priced || plan ? "pt-5 pb-4" : "pt-6 pb-5")}>
            <label htmlFor={amountId} className="text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
              {plan ? "Each purchase" : buying ? "Amount to invest" : "Amount to sell"}
            </label>
            <div
              className={cx(
                "mx-auto mt-3 flex w-fit max-w-full items-baseline justify-center gap-1.5 border-b-2 px-2 pb-1 transition-colors",
                priced && target === "amount" ? "border-synthetic-yellow" : "border-transparent",
              )}
            >
              <span aria-hidden className="text-3xl font-semibold text-synthetic-yellow">
                $
              </span>
              {/*
                The field is exactly as wide as its text, so "MXN" sits right after
                the number: an invisible copy of the text sizes the grid cell and the
                input fills it. With no width of its own (w-0 min-w-full), the input
                never widens the cell, even for a single "0".
              */}
              <span className="grid min-w-0">
                <span aria-hidden className={cx(AMOUNT_TYPE, "invisible col-start-1 row-start-1 pr-1 whitespace-pre")}>
                  {formatAmountInput(amount)}
                </span>
                <input
                  ref={amountRef}
                  id={amountId}
                  value={amount === "" ? "" : formatAmountInput(amount)}
                  onChange={onTypeAmount}
                  onFocus={() => setActive("amount")}
                  placeholder="0"
                  // The numpad below is the keyboard on touch screens; a physical keyboard still types here.
                  inputMode="none"
                  autoComplete="off"
                  spellCheck={false}
                  size={1}
                  aria-describedby={amountHintId}
                  aria-invalid={(order.amountProblem !== null && order.amountProblem !== "empty") || undefined}
                  readOnly={busy}
                  className={cx(
                    AMOUNT_TYPE,
                    "col-start-1 row-start-1 w-0 min-w-full bg-transparent p-0 text-ink caret-synthetic-yellow outline-none placeholder:text-muted/60",
                  )}
                  data-testid="order-amount"
                />
              </span>
              <span className="text-xs font-semibold tracking-[0.16em] text-muted">MXN</span>
            </div>
            <p id={amountHintId} className="mt-3 flex flex-wrap items-center justify-center gap-x-2 text-xs">
              {amountProblem !== null ? (
                <span className="font-medium text-danger">{amountProblem}</span>
              ) : plan ? (
                <span className="text-muted">Taken from your vault&apos;s cash at each purchase.</span>
              ) : (
                <span className="text-muted">
                  {buying ? "Available" : priced && order.price !== null ? `Position at ${formatPrice(order.price)}` : "Position"}:{" "}
                  {formatMoney(order.available ?? "0")}
                </span>
              )}
              {order.max !== "" ? (
                <button
                  type="button"
                  onClick={() => {
                    editAmount(order.max);
                    setActive("amount");
                  }}
                  disabled={busy}
                  className={QUIET_ACTION}
                >
                  Max
                </button>
              ) : null}
            </p>
          </div>

          {priced ? (
            <div className="border-t border-border px-5 py-4" data-testid="order-price-field">
              <div className="flex items-baseline justify-between gap-3">
                <label htmlFor={priceId} className="text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
                  {type === "limit" ? "Limit price" : "Stop price"}
                </label>
                <span className="text-xs text-muted tabular-nums">Now {formatPrice(asset.price)}</span>
              </div>
              <div
                className={cx(
                  "mt-2 flex items-baseline gap-1.5 border-b-2 pb-1 transition-colors",
                  target === "price" ? "border-synthetic-yellow" : "border-border",
                )}
              >
                <span aria-hidden className="text-xl font-semibold text-synthetic-yellow">
                  $
                </span>
                <input
                  id={priceId}
                  value={price === "" ? "" : formatAmountInput(price)}
                  onChange={onTypePrice}
                  onFocus={() => setActive("price")}
                  placeholder={formatAmountInput(priceEntry(asset.price))}
                  inputMode="none"
                  autoComplete="off"
                  spellCheck={false}
                  aria-describedby={priceHintId}
                  aria-invalid={order.priceProblem === "wrong_side" || undefined}
                  readOnly={busy}
                  className="min-w-0 flex-1 bg-transparent p-0 font-mono text-2xl font-semibold tracking-tight text-ink tabular-nums caret-synthetic-yellow outline-none placeholder:text-muted/50"
                  data-testid="order-price"
                />
                <span className="text-xs font-semibold tracking-[0.16em] text-muted">MXN</span>
              </div>
              <p id={priceHintId} className="mt-2 flex flex-wrap items-center gap-x-2 text-xs">
                {order.priceProblem === "wrong_side" ? (
                  <span className="font-medium text-danger">
                    {buying ? "To buy, a stop must be above the current price." : "To sell, a stop must be below the current price."}
                  </span>
                ) : (
                  <span className="text-muted">{priceHint(type, buying)}</span>
                )}
                {type === "limit" ? (
                  <button
                    type="button"
                    onClick={() => {
                      editPrice(priceEntry(asset.price));
                      setActive("price");
                    }}
                    disabled={busy}
                    className={QUIET_ACTION}
                  >
                    Use current
                  </button>
                ) : null}
              </p>
            </div>
          ) : null}

          {plan ? (
            <div className="border-t border-border px-5 py-4">
              <p aria-hidden className="text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
                How often
              </p>
              <SegmentedControl
                legend="How often"
                options={PLAN_FREQUENCIES.map((value) => ({ value, label: FREQUENCY_LABELS[value] }))}
                value={frequency}
                onChange={(next) => {
                  setFrequency(next);
                  setError(null);
                }}
                disabled={busy}
                className="mt-2"
                testId="order-frequency"
              />
            </div>
          ) : null}

          {/* Hairline grid: a 1 px gap over the border color draws the keys' edges. */}
          <div
            className="grid grid-cols-3 gap-px border-y border-border bg-border"
            role="group"
            aria-label={priced ? `Numpad, typing the ${target === "price" ? "price" : "amount"}` : "Numpad"}
          >
            {NUMPAD_KEYS.map((key) => (
              <button
                key={key}
                type="button"
                onClick={() => press(key)}
                disabled={busy}
                aria-label={KEY_LABELS[key]}
                className={cx(
                  "grid h-12 place-items-center bg-surface font-mono text-2xl font-medium text-ink transition-colors",
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
            {plan ? (
              <>
                <SummaryRow label="Each purchase" value={formatMoney(amountForApi(amount))} />
                <SummaryRow label="Frequency" value={FREQUENCY_LABELS[frequency]} />
                <SummaryRow label="Quantity (today's price)" value={quantity} />
                <SummaryRow label="Commission" value="$0.00" strong />
                <div className="border-t border-border pt-2.5">
                  <SummaryRow label="About a month" value={formatMoney(monthlyEquivalent(amount, frequency))} strong />
                </div>
              </>
            ) : (
              <>
                <SummaryRow
                  label={type === "limit" ? "Limit price" : type === "stop" ? "Stop price" : "Price (estimate)"}
                  value={order.price === null ? "—" : formatPrice(order.price)}
                />
                <SummaryRow label={type === "limit" ? "Quantity (at the limit)" : "Quantity (estimate)"} value={quantity} />
                <SummaryRow label="Commission" value="$0.00" strong />
                <div className="border-t border-border pt-2.5">
                  <SummaryRow label="Total" value={formatMoney(amountForApi(amount))} strong />
                </div>
              </>
            )}
          </dl>

          {error ? (
            <p role="alert" className="mx-5 mb-4 border border-danger/40 bg-danger-soft px-3 py-2.5 text-sm text-danger">
              {describeError(error)}
            </p>
          ) : null}

          <p className="mx-5 mb-4 text-center text-xs leading-relaxed text-muted" data-testid="order-footnote">
            {footnote(draft, order, asset)}
          </p>

          {/*
            The gold action stays in view while a long order scrolls beneath it,
            and so does the reason it is disabled for a type that is not live.
          */}
          <div className="sticky bottom-0 border-t border-border bg-surface px-5 pt-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))]">
            {!live ? (
              <p
                id={notLiveId}
                className="mb-2.5 flex items-start justify-center gap-2 text-center text-xs leading-relaxed text-ink-2"
                data-testid="order-not-live"
              >
                <Clock className="mt-0.5 size-3.5 shrink-0 text-synthetic-yellow" aria-hidden />
                <span>
                  {TYPE_PLURALS[type]} are not live yet: nothing is {plan ? "scheduled" : "placed"}.
                </span>
              </p>
            ) : null}
            <button
              type="submit"
              disabled={!live || !order.ready || busy}
              aria-describedby={live ? undefined : notLiveId}
              className={cx(
                "inline-flex h-14 w-full items-center justify-center gap-2 bg-synthetic-yellow text-base font-bold tracking-[0.18em] text-black uppercase transition-colors",
                "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
                "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
                "disabled:cursor-not-allowed disabled:opacity-40",
              )}
              data-testid="order-confirm"
            >
              {busy ? <Loader2 className="size-5 animate-spin" aria-hidden /> : null}
              {busy ? "Placing order…" : confirmLabel(type, buying)}
            </button>
          </div>
        </form>
      )}
    </dialog>
  );
}

/* ------------------------------------------------------------------------ */

function confirmLabel(type: OrderType, buying: boolean): string {
  switch (type) {
    case "market":
      return "Confirm Order";
    case "limit":
      return "Place Limit Order";
    case "stop":
      return buying ? "Place Stop Order" : "Place Stop-Loss";
    case "savings_plan":
      return "Create Savings Plan";
  }
}

/** Why the amount cannot be placed, in a sentence; null when it can (or is still empty). */
function amountHint(
  order: OrderAssessment,
  buying: boolean,
  priced: boolean,
  ticker: string,
  decimals: number,
): string | null {
  switch (order.amountProblem) {
    case "exceeds":
      if (buying) return "More than your available cash.";
      return priced ? "More than your position is worth at this price." : "More than your position is worth.";
    case "too_small":
      return `Less than the smallest unit (${smallestUnit(decimals)} ${ticker}) at this price.`;
    default:
      return null;
  }
}

function priceHint(type: OrderType, buying: boolean): string {
  if (type === "limit") return buying ? "Buys only at this price or lower." : "Sells only at this price or higher.";
  return buying ? "Triggers when the price rises to it." : "Triggers when the price falls to it.";
}

/** What the order will do, under the button. */
function footnote(
  draft: OrderDraft,
  order: OrderAssessment,
  asset: Pick<MarketAsset, "ticker" | "price">,
): string {
  const buying = draft.side === "buy";
  const at = order.price === null ? null : formatPrice(order.price);
  switch (draft.type) {
    case "market":
      return "Market order: it fills at the next available price, which may differ slightly from the estimate.";
    case "limit":
      if (order.marketable) {
        return `The price is already at your limit: it would fill at once, at ${formatPrice(asset.price)} or better.`;
      }
      return `Limit order: it fills only at ${at ?? "your price"} or ${buying ? "lower" : "higher"}, and may never fill.`;
    case "stop":
      return buying
        ? `Stop order: when the price rises to ${at ?? "your stop"}, it becomes a market order and fills at the next available price.`
        : `Stop-loss: when the price falls to ${at ?? "your stop"}, it becomes a market order and fills at the next available price, which may be lower.`;
    case "savings_plan": {
      const each = order.amountProblem === "empty" ? "your amount" : formatMoney(amountForApi(draft.amount));
      return `Savings plan: buys ${each} of ${asset.ticker} ${FREQUENCY_PHRASES[draft.frequency]} at the price of the day, from your vault's cash, with no commission.`;
    }
  }
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
  return (
    <div className="px-5 pt-8 pb-[calc(1.25rem+env(safe-area-inset-bottom))] text-center" data-testid="order-receipt">
      <span aria-hidden className="mx-auto grid size-12 place-items-center bg-synthetic-yellow text-black">
        <Check className="size-6" strokeWidth={2.5} />
      </span>
      <p role="status" className="mt-4 text-lg font-semibold tracking-tight">
        Order filled
      </p>
      <p className="mt-1 text-sm text-muted">
        {`${buying ? "Bought" : "Sold"} ${formatShares(receipt.quantity, decimals)} ${ticker} at ${formatMoney(receipt.price)}.`}
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
