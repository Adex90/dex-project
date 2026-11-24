/**
 * The Tax Center: validating `GET /api/v1/wealth/impuestos` (and the W-8BEN
 * state a certification returns), the words the screens use, the signature
 * as the server compares it, and the accounting export: an RFC 4180 parser
 * that reconciles the CSV with the year's summary before it is saved.
 *
 * - Every figure is the server's, an exact decimal string: nothing here
 *   computes tax. The export's checks add exact units (BigInt), never floats.
 * - Network data is validated before anything renders it; a malformed
 *   payload is rejected whole.
 * - No wording here may tip off a client under review.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { fromUnits, toUnits } from "./decimal.ts";
import { isDecimalString, isNonNegative, isOneOf, isRecord, isText } from "./vault.ts";
import type {
  AssetClass,
  AssetTaxLine,
  FixedTermTaxSummary,
  IsrEstimate,
  IsrSummary,
  MarketTaxSummary,
  TaxCenter,
  TaxProfile,
  TaxRates,
  TaxTreatment,
  TaxYearSummary,
  W8benBlock,
  W8benCertification,
  W8benForm,
  W8benState,
  W8benStatus,
  W8benSummary,
} from "../types/taxes.ts";

/** The Tax Center page. */
export const TAXES_PATH = "/dashboard/taxes";

const STATUSES: readonly W8benStatus[] = ["active", "expired", "outdated", "pending"];
const BLOCKS: readonly W8benBlock[] = ["identity_unverified", "business", "us_person", "non_resident", "missing_data"];
const CLASSES: readonly AssetClass[] = ["equity", "fund", "commodity", "fixed_income"];
const TREATMENTS: readonly TaxTreatment[] = ["listed_shares", "annual_return"];
const MAX_YEARS = 200;
const MAX_HISTORY = 500;
const MAX_ASSETS = 500;

const HASH = /^[0-9a-f]{64}$/;
const COUNTRY = /^[A-Z]{2}$/;
const DATE = /^\d{4}-\d{2}-\d{2}$/;
const RFC = /^[A-ZÑ&]{3,4}\d{6}[A-Z0-9]{3}$/;
const CURP = /^[A-Z]{4}\d{6}[HMX][A-Z]{5}[A-Z0-9]\d$/;
const TICKER = /^[A-Z][A-Z0-9]{0,11}$/;

const isHash = (v: unknown): v is string => typeof v === "string" && HASH.test(v);
const isCountry = (v: unknown): v is string => typeof v === "string" && COUNTRY.test(v);
const isDate = (v: unknown): v is string =>
  typeof v === "string" && DATE.test(v) && !Number.isNaN(Date.parse(`${v}T00:00:00Z`));
const isTimestamp = (v: unknown): v is string => isText(v, 64) && !Number.isNaN(Date.parse(v));
const isCount = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
const isYear = (v: unknown): v is number => typeof v === "number" && Number.isInteger(v) && v >= 2000 && v <= 2100;
const orNull = <T>(v: unknown, check: (x: unknown) => x is T): T | null | undefined =>
  v === null || v === undefined ? null : check(v) ? v : undefined;

// ---------------------------------------------------------------------------
// Validating the API's answer
// ---------------------------------------------------------------------------

function parseProfile(v: unknown): TaxProfile | null {
  if (!isRecord(v)) return null;
  const rfc = orNull(v.rfc, (x): x is string => typeof x === "string" && RFC.test(x));
  const curp = orNull(v.curp, (x): x is string => typeof x === "string" && CURP.test(x));
  const birth = orNull(v.birth_date, isDate);
  if (
    rfc === undefined ||
    curp === undefined ||
    birth === undefined ||
    !isText(v.name, 200) ||
    !isOneOf(["individual", "business"] as const, v.person_type) ||
    !isCountry(v.nationality) ||
    !isCountry(v.tax_residence) ||
    typeof v.identity_verified !== "boolean" ||
    typeof v.address !== "string" ||
    v.address.length > 300
  ) {
    return null;
  }
  return {
    name: v.name,
    rfc,
    curp,
    person_type: v.person_type,
    nationality: v.nationality,
    tax_residence: v.tax_residence,
    identity_verified: v.identity_verified,
    address: v.address,
    birth_date: birth,
  };
}

function parseForm(v: unknown): W8benForm | null {
  if (!isRecord(v)) return null;
  const device = orNull(v.device, (x): x is string => isText(x, 80));
  const ip = orNull(v.ip, (x): x is string => isText(x, 64));
  if (
    device === undefined ||
    ip === undefined ||
    !isCount(v.version) ||
    v.version < 1 ||
    !isTimestamp(v.signed_at) ||
    !isDate(v.valid_until) ||
    !isText(v.name, 200) ||
    !isCountry(v.citizenship) ||
    !isCountry(v.residence) ||
    !isText(v.address, 300) ||
    typeof v.foreign_tin !== "string" ||
    !RFC.test(v.foreign_tin) ||
    !isDate(v.birth_date) ||
    !isCountry(v.treaty_country) ||
    // Part III: a stored form always certifies all three.
    v.beneficial_owner !== true ||
    v.not_us_person !== true ||
    v.treaty_resident !== true ||
    !isText(v.signature, 200) ||
    v.signature !== v.name ||
    !isHash(v.hash) ||
    !isHash(v.previous_hash) ||
    // Valid through a December 31.
    !v.valid_until.endsWith("-12-31")
  ) {
    return null;
  }
  return {
    version: v.version,
    signed_at: v.signed_at,
    valid_until: v.valid_until,
    name: v.name,
    citizenship: v.citizenship,
    residence: v.residence,
    address: v.address,
    foreign_tin: v.foreign_tin,
    birth_date: v.birth_date,
    treaty_country: v.treaty_country,
    beneficial_owner: true,
    not_us_person: true,
    treaty_resident: true,
    signature: v.signature,
    device,
    ip,
    hash: v.hash,
    previous_hash: v.previous_hash,
  };
}

function parseFormSummary(v: unknown): W8benSummary | null {
  if (!isRecord(v)) return null;
  const device = orNull(v.device, (x): x is string => isText(x, 80));
  if (
    device === undefined ||
    !isCount(v.version) ||
    v.version < 1 ||
    !isTimestamp(v.signed_at) ||
    !isDate(v.valid_until) ||
    !isHash(v.hash)
  ) {
    return null;
  }
  return { version: v.version, signed_at: v.signed_at, valid_until: v.valid_until, device, hash: v.hash };
}

const isRate = (x: unknown): x is string => isNonNegative(x) && toUnits(x) < 100n * 10n ** 8n;

/** The W-8BEN state (`GET` inside the Tax Center, and `POST .../w8ben`); null when malformed. */
export function parseW8benState(v: unknown): W8benState | null {
  if (!isRecord(v) || !Array.isArray(v.history) || v.history.length > MAX_HISTORY) return null;
  const current = v.current === null || v.current === undefined ? null : parseForm(v.current);
  if (v.current !== null && v.current !== undefined && current === null) return null;
  const block = orNull(v.blocked_reason, (x): x is W8benBlock => isOneOf(BLOCKS, x));
  const rate = orNull(v.us_dividend_rate, isRate);
  const treaty = orNull(v.us_dividend_rate_treaty, isRate);
  const statutory = orNull(v.us_dividend_rate_statutory, isRate);
  const basis = orNull(v.us_dividend_basis, (x): x is string => isText(x, 120));
  if (
    block === undefined ||
    rate === undefined ||
    treaty === undefined ||
    statutory === undefined ||
    basis === undefined ||
    !isOneOf(STATUSES, v.status) ||
    typeof v.chain_verified !== "boolean" ||
    typeof v.can_certify !== "boolean" ||
    // Not certified exactly when there is no form; blocked exactly when it cannot certify.
    (v.status === "pending") !== (current === null) ||
    v.can_certify !== (block === null)
  ) {
    return null;
  }
  const history: W8benSummary[] = [];
  for (const raw of v.history) {
    const s = parseFormSummary(raw);
    if (!s) return null;
    history.push(s);
  }
  if (current && (history[0]?.version !== current.version || history[0]?.hash !== current.hash)) return null;
  return {
    status: v.status,
    current,
    history,
    chain_verified: v.chain_verified,
    can_certify: v.can_certify,
    blocked_reason: block,
    us_dividend_rate: rate,
    us_dividend_rate_treaty: treaty,
    us_dividend_rate_statutory: statutory,
    us_dividend_basis: basis,
  };
}

function parseAssetLine(v: unknown): AssetTaxLine | null {
  if (
    !isRecord(v) ||
    typeof v.ticker !== "string" ||
    !TICKER.test(v.ticker) ||
    !isText(v.asset_name, 120) ||
    !isOneOf(CLASSES, v.asset_class) ||
    !isOneOf(TREATMENTS, v.treatment) ||
    !isCount(v.buys) ||
    !isNonNegative(v.bought) ||
    !isCount(v.sells) ||
    !isNonNegative(v.proceeds) ||
    !isNonNegative(v.cost_basis) ||
    !isDecimalString(v.net_result)
  ) {
    return null;
  }
  return {
    ticker: v.ticker,
    asset_name: v.asset_name,
    asset_class: v.asset_class,
    treatment: v.treatment,
    buys: v.buys,
    bought: v.bought,
    sells: v.sells,
    proceeds: v.proceeds,
    cost_basis: v.cost_basis,
    net_result: v.net_result,
  };
}

function parseMarket(v: unknown): MarketTaxSummary | null {
  if (
    !isRecord(v) ||
    !Array.isArray(v.by_asset) ||
    v.by_asset.length > MAX_ASSETS ||
    !isCount(v.buys) ||
    !isNonNegative(v.bought) ||
    !isCount(v.sells) ||
    !isNonNegative(v.proceeds) ||
    !isNonNegative(v.cost_basis) ||
    !isNonNegative(v.gains) ||
    !isNonNegative(v.losses) ||
    !isDecimalString(v.net_result) ||
    !isNonNegative(v.commissions)
  ) {
    return null;
  }
  // The net result is the proceeds less their cost, and the gains less the losses.
  const net = toUnits(v.net_result);
  if (net !== toUnits(v.proceeds) - toUnits(v.cost_basis) || net !== toUnits(v.gains) - toUnits(v.losses)) return null;
  const byAsset: AssetTaxLine[] = [];
  for (const raw of v.by_asset) {
    const a = parseAssetLine(raw);
    if (!a) return null;
    byAsset.push(a);
  }
  return {
    buys: v.buys,
    bought: v.bought,
    sells: v.sells,
    proceeds: v.proceeds,
    cost_basis: v.cost_basis,
    gains: v.gains,
    losses: v.losses,
    net_result: v.net_result,
    commissions: v.commissions,
    by_asset: byAsset,
  };
}

function parseFixedTerm(v: unknown): FixedTermTaxSummary | null {
  if (
    !isRecord(v) ||
    !isCount(v.contracts_paid) ||
    !isNonNegative(v.principal_returned) ||
    !isNonNegative(v.interest_paid) ||
    !isCount(v.contracts_open) ||
    !isNonNegative(v.principal_open)
  ) {
    return null;
  }
  return {
    contracts_paid: v.contracts_paid,
    principal_returned: v.principal_returned,
    interest_paid: v.interest_paid,
    contracts_open: v.contracts_open,
    principal_open: v.principal_open,
  };
}

function parseRates(v: unknown): TaxRates | null {
  if (
    !isRecord(v) ||
    !isRate(v.interest_withholding_annual) ||
    !isText(v.interest_basis, 120) ||
    !isRate(v.listed_shares) ||
    !isText(v.listed_shares_basis, 120)
  ) {
    return null;
  }
  return {
    interest_withholding_annual: v.interest_withholding_annual,
    interest_basis: v.interest_basis,
    listed_shares: v.listed_shares,
    listed_shares_basis: v.listed_shares_basis,
  };
}

function parseEstimate(v: unknown): IsrEstimate | null {
  if (
    !isRecord(v) ||
    !isNonNegative(v.interest_withholding) ||
    !isDecimalString(v.listed_shares_net) ||
    !isNonNegative(v.listed_shares_tax) ||
    !isNonNegative(v.total) ||
    toUnits(v.total) !== toUnits(v.interest_withholding) + toUnits(v.listed_shares_tax)
  ) {
    return null;
  }
  return {
    interest_withholding: v.interest_withholding,
    listed_shares_net: v.listed_shares_net,
    listed_shares_tax: v.listed_shares_tax,
    total: v.total,
  };
}

function parseIsr(v: unknown): IsrSummary | null {
  if (!isRecord(v)) return null;
  const estimated = v.estimated === null || v.estimated === undefined ? null : parseEstimate(v.estimated);
  const rates = v.rates === null || v.rates === undefined ? null : parseRates(v.rates);
  const reason = orNull(v.not_estimated_reason, (x): x is "no_rates" => x === "no_rates");
  if (
    (v.estimated !== null && v.estimated !== undefined && estimated === null) ||
    (v.rates !== null && v.rates !== undefined && rates === null) ||
    reason === undefined ||
    // An estimate, or the reason there is none.
    (estimated === null) !== (reason !== null) ||
    !isNonNegative(v.withheld) ||
    !isDecimalString(v.annual_return_result)
  ) {
    return null;
  }
  return {
    withheld: v.withheld,
    estimated,
    rates,
    not_estimated_reason: reason,
    annual_return_result: v.annual_return_result,
  };
}

function parseSummary(v: unknown): TaxYearSummary | null {
  if (!isRecord(v)) return null;
  const market = parseMarket(v.market);
  const fixedTerm = parseFixedTerm(v.fixed_term);
  const isr = parseIsr(v.isr);
  if (
    !market ||
    !fixedTerm ||
    !isr ||
    !isYear(v.year) ||
    typeof v.in_progress !== "boolean" ||
    !isTimestamp(v.period_start) ||
    !isTimestamp(v.period_end) ||
    Date.parse(v.period_end) < Date.parse(v.period_start) ||
    !isHash(v.reference)
  ) {
    return null;
  }
  return {
    year: v.year,
    in_progress: v.in_progress,
    period_start: v.period_start,
    period_end: v.period_end,
    market,
    fixed_term: fixedTerm,
    isr,
    reference: v.reference,
  };
}

/** `GET /api/v1/wealth/impuestos`; null when malformed. */
export function parseTaxCenter(v: unknown): TaxCenter | null {
  if (!isRecord(v) || !Array.isArray(v.years) || v.years.length === 0 || v.years.length > MAX_YEARS) return null;
  const profile = parseProfile(v.profile);
  const w8ben = parseW8benState(v.w8ben);
  const summary = parseSummary(v.summary);
  if (!profile || !w8ben || !summary) return null;
  const years: number[] = [];
  for (const y of v.years) {
    // Newest first, each once.
    if (!isYear(y) || (years.length > 0 && y >= years[years.length - 1]!)) return null;
    years.push(y);
  }
  return { profile, w8ben, years, summary };
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

export const W8BEN_STATUS_LABELS: Record<W8benStatus, string> = {
  active: "Active",
  expired: "Expired",
  outdated: "Needs renewal",
  pending: "Not certified",
};

/** Why a client cannot certify, in the words the page shows. */
export const W8BEN_BLOCK_MESSAGES: Record<W8benBlock, string> = {
  identity_unverified: "Verify your identity to certify a W-8BEN.",
  business: "Business accounts certify on Form W-8BEN-E, which is not available here yet.",
  us_person: "U.S. citizens and residents certify their status on Form W-9 instead.",
  non_resident: "Here you can certify as a resident of Mexico under the U.S.–Mexico tax treaty.",
  missing_data: "Your file needs your RFC, date of birth and address. Complete your identity verification first.",
};

export const ASSET_CLASS_LABELS: Record<AssetClass, string> = {
  equity: "Equity",
  fund: "Fund",
  commodity: "Commodity",
  fixed_income: "Fixed income",
};

/** The country of a code, as the W-8BEN and the profile show it. */
export function countryName(code: string): string {
  if (code === "MX") return "México";
  if (code === "US") return "United States";
  return code;
}

/** A rate in percent, trailing zeros trimmed past two decimals: "0.9000" → "0.90%", "10.0000" → "10%". */
export function formatRate(rate: string): string {
  const [int = "0", frac = ""] = rate.split(".");
  const trimmed = frac.replace(/0+$/, "");
  if (trimmed === "") return `${int}%`;
  return `${int}.${trimmed.padEnd(2, "0")}%`;
}

/** How a line's result is taxed. */
export function treatmentLabel(treatment: TaxTreatment, rates: TaxRates | null): string {
  if (treatment === "annual_return") return "Added to your annual return";
  return rates ? `Listed shares · ${formatRate(rates.listed_shares)} (LISR art. 129)` : "Listed shares (LISR art. 129)";
}

/** A calendar date ("YYYY-MM-DD") as the Tax Center writes it: "Dec 31, 2029". */
export function formatTaxDate(date: string): string {
  return new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", year: "numeric", timeZone: "UTC" }).format(
    new Date(`${date}T00:00:00Z`),
  );
}

/** "Valid through Dec 31, 2029". */
export function validThrough(date: string): string {
  const label = formatTaxDate(date);
  return `Valid through ${label}`;
}

// ---------------------------------------------------------------------------
// The signature
// ---------------------------------------------------------------------------

/** The signature as the server compares it with the legal name: single spaces, NFC, upper case. */
export function normalizeSignature(input: string): string {
  return input.trim().split(/\s+/).filter(Boolean).join(" ").normalize("NFC").toUpperCase();
}

export function signatureMatches(input: string, legalName: string): boolean {
  return normalizeSignature(input) === legalName && legalName.length > 0;
}

/** The body of `POST /api/v1/wealth/impuestos/w8ben`. */
export function certificationRequest(
  signature: string,
  confirmed: { beneficialOwner: boolean; notUsPerson: boolean; treatyResident: boolean },
): W8benCertification {
  return {
    beneficial_owner: confirmed.beneficialOwner,
    not_us_person: confirmed.notUsPerson,
    treaty_resident: confirmed.treatyResident,
    signature: signature.trim(),
  };
}

// ---------------------------------------------------------------------------
// The accounting export
// ---------------------------------------------------------------------------

export const CSV_HEADER = [
  "date",
  "type",
  "reference",
  "ticker",
  "asset_name",
  "asset_class",
  "quantity",
  "price",
  "amount",
  "commission",
  "cost_basis",
  "realized_result",
  "interest",
  "balance",
  "ledger_sequence",
  "ledger_hash",
] as const;

const ROW_TYPES = [
  "deposit",
  "withdrawal",
  "transfer_in",
  "transfer_out",
  "to_fixed_term",
  "from_fixed_term",
  "market_buy",
  "market_sell",
];

/** "synthetic-capital-taxes-2026.csv". */
export function exportFileName(year: number): string {
  return `synthetic-capital-taxes-${year}.csv`;
}

/**
 * RFC 4180: records end in CRLF, fields are separated by commas, a quoted
 * field may hold commas, CRLF and doubled quotes. Null when malformed.
 */
export function parseCsv(text: string): string[][] | null {
  if (!text.endsWith("\r\n")) return null;
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  let closed = false; // just past a closing quote: only a separator may follow
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!;
    if (quoted) {
      if (c !== '"') field += c;
      else if (text[i + 1] === '"') {
        field += '"';
        i++;
      } else {
        quoted = false;
        closed = true;
      }
      continue;
    }
    if (c === ",") {
      row.push(field);
      field = "";
      closed = false;
    } else if (c === "\r") {
      if (text[i + 1] !== "\n") return null;
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
      closed = false;
      i++;
    } else if (closed || c === "\n") {
      return null;
    } else if (c === '"') {
      if (field !== "") return null;
      quoted = true;
    } else {
      field += c;
    }
  }
  return quoted ? null : rows;
}

export type ExportCheck = { ok: true; rows: number } | { ok: false; problem: string };

function units(value: string): bigint | null {
  if (value === "") return 0n;
  return isDecimalString(value) ? toUnits(value) : null;
}

/**
 * Reconciles the export with the year's summary before it is saved: the same
 * columns, rows of that year only, and the same number of trades, gross
 * purchases and sales, realized result and interest, to the unit.
 */
export function verifyExport(text: string, summary: TaxYearSummary): ExportCheck {
  const fail = (problem: string): ExportCheck => ({ ok: false, problem });
  const rows = parseCsv(text);
  if (!rows || rows.length === 0) return fail("The export is not valid CSV.");
  if (rows[0]!.join(",") !== CSV_HEADER.join(",")) return fail("The export does not have the expected columns.");
  const col = (name: (typeof CSV_HEADER)[number]) => CSV_HEADER.indexOf(name);
  let buys = 0;
  let sells = 0;
  let bought = 0n;
  let proceeds = 0n;
  let result = 0n;
  let interest = 0n;
  for (const row of rows.slice(1)) {
    if (row.length !== CSV_HEADER.length) return fail("A row of the export has the wrong number of columns.");
    const type = row[col("type")]!;
    const date = row[col("date")]!;
    if (!ROW_TYPES.includes(type)) return fail("The export has a movement of an unknown kind.");
    if (!date.startsWith(`${summary.year}-`) || !date.endsWith("-06:00")) {
      return fail(`The export has a movement outside ${summary.year}.`);
    }
    const amount = units(row[col("amount")]!);
    const realized = units(row[col("realized_result")]!);
    const paid = units(row[col("interest")]!);
    if (amount === null || realized === null || paid === null || !HASH.test(row[col("ledger_hash")]!)) {
      return fail("The export has a figure that is not a number.");
    }
    if (type === "market_buy") {
      buys += 1;
      bought -= amount;
    } else if (type === "market_sell") {
      sells += 1;
      proceeds += amount;
      result += realized;
    } else if (type === "from_fixed_term") {
      interest += paid;
    }
  }
  const m = summary.market;
  if (buys !== m.buys || sells !== m.sells) return fail("The export's trades do not match the summary.");
  if (bought !== toUnits(m.bought) || proceeds !== toUnits(m.proceeds)) {
    return fail("The export's purchases or sales do not match the summary.");
  }
  if (result !== toUnits(m.net_result)) return fail("The export's realized result does not match the summary.");
  if (interest !== toUnits(summary.fixed_term.interest_paid)) {
    return fail("The export's interest does not match the summary.");
  }
  return { ok: true, rows: rows.length - 1 };
}

// ---------------------------------------------------------------------------
// The Constancia de Retenciones (preview)
// ---------------------------------------------------------------------------

export interface ConstanciaFigures {
  /** Fixed-term interest paid in the year. */
  nominalInterest: string;
  withheld: string;
  /** Net result of listed shares (the base of LISR art. 129), split into gain or loss. */
  listedGain: string;
  listedLoss: string;
  /** Commodities and fixed income: added to the annual return. */
  otherResult: string;
  estimatedTotal: string | null;
}

/** The figures the preview shows, from the server's summary. */
export function constanciaFigures(summary: TaxYearSummary): ConstanciaFigures {
  const listedNet = summary.market.by_asset
    .filter((a) => a.treatment === "listed_shares")
    .reduce((sum, a) => sum + toUnits(a.net_result), 0n);
  return {
    nominalInterest: summary.fixed_term.interest_paid,
    withheld: summary.isr.withheld,
    listedGain: listedNet > 0n ? fromUnits(listedNet) : "0",
    listedLoss: listedNet < 0n ? fromUnits(-listedNet) : "0",
    otherResult: summary.isr.annual_return_result,
    estimatedTotal: summary.isr.estimated?.total ?? null,
  };
}
