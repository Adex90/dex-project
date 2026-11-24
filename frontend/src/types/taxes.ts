/**
 * The Tax Center (roadmap item 26): `GET /api/v1/wealth/impuestos`, the
 * W-8BEN certification and the accounting export. English names, like the
 * rest of `/wealth`; `lib/taxes.ts` validates them.
 */

import type { DecimalString } from "./vault.ts";

/** What the client's KYC file says, for tax purposes. */
export interface TaxProfile {
  /** The legal name, upper case, as the W-8BEN is signed. */
  name: string;
  rfc: string | null;
  curp: string | null;
  person_type: "individual" | "business";
  /** ISO 3166-1 alpha-2. */
  nationality: string;
  tax_residence: string;
  identity_verified: boolean;
  /** The permanent address on one line. */
  address: string;
  /** "YYYY-MM-DD". */
  birth_date: string | null;
}

export type W8benStatus = "active" | "expired" | "outdated" | "pending";

export type W8benBlock = "identity_unverified" | "business" | "us_person" | "non_resident" | "missing_data";

/** One signed version of the W-8BEN (immutable on the server). */
export interface W8benForm {
  version: number;
  signed_at: string;
  /** Last day of validity, included: "2029-12-31". */
  valid_until: string;
  name: string;
  citizenship: string;
  residence: string;
  address: string;
  foreign_tin: string;
  birth_date: string;
  treaty_country: string;
  beneficial_owner: boolean;
  not_us_person: boolean;
  treaty_resident: boolean;
  signature: string;
  device: string | null;
  ip: string | null;
  hash: string;
  previous_hash: string;
}

export interface W8benSummary {
  version: number;
  signed_at: string;
  valid_until: string;
  device: string | null;
  hash: string;
}

export interface W8benState {
  status: W8benStatus;
  current: W8benForm | null;
  /** Newest first. */
  history: W8benSummary[];
  /** The server recomputed every version's hash and link. */
  chain_verified: boolean;
  can_certify: boolean;
  blocked_reason: W8benBlock | null;
  /** US withholding on US-source dividends today, in %: the treaty's with an active form. */
  us_dividend_rate: string | null;
  us_dividend_rate_treaty: string | null;
  us_dividend_rate_statutory: string | null;
  us_dividend_basis: string | null;
}

export type AssetClass = "equity" | "fund" | "commodity" | "fixed_income";

/** "listed_shares": LISR art. 129; "annual_return": added to the annual return. */
export type TaxTreatment = "listed_shares" | "annual_return";

export interface AssetTaxLine {
  ticker: string;
  asset_name: string;
  asset_class: AssetClass;
  treatment: TaxTreatment;
  buys: number;
  bought: DecimalString;
  sells: number;
  proceeds: DecimalString;
  cost_basis: DecimalString;
  net_result: DecimalString;
}

export interface MarketTaxSummary {
  buys: number;
  bought: DecimalString;
  sells: number;
  /** Gross sales. */
  proceeds: DecimalString;
  /** Average cost released by the sales. */
  cost_basis: DecimalString;
  gains: DecimalString;
  /** Positive. */
  losses: DecimalString;
  /** proceeds − cost_basis. */
  net_result: DecimalString;
  commissions: DecimalString;
  by_asset: AssetTaxLine[];
}

export interface FixedTermTaxSummary {
  contracts_paid: number;
  principal_returned: DecimalString;
  /** Nominal interest paid in the year. */
  interest_paid: DecimalString;
  contracts_open: number;
  principal_open: DecimalString;
}

export interface TaxRates {
  /** Annual % on interest-bearing capital ("0.9000"). */
  interest_withholding_annual: DecimalString;
  interest_basis: string;
  /** % on the year's net gain from listed shares ("10.0000"). */
  listed_shares: DecimalString;
  listed_shares_basis: string;
}

export interface IsrEstimate {
  interest_withholding: DecimalString;
  listed_shares_net: DecimalString;
  listed_shares_tax: DecimalString;
  total: DecimalString;
}

export interface IsrSummary {
  /** What the ledgers record as withheld. */
  withheld: DecimalString;
  estimated: IsrEstimate | null;
  rates: TaxRates | null;
  not_estimated_reason: "no_rates" | null;
  /** Net result of commodities and fixed income: added to the annual return, not estimated. */
  annual_return_result: DecimalString;
}

export interface TaxYearSummary {
  year: number;
  /** The year in progress: its figures change until it closes. */
  in_progress: boolean;
  period_start: string;
  /** The last instant included (now, for the year in progress). */
  period_end: string;
  market: MarketTaxSummary;
  fixed_term: FixedTermTaxSummary;
  isr: IsrSummary;
  /** SHA-256 of the summary (SC-FISCAL-v1). */
  reference: string;
}

export interface TaxCenter {
  profile: TaxProfile;
  w8ben: W8benState;
  /** Years with activity, newest first (always the current one). */
  years: number[];
  summary: TaxYearSummary;
}

/** The body of `POST /api/v1/wealth/impuestos/w8ben`. */
export interface W8benCertification {
  beneficial_owner: boolean;
  not_us_person: boolean;
  treaty_resident: boolean;
  signature: string;
}
