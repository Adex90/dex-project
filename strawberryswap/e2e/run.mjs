// FULL-STACK END-TO-END (local): anvil + real contracts + static frontend in real Chromium + the Go Automator.
//
//   browser (snarkjs in a Web Worker, mock wallet) ── deposit ─► DarkPool ◄── Go Automator (batch proof) ── signer ─► anvil
//                                                   ── intent ─►           ── POST opening ─►
//                                                   ◄─ output note recovered, withdrawn ─
//
// Needs: anvil/forge/cast, solc 0.8.26 (set SOLC), the Go binary (AUTOMATOR_BIN), Node + playwright (PLAYWRIGHT_MODULE),
// and the circuit artifacts built in circuits/build (joinsplit + batch_swap_n4 dev keys).
import { spawn, execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const env = process.env;
const SOLC = env.SOLC ?? "";
const AUTOMATOR_BIN = env.AUTOMATOR_BIN ?? (() => { throw new Error("set AUTOMATOR_BIN"); })();
const PW = createRequire(import.meta.url)(env.PLAYWRIGHT_MODULE ?? "playwright");
const PK0 = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"; // anvil #0 (deployer)
const USER = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"; // anvil #1 (the retail user)
const RPC = "http://127.0.0.1:8545", SITE = "http://127.0.0.1:4173", RELAY = "http://127.0.0.1:8787";
const procs = [];
const log = (...a) => console.log("[e2e]", ...a);
const sh = (cmd, args, opts = {}) => {
  try {
    return execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...opts });
  } catch (e) {
    // never echo the full command line (it can contain kilobytes of bytecode)
    throw new Error(`${cmd} ${args.slice(0, 3).join(" ")} … failed:\n${String(e.stderr ?? e.message).slice(-1500)}`);
  }
};
const cast = (...a) => sh("cast", a).trim();
const bg = (name, cmd, args, opts = {}) => {
  const p = spawn(cmd, args, { stdio: ["ignore", "pipe", "pipe"], ...opts });
  const out = [];
  p.stdout.on("data", (d) => out.push(String(d)));
  p.stderr.on("data", (d) => out.push(String(d)));
  p.logs = () => out.join("");
  procs.push(p);
  p.name = name;
  return p;
};
const until = async (what, fn, ms = 60_000, step = 500) => {
  const t0 = Date.now();
  for (;;) {
    try { const v = await fn(); if (v) return v; } catch { /* retry */ }
    if (Date.now() - t0 > ms) throw new Error(`timeout waiting for ${what}`);
    await new Promise((r) => setTimeout(r, step));
  }
};

async function main() {
  // ------------------------------------------------------------------------------------------ chain + contracts
  bg("anvil", "anvil", ["--port", "8545", "--block-time", "2", "--silent"]);
  await until("anvil", async () => cast("chain-id", "--rpc-url", RPC) === "31337");
  const startBlock = cast("block-number", "--rpc-url", RPC);

  const hex = readFileSync(join(root, "contracts/test/fixtures/PoseidonT3Code.sol"), "utf8").match(/hex"([0-9a-f]+)"/)[1];
  const poseidon = JSON.parse(cast("send", "--private-key", PK0, "--rpc-url", RPC, "--json", "--create", "0x" + hex)).contractAddress;
  log("poseidon", poseidon);

  const forgeArgs = ["script", "script/DeployLocal.s.sol", "--rpc-url", RPC, "--private-key", PK0, "--broadcast", ...(SOLC ? ["--use", SOLC] : [])];
  const out = sh("forge", forgeArgs, { cwd: join(root, "contracts"), env: { ...env, POSEIDON: poseidon, USER, FOUNDRY_OFFLINE: "true" } });
  const addr = Object.fromEntries([...out.matchAll(/ADDR_(\w+)\s+(0x[0-9a-fA-F]{40})/g)].map((m) => [m[1], m[2]]));
  assert.ok(addr.POOL && addr.TOKEN0 && addr.TOKEN1, "deployment addresses not found");
  log("pool", addr.POOL);
  const sym = (t) => cast("call", t, "symbol()(string)", "--rpc-url", RPC).replaceAll('"', "");
  const [s0, s1] = [sym(addr.TOKEN0), sym(addr.TOKEN1)];
  const bal = (t, a) => BigInt(cast("call", t, "balanceOf(address)(uint256)", a, "--rpc-url", RPC).split(" ")[0]);

  // ------------------------------------------------------------------------------------------ frontend (static export)
  log("building the static frontend …");
  sh("npm", ["run", "build"], {
    cwd: join(root, "frontend"),
    env: { ...env, NEXT_TELEMETRY_DISABLED: "1", REQUIRE_ZK: "1", NEXT_PUBLIC_CHAIN_ID: "31337", NEXT_PUBLIC_CHAIN_NAME: "Local", NEXT_PUBLIC_RPC_URL: RPC,
      NEXT_PUBLIC_DARKPOOL_ADDRESS: addr.POOL, NEXT_PUBLIC_DEPLOY_BLOCK: startBlock, NEXT_PUBLIC_AUTOMATOR_URL: RELAY },
  });
  sh("npm", ["run", "verify:static"], { cwd: join(root, "frontend") });
  bg("site", "python3", ["-m", "http.server", "4173", "--bind", "127.0.0.1", "--directory", join(root, "frontend/out")]);

  // ------------------------------------------------------------------------------------------ signer gateway + Automator
  bg("signer", "node", [join(here, "dev-signer.mjs")]);
  const cfg = {
    listen: "127.0.0.1:8787", rpcUrl: RPC, signerUrl: "http://127.0.0.1:8788", chainId: 31337, pool: addr.POOL, deployBlock: Number(startBlock),
    batchSize: 4, minSwaps: 1, intervalSeconds: 15, jitterSeconds: 0, gasLimit: 6_000_000,
    snarkjs: join(root, "circuits/node_modules/.bin/snarkjs"),
    wasm: join(root, "circuits/build/batch_swap_n4/main_n4_js/main_n4.wasm"), zkey: join(root, "circuits/build/batch_swap_n4/batch_swap_n4.zkey"),
    proverTmpDir: "/tmp", devAllowDirectEgress: true, // no Tor in CI; production refuses to start without it
  };
  mkdirSync(join(here, "build"), { recursive: true });
  writeFileSync(join(here, "build/automator.json"), JSON.stringify(cfg));
  const automator = bg("automator", AUTOMATOR_BIN, ["serve", "--config", join(here, "build/automator.json")]);
  await until("automator", async () => (await fetch(`${RELAY}/healthz`)).status === 204, 20_000);
  log("automator up");

  // ------------------------------------------------------------------------------------------ the browser
  const browser = await PW.chromium.launch();
  const ctx = await browser.newContext();
  const sigs = [];
  await ctx.exposeFunction("__recordSig", (s) => void sigs.push(s));
  await ctx.addInitScript({ content: readFileSync(join(here, "wallet-mock.js"), "utf8").replace("__ACCOUNT__", USER) });
  const page = await ctx.newPage();
  const requests = [];
  // blob:/data: are in-page (snarkjs spawns helper workers from blobs); only real network requests count
  page.on("request", (r) => { if (/^https?:/.test(r.url())) requests.push({ url: r.url(), method: r.method(), body: r.postData() ?? "" }); });
  page.on("response", (r) => { if (r.status() === 404) log("404:", r.url()); });
  page.on("pageerror", (e) => log("PAGE ERROR:", e.message));
  page.on("console", (m) => { if (m.type() === "error") log("console.error:", m.text().slice(0, 200)); });

  await page.goto(`${SITE}/trade/`);
  await page.getByText("0x7099…79C8").waitFor({ timeout: 30_000 }); // the mock wallet is already authorised
  await page.getByRole("button", { name: "Unlock shielded wallet" }).click();
  log("unlocking (downloads + verifies ~11 MB of proving artifacts, then scans the chain) …");
  await page.getByText("local tree == on-chain root").waitFor({ timeout: 180_000 });
  log("wallet unlocked and in sync");

  // --- deposit 1000 of token0 (real proof, generated in the worker)
  await page.locator('select[aria-label="Deposit asset"]').selectOption("0");
  await page.locator('input[aria-label="Deposit amount"]').fill("1000");
  const t0 = Date.now();
  await page.getByRole("button", { name: "Deposit", exact: true }).click();
  await page.getByText("✓ Recovering the new note from chain events").waitFor({ timeout: 240_000 });
  log(`deposit done in ${((Date.now() - t0) / 1000).toFixed(1)} s (incl. proof generation)`);
  assert.equal(bal(addr.TOKEN0, USER), 9_000n * 10n ** 18n, "1000 token0 left the user's public balance");

  // --- private swap: sell 100 token0 at the curve price (the Automator batches it)
  await page.locator('select[aria-label="Asset to sell"]').selectOption("0");
  await page.locator('input[aria-label="Amount to sell"]').fill("100");
  const t1 = Date.now();
  await page.getByRole("button", { name: "Create private intent" }).click();
  await page.getByText("✓ Sending the intent to the Automator").waitFor({ timeout: 300_000 });
  log(`intent created + sent in ${((Date.now() - t1) / 1000).toFixed(1)} s`);

  // --- the Go Automator proves and settles the batch; the browser then recovers the sealed output
  log("waiting for the Automator to settle the batch (Go prover) …");
  await page.locator("li", { hasText: "→ ≥" }).getByText("settled", { exact: true }).waitFor({ timeout: 480_000 });
  log("intent settled; output note recovered by the wallet");

  // --- withdraw the output note (token1)
  const row = page.locator("li", { hasText: s1 }).filter({ has: page.getByRole("button", { name: "Withdraw" }) });
  const before1 = bal(addr.TOKEN1, USER);
  await row.getByRole("button", { name: "Withdraw" }).click();
  await page.getByText("✓ Updating balances").last().waitFor({ timeout: 240_000 });
  const gained = bal(addr.TOKEN1, USER) - before1;
  log(`withdrew ${Number(gained / 10n ** 15n) / 1000} ${s1} to the public balance`);
  assert.ok(gained > 190n * 10n ** 18n && gained < 200n * 10n ** 18n, `payout ≈ 100 × price(≈2) minus nothing: got ${gained}`);

  // ------------------------------------------------------------------------------------------ privacy assertions on everything the page sent
  const hosts = new Set(requests.map((r) => new URL(r.url).host));
  assert.deepEqual([...hosts].sort(), ["127.0.0.1:4173", "127.0.0.1:8545", "127.0.0.1:8787"], `the page talked to unexpected hosts: ${[...hosts]}`);
  const relay = requests.filter((r) => r.url.startsWith(RELAY));
  const posts = relay.filter((r) => r.method === "POST");
  assert.equal(posts.length, 1, "exactly one intent was transmitted");
  assert.deepEqual(Object.keys(JSON.parse(posts[0].body)).sort(), ["deadline", "destBlinding", "destPubKey", "dir", "minOut", "refundPubKey", "value"], "the relay received only the intent opening");
  assert.ok(sigs.length >= 1, "the wallet signed the key-derivation message");
  for (const r of requests) {
    for (const s of sigs) assert.ok(!r.url.includes(s) && !r.body.includes(s), "the key-derivation signature leaked in a request");
    if (!r.url.startsWith(RPC)) assert.ok(!/inPrivKey|privKey|witness/i.test(r.body), `a witness/private field left the device in ${r.url}`);
  }
  log(`privacy: ${requests.length} requests to 3 local hosts only; the relay got 1 POST with the 7 opening fields; the signature never left the page`);

  await browser.close();
  log("E2E PASSED");
}

let failed = false;
try { await main(); } catch (e) {
  failed = true;
  console.error("[e2e] FAILED:", e);
  for (const p of procs) if (["automator", "signer"].includes(p.name)) console.error(`--- ${p.name} output ---\n${p.logs().slice(-3000)}`);
} finally {
  for (const p of procs) p.kill("SIGKILL");
}
process.exit(failed ? 1 : 0);
