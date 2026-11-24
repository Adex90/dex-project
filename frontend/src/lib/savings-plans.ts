/**
 * Savings plans (migration 0023, roadmap item 4): the plans the API answers
 * (`SavingsPlan`, types/market.ts), validated whole, and how the screens
 * describe them: the cadence, the purchase dates in Mexico City time, the
 * last purchase, why one was skipped, and what the active plans add up to in
 * a month. The checkout's receipt and the Vault's Savings plans panel say the
 * same things in the same words.
 *
 * - A plan buys `amount` of its asset at once, then at 9:00 a.m. Mexico City
 *   time every 7 or 14 days, or on the same day of each month; the backend
 *   owns the calendar and sends the next dates (`upcoming`).
 * - A purchase a rule stops is skipped and the plan carries on. Why, in
 *   neutral words (`skipReason`): the client always reads the same quiet
 *   sentence for anything but the reasons they can act on (cash, the market,
 *   the amount, their verification).
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { addUnits, formatMoney, toUnits } from "./decimal.ts";
import { formatPrice, monthlyEquivalent } from "./market.ts";
import { isTicker, parseOrderExecution } from "./market-data.ts";
import { formatShares, isNonNegative, isOneOf, isPositive, isRecord, isText } from "./vault.ts";
import type {
  PlanAction,
  PlanFrequency,
  PlanPurchaseResult,
  SavingsPlan,
  SavingsPlans,
  SavingsPlanStatus,
} from "../types/market.ts";
import type { DecimalString } from "../types/vault.ts";

const STATUSES: readonly SavingsPlanStatus[] = ["active", "paused", "cancelled"];
const FREQUENCIES: readonly PlanFrequency[] = ["weekly", "biweekly", "monthly"];
const RESULTS: readonly PlanPurchaseResult[] = ["executed", "skipped"];
/** A skip reason is an error code: "SALDO_INSUFICIENTE". */
const REASON_RE = /^[A-Z][A-Z0-9_]{0,63}$/;
const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;
/** The backend keeps at most 20 open plans per client; far beyond that is not a list. */
const MAX_PLANS = 200;
/** The backend sends the next purchase and two more. */
const MAX_UPCOMING = 12;

const MEXICO_CITY = "America/Mexico_City";

export const FREQUENCY_LABELS: Readonly<Record<PlanFrequency, string>> = {
  weekly: "Weekly",
  biweekly: "Every 2 weeks",
  monthly: "Monthly",
};

/** After an amount: "$500.00 every week". */
export const FREQUENCY_PHRASES: Readonly<Record<PlanFrequency, string>> = {
  weekly: "every week",
  biweekly: "every two weeks",
  monthly: "every month",
};

/* ------------------------------------------------------------------------ */
/* Validation                                                               */
/* ------------------------------------------------------------------------ */

/** An ISO 8601 instant the browser can read. */
function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isNullable<T>(value: unknown, check: (v: unknown) => v is T): value is T | null {
  return value === null || check(value);
}

/**
 * A savings plan (creating one, an action on one, or a row of the list);
 * null if malformed or incoherent: only an active plan has dates ahead, in
 * order and starting with the next one; a cancelled one says when; the last
 * result agrees with its time, its reason and the purchases counted.
 */
export function parseSavingsPlan(value: unknown): SavingsPlan | null {
  if (!isRecord(value)) return null;
  const { plan_id, order_type, status, ticker, side, amount, frequency, anchor_date } = value;
  const { next_execution_at, upcoming, last_execution_at, last_result, failure_reason } = value;
  const { purchases, skipped, invested, quantity, created_at, cancelled_at } = value;
  const lastPurchase = value.last_purchase === null ? null : parseOrderExecution(value.last_purchase);
  if (
    !isText(plan_id, 64) ||
    order_type !== "savings_plan" ||
    !isOneOf(STATUSES, status) ||
    !isTicker(ticker) ||
    side !== "buy" ||
    !isPositive(amount) ||
    !isOneOf(FREQUENCIES, frequency) ||
    !(typeof anchor_date === "string" && DATE_RE.test(anchor_date)) ||
    !isNullable(next_execution_at, isInstant) ||
    !Array.isArray(upcoming) ||
    upcoming.length > MAX_UPCOMING ||
    !upcoming.every(isInstant) ||
    !isNullable(last_execution_at, isInstant) ||
    !isNullable(last_result, (v): v is PlanPurchaseResult => isOneOf(RESULTS, v)) ||
    !isNullable(failure_reason, (v): v is string => typeof v === "string" && REASON_RE.test(v)) ||
    (value.last_purchase !== null && lastPurchase === null) ||
    !isCount(purchases) ||
    !isCount(skipped) ||
    !isNonNegative(invested) ||
    !isNonNegative(quantity) ||
    !isInstant(created_at) ||
    !isNullable(cancelled_at, isInstant)
  ) {
    return null;
  }
  const dates = upcoming as string[];
  const ascending = dates.every((d, i) => i === 0 || Date.parse(d) > Date.parse(dates[i - 1]!));
  const coherent =
    (status === "active") === (next_execution_at !== null) &&
    (status === "active" ? dates.length > 0 && dates[0] === next_execution_at : dates.length === 0) &&
    ascending &&
    (status === "cancelled") === (cancelled_at !== null) &&
    (last_result === null) === (last_execution_at === null) &&
    (last_result === "skipped") === (failure_reason !== null) &&
    (purchases === 0) === (lastPurchase === null) &&
    (purchases > 0 || (toUnits(invested) === 0n && toUnits(quantity) === 0n)) &&
    (last_result !== "executed" || purchases > 0) &&
    (last_result !== "skipped" || skipped > 0);
  if (!coherent) return null;
  return {
    plan_id,
    order_type,
    status,
    ticker,
    side,
    amount,
    frequency,
    anchor_date,
    next_execution_at,
    upcoming: dates,
    last_execution_at,
    last_result,
    failure_reason,
    last_purchase: lastPurchase,
    purchases,
    skipped,
    invested,
    quantity,
    created_at,
    cancelled_at,
  };
}

/** `GET /api/v1/market/savings-plans`; null if any plan is malformed or repeats. */
export function parseSavingsPlans(value: unknown): SavingsPlans | null {
  if (!isRecord(value) || !Array.isArray(value.plans) || value.plans.length > MAX_PLANS) return null;
  const plans: SavingsPlan[] = [];
  const ids = new Set<string>();
  for (const raw of value.plans) {
    const plan = parseSavingsPlan(raw);
    if (!plan || ids.has(plan.plan_id)) return null;
    ids.add(plan.plan_id);
    plans.push(plan);
  }
  return { plans };
}

/* ------------------------------------------------------------------------ */
/* Words                                                                    */
/* ------------------------------------------------------------------------ */

/** "$500.00 every week". */
export function planCadence(plan: Pick<SavingsPlan, "amount" | "frequency">): string {
  return `${formatMoney(plan.amount)} ${FREQUENCY_PHRASES[plan.frequency]}`;
}

const DATE_PARTS = new Intl.DateTimeFormat("en-US", {
  timeZone: MEXICO_CITY,
  weekday: "short",
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
  hour12: true,
});

function partsOf(ms: number): Record<string, string> {
  const parts: Record<string, string> = {};
  for (const part of DATE_PARTS.formatToParts(ms)) parts[part.type] = part.value;
  return parts;
}

/**
 * A purchase's date and time in Mexico City, where the plan's calendar lives:
 * "Tue, Dec 1 · 9:00 AM", with the year when it is not `now`'s ("Tue, Jan 5,
 * 2027 · 9:00 AM"). Built from parts, so every browser spaces it alike.
 */
export function formatPlanDate(iso: string, now = Date.now()): string {
  const p = partsOf(Date.parse(iso));
  const year = p.year !== partsOf(now).year ? `, ${p.year}` : "";
  return `${p.weekday}, ${p.month} ${p.day}${year} · ${p.hour}:${p.minute} ${p.dayPeriod}`;
}

/** Just the day, in Mexico City: "Dec 1". */
export function formatPlanDay(iso: string): string {
  const p = partsOf(Date.parse(iso));
  return `${p.month} ${p.day}`;
}

/**
 * The next purchase of an active plan: its date, or "In a moment" once it is
 * due (the scheduler runs due purchases within seconds). Null for a plan
 * with none (paused, cancelled, or resuming while the server answers).
 */
export function nextPurchaseLabel(plan: Pick<SavingsPlan, "next_execution_at">, now = Date.now()): string | null {
  if (plan.next_execution_at === null) return null;
  return Date.parse(plan.next_execution_at) <= now ? "In a moment" : formatPlanDate(plan.next_execution_at, now);
}

/**
 * Why a purchase was skipped, after "Skipped:": the reasons a client can act
 * on in their words, anything else in one quiet sentence that says nothing
 * about why.
 */
const SKIP_REASONS: Readonly<Record<string, string>> = {
  SALDO_INSUFICIENTE: "not enough available cash",
  MERCADO_CERRADO: "the market was closed",
  MERCADO_NO_DISPONIBLE: "the market was not open",
  MONTO_INSUFICIENTE: "the amount was below one unit at that price",
  KYC_NO_VERIFICADO: "your identity verification was pending",
};
const QUIET_SKIP = "the purchase was not available at that time";

export function skipReason(code: string | null): string {
  return code !== null && Object.hasOwn(SKIP_REASONS, code) ? SKIP_REASONS[code]! : QUIET_SKIP;
}

/**
 * The last purchase, in a sentence: "Bought 0.31 AUG1 at $1,612.40 on Nov 24"
 * or "Skipped on Dec 1: not enough available cash"; null before the first.
 * `decimals`: the asset's quantity increment.
 */
export function lastPurchaseSentence(
  plan: Pick<SavingsPlan, "ticker" | "last_result" | "last_execution_at" | "failure_reason" | "last_purchase">,
  decimals = 4,
): string | null {
  if (plan.last_result === "skipped" && plan.last_execution_at !== null) {
    return `Skipped on ${formatPlanDay(plan.last_execution_at)}: ${skipReason(plan.failure_reason)}`;
  }
  const fill = plan.last_purchase;
  if (fill === null) return null;
  return `Bought ${formatShares(fill.quantity, decimals)} ${plan.ticker} at ${formatPrice(fill.price)} on ${formatPlanDay(fill.executed_at)}`;
}

/** What the active plans invest in an average month: the sum of each one's monthly equivalent. */
export function monthlyTotal(plans: readonly Pick<SavingsPlan, "status" | "amount" | "frequency">[]): DecimalString {
  const monthly = plans
    .filter((plan) => plan.status === "active")
    .map((plan) => monthlyEquivalent(plan.amount, plan.frequency));
  return monthly.length === 0 ? "0" : addUnits(...monthly);
}

/** "2 active · 1 paused", "1 active", "1 paused". */
export function plansCount(plans: readonly Pick<SavingsPlan, "status">[]): string {
  const active = plans.filter((plan) => plan.status === "active").length;
  const paused = plans.filter((plan) => plan.status === "paused").length;
  const parts = [active > 0 ? `${active} active` : "", paused > 0 ? `${paused} paused` : ""].filter(Boolean);
  return parts.length === 0 ? "No savings plans" : parts.join(" · ");
}

/**
 * The plan as it stands right after `action`, before the server answers:
 * paused without dates, resumed with its dates still to come (the server
 * computes them), cancelled. The list shows this at once.
 */
export function applyAction(plan: SavingsPlan, action: PlanAction): SavingsPlan {
  switch (action) {
    case "pause":
      return { ...plan, status: "paused", next_execution_at: null, upcoming: [] };
    case "resume":
      return { ...plan, status: "active" };
    case "cancel":
      return { ...plan, status: "cancelled", next_execution_at: null, upcoming: [] };
  }
}

/** The sentence that confirms an action, from the plan the server answered. */
export function actionSentence(plan: SavingsPlan, action: PlanAction, now = Date.now()): string {
  const what = `Savings plan for ${plan.ticker}`;
  switch (action) {
    case "pause":
      return `${what} paused: no purchases until you resume it.`;
    case "resume": {
      const next = nextPurchaseLabel(plan, now);
      return next === null ? `${what} resumed.` : `${what} resumed. Next purchase: ${next}.`;
    }
    case "cancel":
      return `${what} cancelled. What it bought stays in your vault.`;
  }
}
