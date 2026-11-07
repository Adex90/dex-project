// Tests for the zk-DECO oracle skeleton circuit (witness-level: a violated constraint => no proof exists).
import * as snarkjs from "snarkjs";
import { init, stringify, rand } from "./lib.mjs";

const WASM = "build/zkdeco_oracle/zkdeco_oracle_js/zkdeco_oracle.wasm";
const BODY_LEN = 96, SYMLEN = 6, CHUNK = 24;
const h = await init();
let pass = 0, fail = 0;

const bytes = (s) => [...Buffer.from(s, "latin1")];
const pack = (arr) => arr.reduce((a, b, i) => a + (BigInt(b) << BigInt(8 * i)), 0n);

/** Builds a full witness input from a JSON-ish body string; fields can be overridden to attack it. */
function make(bodyStr, o = {}) {
  const body = bytes(bodyStr); while (body.length < BODY_LEN) body.push(0);
  const sym = o.symbolBytes ?? bytes("XAUUSD");
  const salt = o.salt ?? rand();
  const chunks = Array.from({ length: BODY_LEN / CHUNK }, (_, j) => pack(body.slice(j * CHUNK, (j + 1) * CHUNK)));
  const idx = (needle) => bodyStr.indexOf(needle);
  const priceStr = (bodyStr.match(/"price":([0-9.]+)/) ?? [])[1] ?? "0.0";
  const [ip, fp] = priceStr.split(".");
  const scaled = BigInt(ip + (fp ?? "")) * 10n ** BigInt(18 - (fp ?? "").length);
  const ts = BigInt((bodyStr.match(/"ts":(\d+)/) ?? [])[1] ?? 0);
  return {
    providerId: o.providerId ?? 12345n,
    queryCommitment: o.queryCommitment ?? h(pack(sym), salt),
    price: o.price ?? scaled,
    observedAt: o.observedAt ?? ts,
    bodyCommitment: o.bodyCommitment ?? h(...chunks),
    body: o.body ?? body,
    symbol: sym, salt,
    offSymKey: o.offSymKey ?? idx('"symbol":"'),
    offPriceKey: o.offPriceKey ?? idx('"price":'),
    offTsKey: o.offTsKey ?? idx('"ts":'),
    intLen: o.intLen ?? ip.length,
    fracLen: o.fracLen ?? (fp ?? "").length,
  };
}
const run = (i) => snarkjs.wtns.calculate(stringify(i), WASM, { type: "mem" });
async function accepts(n, i) { try { await run(i); pass++; console.log("  ok   accepts ", n); } catch (e) { fail++; console.log("  FAIL accepts ", n, String(e.message).split("\n")[0]); } }
async function rejects(n, i) { try { await run(i); fail++; console.log("  FAIL rejects ", n, "(accepted!)"); } catch (e) { if (!/Assert Failed|Error in template/.test(String(e.message))) { fail++; console.log("  FAIL rejects ", n, "not a constraint failure:", String(e.message).split("\n")[0]); return; } pass++; const m = String(e.message).match(/template (\w+?)_\d+ line: (\d+)/); console.log("  ok   rejects ", n.padEnd(52), m ? `[${m[1]}:${m[2]}]` : `[${String(e.message).slice(0,60)}]`); } }

const GOOD = '{"symbol":"XAUUSD","price":2345.678912,"ts":1767225600}';
console.log("positive");
await accepts("canonical body", make(GOOD));
await accepts("price 0.5 (1 int digit, 1 frac digit)", make('{"symbol":"XAUUSD","price":0.5,"ts":1767225600}'));
await accepts("price last field (terminator '}')", make('{"symbol":"XAUUSD","ts":1767225600,"price":88.25}'));
await accepts("max precision 12.10 digits", make('{"symbol":"XAUUSD","price":123456789012.1234567890,"ts":1767225600}'));

// public price/time are exactly what was parsed
{ const i = make(GOOD); console.log(`       parsed price = ${i.price} (1e18-scaled), observedAt = ${i.observedAt}`); }

console.log("negative");
await rejects("claimed price != body price", make(GOOD, { price: 2345_678913n * 10n ** 12n }));
await rejects("claimed timestamp != body timestamp", make(GOOD, { observedAt: 1767225601n }));
await rejects("tampered body byte vs notarised commitment", (() => { const i = make(GOOD); const c = make(GOOD).bodyCommitment; i.body = [...i.body]; i.body[30] = i.body[30] === 57 ? 56 : i.body[30] + 1; i.bodyCommitment = c; return i; })());
await rejects("body byte >= 256 (packing malleability)", (() => { const i = make(GOOD); i.body = [...i.body]; i.body[90] = 256; return i; })());
await rejects("wrong hidden symbol (commitment mismatch)", make(GOOD, { symbolBytes: bytes("XAGUSD") }));
await rejects("symbol is only a PREFIX of the response symbol", make('{"symbol":"XAUUSDT","price":1.5,"ts":1767225600}'));
await rejects("offset points to the wrong place", make(GOOD, { offPriceKey: 5 }));
await rejects("offset out of body range", make(GOOD, { offPriceKey: 120 }));
await rejects("wrap-around offset (p-1)", make(GOOD, { offPriceKey: 21888242871839275222246405745257275088548364400416034343698204186575808495616n }));
await rejects("intLen understated", make(GOOD, { intLen: 3 }));
await rejects("fracLen overstated", make(GOOD, { fracLen: 7 }));
await rejects("intLen = 0", make(GOOD, { intLen: 0 }));
await rejects("non-digit inside price (2345.67x912)", make('{"symbol":"XAUUSD","price":2345.67x912,"ts":1767225600}', { intLen: 4, fracLen: 7, price: 1n }));
await rejects("missing terminator after price", make('{"symbol":"XAUUSD","price":2345.6789 "ts":1767225600}', { intLen: 4, fracLen: 4 }));
await rejects("price key misspelt", make('{"symbol":"XAUUSD","prise":2345.6789,"ts":1767225600}', { offPriceKey: 20 }));
await rejects("price zero", make('{"symbol":"XAUUSD","price":0.0,"ts":1767225600}'));
await rejects("timestamp with 9 digits", make('{"symbol":"XAUUSD","price":2.5,"ts":176722560}'));
await rejects("price with no decimal point", make('{"symbol":"XAUUSD","price":2345,"ts":1767225600}', { intLen: 4, fracLen: 1 }));

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
