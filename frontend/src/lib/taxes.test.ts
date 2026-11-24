/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ASSET_CLASS_LABELS,
  CSV_HEADER,
  W8BEN_BLOCK_MESSAGES,
  W8BEN_STATUS_LABELS,
  certificationRequest,
  constanciaFigures,
  countryName,
  exportFileName,
  formatRate,
  formatTaxDate,
  normalizeSignature,
  parseCsv,
  parseTaxCenter,
  parseW8benState,
  signatureMatches,
  treatmentLabel,
  validThrough,
  verifyExport,
} from "./taxes.ts";
import type { TaxYearSummary } from "../types/taxes.ts";

const H = (c: string) => c.repeat(64);

const FORM = {
  version: 2,
  signed_at: "2026-11-23T22:00:00Z",
  valid_until: "2029-12-31",
  name: "ANA ROJAS NÚÑEZ",
  citizenship: "MX",
  residence: "MX",
  address: "Av. Reforma 222 Int. 4B, Juárez, Cuauhtémoc, Ciudad de México, C.P. 06600, México",
  foreign_tin: "RONA850315AB1",
  birth_date: "1985-03-15",
  treaty_country: "MX",
  beneficial_owner: true,
  not_us_person: true,
  treaty_resident: true,
  signature: "ANA ROJAS NÚÑEZ",
  device: "Chrome on macOS",
  ip: "203.0.113.7",
  hash: H("b"),
  previous_hash: H("a"),
};

const W8BEN = {
  status: "active",
  current: FORM,
  history: [
    {
      version: 2,
      signed_at: "2026-11-23T22:00:00Z",
      valid_until: "2029-12-31",
      device: "Chrome on macOS",
      hash: H("b"),
    },
    { version: 1, signed_at: "2026-11-22T22:00:00Z", valid_until: "2029-12-31", device: null, hash: H("a") },
  ],
  chain_verified: true,
  can_certify: true,
  blocked_reason: null,
  us_dividend_rate: "10.00",
  us_dividend_rate_treaty: "10.00",
  us_dividend_rate_statutory: "30.00",
  us_dividend_basis: "Tratado México–EUA, art. 10(2); IRC §871(a) y §1441",
};

const SUMMARY = {
  year: 2026,
  in_progress: true,
  period_start: "2026-01-01T06:00:00Z",
  period_end: "2026-11-24T01:00:00Z",
  market: {
    buys: 2,
    bought: "3500.00000000",
    sells: 2,
    proceeds: "1600.00000000",
    cost_basis: "1550.00000000",
    gains: "200.00000000",
    losses: "150.00000000",
    net_result: "50.00000000",
    commissions: "0",
    by_asset: [
      {
        ticker: "AUG1",
        asset_name: "Tokenized gold, 1 g",
        asset_class: "commodity",
        treatment: "annual_return",
        buys: 1,
        bought: "1500.00000000",
        sells: 1,
        proceeds: "600.00000000",
        cost_basis: "750.00000000",
        net_result: "-150.00000000",
      },
      {
        ticker: "STSLA",
        asset_name: "Synthetic Tesla, 1 share",
        asset_class: "equity",
        treatment: "listed_shares",
        buys: 1,
        bought: "2000.00000000",
        sells: 1,
        proceeds: "1000.00000000",
        cost_basis: "800.00000000",
        net_result: "200.00000000",
      },
    ],
  },
  fixed_term: {
    contracts_paid: 1,
    principal_returned: "5000.00000000",
    interest_paid: "36.94000000",
    contracts_open: 1,
    principal_open: "2000.00000000",
  },
  isr: {
    withheld: "0",
    estimated: {
      interest_withholding: "3.45",
      listed_shares_net: "200.00000000",
      listed_shares_tax: "20.00",
      total: "23.45",
    },
    rates: {
      interest_withholding_annual: "0.9000",
      interest_basis: "LIF 2026, art. 24; LISR art. 54",
      listed_shares: "10.0000",
      listed_shares_basis: "LISR art. 129",
    },
    not_estimated_reason: null,
    annual_return_result: "-150.00000000",
  },
  reference: H("c"),
};

const CENTER = {
  profile: {
    name: "ANA ROJAS NÚÑEZ",
    rfc: "RONA850315AB1",
    curp: "RONA850315MDFJXN03",
    person_type: "individual",
    nationality: "MX",
    tax_residence: "MX",
    identity_verified: true,
    address: FORM.address,
    birth_date: "1985-03-15",
  },
  w8ben: W8BEN,
  years: [2026, 2025],
  summary: SUMMARY,
};

test("the Tax Center's answer, validated whole", () => {
  const center = parseTaxCenter(CENTER);
  assert.ok(center);
  assert.equal(center.w8ben.current?.valid_until, "2029-12-31");
  assert.equal(center.summary.market.by_asset[1]?.treatment, "listed_shares");
  assert.deepEqual(center.years, [2026, 2025]);
  assert.ok(parseTaxCenter({ ...CENTER, w8ben: { ...W8BEN, status: "pending", current: null, history: [] } }));
  assert.ok(
    parseTaxCenter({
      ...CENTER,
      summary: { ...SUMMARY, isr: { ...SUMMARY.isr, estimated: null, not_estimated_reason: "no_rates", rates: null } },
    }),
    "a year without published rates has no estimate, and says why",
  );
  const market = SUMMARY.market;
  for (const bad of [
    { ...CENTER, years: [2025, 2026] },
    { ...CENTER, years: [] },
    { ...CENTER, w8ben: { ...W8BEN, status: "pending" } },
    { ...CENTER, w8ben: { ...W8BEN, can_certify: true, blocked_reason: "us_person" } },
    { ...CENTER, w8ben: { ...W8BEN, current: { ...FORM, not_us_person: false } } },
    { ...CENTER, w8ben: { ...W8BEN, current: { ...FORM, signature: "OTRA PERSONA" } } },
    { ...CENTER, w8ben: { ...W8BEN, current: { ...FORM, valid_until: "2029-06-30" } } },
    { ...CENTER, w8ben: { ...W8BEN, history: W8BEN.history.slice(1) } },
    { ...CENTER, w8ben: { ...W8BEN, us_dividend_rate: "100.00" } },
    { ...CENTER, profile: { ...CENTER.profile, rfc: "nope" } },
    { ...CENTER, summary: { ...SUMMARY, market: { ...market, net_result: "51.00000000" } } },
    { ...CENTER, summary: { ...SUMMARY, market: { ...market, gains: "201.00000000" } } },
    {
      ...CENTER,
      summary: { ...SUMMARY, isr: { ...SUMMARY.isr, estimated: { ...SUMMARY.isr.estimated, total: "23.46" } } },
    },
    { ...CENTER, summary: { ...SUMMARY, isr: { ...SUMMARY.isr, estimated: null } } },
    { ...CENTER, summary: { ...SUMMARY, isr: { ...SUMMARY.isr, withheld: "-1" } } },
    {
      ...CENTER,
      summary: { ...SUMMARY, market: { ...market, by_asset: [{ ...market.by_asset[0], asset_class: "crypto" }] } },
    },
    { ...CENTER, summary: { ...SUMMARY, reference: "abc" } },
  ]) {
    assert.equal(parseTaxCenter(bad), null, JSON.stringify(bad).slice(0, 120));
  }
  assert.ok(parseW8benState(W8BEN), "what a certification answers");
});

test("rates, countries and treatments in words", () => {
  assert.equal(formatRate("0.9000"), "0.90%");
  assert.equal(formatRate("0.5000"), "0.50%");
  assert.equal(formatRate("10.0000"), "10%");
  assert.equal(formatRate("30.00"), "30%");
  assert.equal(formatRate("0.125"), "0.125%");
  assert.equal(countryName("MX"), "México");
  assert.equal(countryName("US"), "United States");
  assert.equal(
    treatmentLabel("listed_shares", parseTaxCenter(CENTER)!.summary.isr.rates),
    "Listed shares · 10% (LISR art. 129)",
  );
  assert.equal(treatmentLabel("annual_return", null), "Added to your annual return");
  assert.equal(validThrough("2029-12-31"), "Valid through Dec 31, 2029");
  assert.equal(formatTaxDate("2029-12-31"), "Dec 31, 2029");
  assert.equal(exportFileName(2026), "synthetic-capital-taxes-2026.csv");
});

test("the signature is compared as the server compares it", () => {
  assert.equal(normalizeSignature("  ana   rojas\tnúñez "), "ANA ROJAS NÚÑEZ");
  // Decomposed accents (NFD) are composed first.
  assert.ok(signatureMatches("Ana Rojas Núñez", "ANA ROJAS NÚÑEZ"));
  assert.ok(!signatureMatches("Ana Rojas", "ANA ROJAS NÚÑEZ"));
  assert.ok(!signatureMatches("", ""));
  assert.deepEqual(
    certificationRequest(" Ana Rojas Núñez ", { beneficialOwner: true, notUsPerson: true, treatyResident: true }),
    { beneficial_owner: true, not_us_person: true, treaty_resident: true, signature: "Ana Rojas Núñez" },
  );
});

test("RFC 4180: quoted fields, doubled quotes and CRLF", () => {
  assert.deepEqual(parseCsv('a,b,c\r\n1,"x, y","say ""hi"""\r\n'), [
    ["a", "b", "c"],
    ["1", "x, y", 'say "hi"'],
  ]);
  assert.deepEqual(
    parseCsv('a,"line\r\nbreak"\r\n'),
    [["a", "line\r\nbreak"]],
    "CRLF inside quotes stays in the field",
  );
  assert.deepEqual(parseCsv("a,,c\r\n"), [["a", "", "c"]]);
  for (const bad of ["a,b\r\nc,d", "a,b\nc,d\r\n", 'a,b"c\r\n', '"ab"c,d\r\n', '"open\r\n', "a\rb\r\n"]) {
    assert.equal(parseCsv(bad), null, JSON.stringify(bad));
  }
});

const ROW = (cells: Record<string, string>) => CSV_HEADER.map((c) => cells[c] ?? "");
const quote = (v: string) => (/[",\r\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
const csv = (rows: string[][]) => rows.map((r) => r.map(quote).join(",")).join("\r\n") + "\r\n";
const BASE = { ledger_hash: H("d"), date: "2026-11-23T16:24:05-06:00" };
const EXPORT_ROWS = [
  [...CSV_HEADER],
  ROW({ ...BASE, type: "deposit", amount: "30000.00", balance: "30000.00", ledger_sequence: "1" }),
  ROW({
    ...BASE,
    type: "market_buy",
    reference: "CO-00000001",
    ticker: "STSLA",
    asset_name: "Synthetic Tesla, 1 share",
    amount: "-2000.00",
    commission: "0.00",
  }),
  ROW({
    ...BASE,
    type: "market_buy",
    reference: "CO-00000002",
    ticker: "AUG1",
    amount: "-1500.00",
    commission: "0.00",
  }),
  ROW({
    ...BASE,
    type: "market_sell",
    ticker: "STSLA",
    asset_name: "Synthetic Tesla, 1 share",
    amount: "1000.00",
    cost_basis: "800.00",
    realized_result: "200.00",
  }),
  ROW({
    ...BASE,
    type: "market_sell",
    ticker: "AUG1",
    amount: "600.00",
    cost_basis: "750.00",
    realized_result: "-150.00",
  }),
  ROW({ ...BASE, type: "to_fixed_term", reference: "MUTUO-1", amount: "-5000.00" }),
  ROW({ ...BASE, type: "from_fixed_term", reference: "MUTUO-1", amount: "5036.94", interest: "36.94" }),
  ROW({ ...BASE, type: "to_fixed_term", reference: "MUTUO-2", amount: "-2000.00" }),
];

test("the export is reconciled with the summary before it is saved", () => {
  const summary = parseTaxCenter(CENTER)!.summary;
  const text = csv(EXPORT_ROWS);
  assert.ok(text.includes('"Synthetic Tesla, 1 share"'));
  assert.deepEqual(verifyExport(text, summary), { ok: true, rows: 8 });

  const changed = (i: number, cells: Record<string, string>) =>
    csv(EXPORT_ROWS.map((r, j) => (j === i ? CSV_HEADER.map((c, k) => cells[c] ?? r[k]!) : r)));
  const problem = (text: string) => {
    const r = verifyExport(text, summary);
    return r.ok ? null : r.problem;
  };
  assert.match(problem(changed(4, { realized_result: "201.00" })) ?? "", /realized result/);
  assert.match(problem(changed(7, { interest: "36.95" })) ?? "", /interest/);
  assert.match(problem(changed(2, { amount: "-2000.01" })) ?? "", /purchases or sales/);
  assert.match(problem(changed(5, { type: "deposit" })) ?? "", /trades/);
  assert.match(problem(changed(1, { date: "2025-12-31T23:00:00-06:00" })) ?? "", /outside 2026/);
  assert.match(problem(changed(1, { amount: "1,000" })) ?? "", /not a number/);
  assert.match(problem(csv([["date", "kind"], ...EXPORT_ROWS.slice(1)])) ?? "", /columns/);
  assert.match(problem(text.slice(0, -2)) ?? "", /not valid CSV/);
});

test("the Constancia de Retenciones preview reads the summary", () => {
  const summary: TaxYearSummary = parseTaxCenter(CENTER)!.summary;
  assert.deepEqual(constanciaFigures(summary), {
    nominalInterest: "36.94000000",
    withheld: "0",
    listedGain: "200.00000000",
    listedLoss: "0",
    otherResult: "-150.00000000",
    estimatedTotal: "23.45",
  });
  const loss = {
    ...summary,
    market: { ...summary.market, by_asset: summary.market.by_asset.map((a) => ({ ...a, net_result: "-80.00000000" })) },
  };
  assert.equal(constanciaFigures(loss).listedLoss, "80.00000000");
});

test("no tax wording would tip off a client under review", () => {
  const TIP_OFF =
    /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/i;
  for (const words of [
    ...Object.values(W8BEN_STATUS_LABELS),
    ...Object.values(W8BEN_BLOCK_MESSAGES),
    ...Object.values(ASSET_CLASS_LABELS),
    treatmentLabel("annual_return", null),
    treatmentLabel("listed_shares", null),
  ]) {
    assert.doesNotMatch(words, TIP_OFF, words);
  }
});
