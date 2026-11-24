/**
 * End-to-end smoke test: drives the real app in Chromium against a running
 * backend (MODO_DEMO=true, so the simulated biometric provider and demo SPEI
 * deposits exist) and a Next.js server.
 *
 *   FRONTEND_URL=http://localhost:3000 node e2e/smoke.mjs [screenshot-dir]
 *
 * Requires the `playwright` package (resolved from NODE_PATH or a global
 * install) and a Chromium build (PLAYWRIGHT_BROWSERS_PATH). Chromium runs with
 * a fake camera (an animated test pattern), so the liveness frames differ.
 *
 * First pass: landing -> sign-up -> KYC-locked dashboard -> the four-step
 * verification at /dashboard/verify (identity, INE photos and details,
 * liveness + declarations, review + agreements) -> VERIFICADO, unlocked
 * without a reload -> deposit, fixed-term investment, the Account tab's previews
 * (the Sovereign Yield counter, the debit card), a withdrawal to the linked
 * bank account, Proof of Reserve, ledger,
 * moving between the dashboard's tabs (Vault, Discover, Account, Invest,
 * Reserve), the Document Inbox (a trade confirmation per executed order and
 * the month's statement, reconciled with the ledgers; the zero-commission
 * breakdown, the reserve seal before and after a snapshot, the JSON download,
 * printing and Save as PDF), Legal Beneficiaries (the live allocation bar,
 * Save only at exactly 100%, Split evenly, versions, a save refused because
 * another device saved first, every version in the security log), the
 * Tax Center (the year's summary against the cash ledger, the W-8BEN
 * certified and renewed, the accounting CSV reconciled before it is saved,
 * the Constancia de Retenciones printed), Savings plans (created from the
 * checkout with its first purchase at once, the receipt's next dates, the
 * Vault's list live with a plan from another device, Pause, Resume and
 * Cancel)
 * and the Security page (devices signed in, Revoke and "Sign out all
 * other devices", the inactivity timeout, a password change, the security
 * log; then, for a client of its own on a phone, two-step verification turned
 * on from there and the inactivity sign-out on a fake clock). It ends on asset pages opened by their address (AUG1, an unknown
 * ticker, an address that is not a ticker, a market without live prices) and
 * on Discover: the catalog, its top movers and collections, its search and
 * its screener (a collection presets it, toggles narrow it), and a first buy
 * from it (a fraction of STSLA), sold back from the Vault with "Max". On AUG1 the pro chart is
 * read as a line and as candles (crosshair, legend, keyboard, ranges), and the
 * chart type it was left in is remembered on STSLA's page.
 * Second pass: signing in again lands directly on the Vault, which is live
 * over its WebSocket: a deposit made outside the browser reaches the screen
 * with REST blocked, a server restart (1001) reconnects, and a session the
 * server ends (4001) returns to sign-in. The Vault's allocation shows the
 * shares its own figures give, exactly, and follows the deposit and the gold
 * buy live. In between, trading with REST for
 * the vault blocked: a gold buy on another device appears in the asset list,
 * its row opens the asset's page, a buy whose answer is lost is retried with
 * the same idempotency key and fills once, refusals show their reason; then,
 * on a phone-sized screen (bottom tab bar instead of the sidebar, Discover
 * included), "Max" sells the whole position.
 * The theme is dark only ("Sovereign Gold"); the passes still differ in the
 * browser's preferred color scheme, which must not change it.
 */
import { createRequire } from "node:module";
import { createHmac } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import { isDeepStrictEqual } from "node:util";

// Node >= 22.18 strips TypeScript types, so the app's own (framework-free)
// KYC library builds an identity that passes every frontend and backend check.
import { curpCheckDigit, sampleIdentity, SAMPLE_IDENTITY_SPACE, todayInMexicoCity } from "../src/lib/kyc.ts";
// And exact decimals, to read the documents' figures as the screens do.
import { formatMoney, isZero, toUnits } from "../src/lib/decimal.ts";
// And the Tax Center's RFC 4180 parser, to read the accounting export it saves.
import { CSV_HEADER, parseCsv } from "../src/lib/taxes.ts";
// And the allocation's exact math, to know what the Vault must show for the figures the API gives.
import { allocation, formatShare } from "../src/lib/allocation.ts";
// And the Sovereign Yield preview's, to check the Account tab's live counter against it.
import { SOVEREIGN_YIELD_PCT, accrued, formatTruncated, monthWindow } from "../src/lib/wealth.ts";
// And the savings plans' calendar words, to read the dates a plan shows in Mexico City time.
import { formatPlanDate, formatPlanDay } from "../src/lib/savings-plans.ts";

// createRequire honors NODE_PATH (ESM imports do not), so a global install works.
const { chromium } = createRequire(import.meta.url)("playwright");

const BASE = process.env.FRONTEND_URL ?? "http://localhost:3000";
// The backend the frontend was built against (NEXT_PUBLIC_API_URL): for deposits made outside the browser.
const API = process.env.API_URL ?? "http://localhost:8080";
const OUT = process.argv[2] ?? ".";
mkdirSync(OUT, { recursive: true });

const runId = Date.now();
const email = `e2e-${runId}@example.mx`;
// Changed on the Security page at the end of the first pass: later sign-ins use the new one.
let password = "austrian-school-1926";
// CURP, RFC and voter key must be unique among verified files (IDENTIDAD_EN_USO),
// so every run uses a different identity. Seeds wrap at SAMPLE_IDENTITY_SPACE
// (~10^7): distinct for runs started more than a millisecond apart within ~2.7 h,
// and a collision needs the very same millisecond modulo that space.
const seed = runId % SAMPLE_IDENTITY_SPACE;
const identity = sampleIdentity(seed, todayInMexicoCity());
const errors = [];

function check(condition, message) {
  if (!condition) throw new Error(`FAILED: ${message}`);
  console.log(`ok - ${message}`);
}

/** Signs in through the API, as another device would: a session of its own (named after `userAgent`). */
async function apiLogin(emailAddress, secret, userAgent) {
  const response = await fetch(`${API}/api/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json", ...(userAgent ? { "user-agent": userAgent } : {}) },
    body: JSON.stringify({ email: emailAddress, password: secret }),
  });
  const body = await response.json();
  if (body.estado !== "OK") throw new Error(`API sign-in failed: ${JSON.stringify(body)}`);
  return body.token;
}

/**
 * A fresh Proof of Reserve cut over every account, as it stands now: the stack
 * cuts on demand (CORTE_MIN_INTERVALO_SEG=0), and a new client's first proof
 * is not in the latest cut, so the server cuts again. Returns the public copy.
 */
async function freshCut(emailAddress, secret) {
  if ((await apiRegister(emailAddress, secret)) !== 201) throw new Error(`could not register ${emailAddress}`);
  const proof = await fetch(`${API}/api/v1/reserva/prueba`, {
    headers: { authorization: `Bearer ${await apiLogin(emailAddress, secret)}` },
  });
  if (proof.status !== 200) throw new Error(`the new client's proof answered ${proof.status}`);
  return (await fetch(`${API}/api/v1/reserva/cortes/ultimo`)).json();
}

/** The HTTP status of an authenticated GET: 200 while `token`'s session lives, 401 once it ended. */
async function apiStatus(path, token) {
  return (await fetch(`${API}${path}`, { headers: { authorization: `Bearer ${token}` } })).status;
}

/** Devices that sign in through the API on the Security page's tests. */
const IPHONE_UA =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const WINDOWS_UA =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.2592.68";

/** A computed style once the element's CSS transitions (hover, current) have finished. */
async function settledStyle(locator, property) {
  return locator.evaluate(async (el, name) => {
    await Promise.all(el.getAnimations().map((a) => a.finished.catch(() => undefined)));
    return getComputedStyle(el)[name];
  }, property);
}

/** The session token this page holds (sessionStorage), and the session id (`sid`) it carries. */
async function pageToken(page) {
  return page.evaluate(() => JSON.parse(sessionStorage.getItem("synthetic-capital-session")).state.token);
}
function sidOf(token) {
  return JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString()).sid;
}

/** RFC 4648 base32 (the authenticator key the setup page shows, spaces allowed). */
function base32Decode(text) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = 0;
  let value = 0;
  const bytes = [];
  for (const ch of text.replace(/[\s=]/g, "").toUpperCase()) {
    const v = alphabet.indexOf(ch);
    if (v < 0) throw new Error(`not base32: ${ch}`);
    value = (value << 5) | v;
    bits += 5;
    if (bits >= 8) {
      bytes.push((value >>> (bits - 8)) & 0xff);
      bits -= 8;
    }
  }
  return Buffer.from(bytes);
}

/** The 6-digit TOTP code (RFC 6238: SHA-1, 30 s) of `secret` at time step `step`, as an authenticator app shows it. */
function totpCode(secret, step) {
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(step));
  const h = createHmac("sha1", secret).update(counter).digest();
  const offset = h[h.length - 1] & 0xf;
  return String((h.readUInt32BE(offset) & 0x7fffffff) % 1_000_000).padStart(6, "0");
}

/** Registers a client through the API: a new account, still without KYC, holding nothing. */
async function apiRegister(emailAddress, secret) {
  const response = await fetch(`${API}/api/v1/auth/registro`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      tipo_persona: "FISICA",
      nombre: "Corte",
      apellido_paterno: "Nuevo",
      email: emailAddress,
      password: secret,
    }),
  });
  return response.status;
}

/** A demo SPEI deposit through the API (MODO_DEMO), not through this browser. */
async function apiDeposit(token, monto) {
  const response = await fetch(`${API}/api/v1/ifpe/depositos/simulados`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({ monto, clave_idempotencia: crypto.randomUUID() }),
  });
  return response.status;
}

/** A market order through the API, as another device would place it. */
async function apiOrder(token, order) {
  const response = await fetch(`${API}/api/v1/market/order`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({ ...order, idempotency_key: crypto.randomUUID() }),
  });
  return { status: response.status, receipt: await response.json() };
}

/** Collapses the whitespace `innerText` puts between a component's pieces. */
const flat = (text) => text.replace(/\s+/g, " ").trim();

/**
 * The vault's allocation from its own figures (GET /api/v1/vault), through
 * the app's exact math (lib/allocation.ts): each part's key and share, and
 * the bar's blocks in basis points. What the Vault must show.
 */
async function expectedAllocation(token) {
  const response = await fetch(`${API}/api/v1/vault`, { headers: { authorization: `Bearer ${token}` } });
  const vault = await response.json();
  const split = allocation(vault.balance, vault.holdings);
  return {
    parts: split.parts.map((part) => [part.key, formatShare(part.basisPoints, part.value)]),
    bar: split.parts.filter((part) => part.basisPoints > 0).map((part) => part.basisPoints),
  };
}

/** Waits until the Vault's allocation reads exactly `expected` (`expectedAllocation`), in the list and in the bar. */
async function allocationShows(page, expected, timeout = 5_000) {
  await page.waitForFunction(
    (want) =>
      JSON.stringify({
        parts: [...document.querySelectorAll("[data-testid=allocation-part]")].map((row) => [
          row.getAttribute("data-key"),
          row.querySelector("[data-testid=allocation-share]")?.textContent ?? null,
        ]),
        bar: [...document.querySelectorAll("[data-testid=allocation-segment]")].map((block) =>
          Number(block.getAttribute("data-share")),
        ),
      }) === want,
    JSON.stringify(expected),
    { timeout },
  );
}

/**
 * An API refusal answered in the browser instead of by the backend, for
 * states a run cannot reach for real (the market closed, a conflicting key).
 * It carries the API's CORS header, or the browser would hide the answer.
 */
function apiRefusal(status, codigo) {
  return {
    status,
    headers: { "access-control-allow-origin": new URL(BASE).origin },
    json: { error: { codigo, mensaje: "respuesta simulada por la prueba" } },
  };
}

// The browser logs its own "Failed to load resource" line for every API call
// that fails. The calls this test fails on purpose (a refused order, a lost
// answer, a ticker that does not exist) are announced first: each entry
// excuses one such line for that API path, and nothing else.
const deliberateFailures = [];
const expectFailure = (path) => deliberateFailures.push(path);

/** Waits for the header's stream indicator to read `status` ("live" only once a snapshot applied). */
async function streamStatus(page, status, timeout = 15_000) {
  await page.getByTestId("realtime-status").and(page.locator(`[data-status=${status}]`)).waitFor({ timeout });
}

/** The dashboard's main navigation (the sidebar from lg up, the bottom tab bar below). */
const mainNav = (page) => page.getByRole("navigation", { name: "Main" });

/** Opens a dashboard tab from the main navigation and checks it is marked current. */
async function openTab(page, name) {
  const link = mainNav(page).getByRole("link", { name, exact: true });
  await link.click();
  await page.waitForURL(name === "Vault" ? "**/dashboard" : `**/dashboard/${name.toLowerCase()}`);
  check((await link.getAttribute("aria-current")) === "page", `the ${name} tab is the current page`);
}

/**
 * Full-page screenshot once finite animations (step reveal, transitions) have
 * finished; `fullPage: false` keeps to the viewport, as a person sees a modal.
 */
async function shot(page, name, { fullPage = true } = {}) {
  await page.evaluate(() =>
    Promise.race([
      Promise.all(
        document
          .getAnimations()
          .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
          .map((a) => a.finished.catch(() => undefined)),
      ),
      new Promise((resolve) => setTimeout(resolve, 2000)),
    ]),
  );
  // A full-page capture draws fixed and sticky parts (the sidebar, the header) at
  // the current scroll offset: capture from the top, then put the scroll back.
  const scrollY = fullPage
    ? await page.evaluate(() => {
        const y = window.scrollY;
        window.scrollTo({ top: 0, behavior: "instant" });
        return y;
      })
    : 0;
  await page.screenshot({ path: `${OUT}/${name}.png`, fullPage });
  if (scrollY > 0) await page.evaluate((y) => window.scrollTo({ top: y, behavior: "instant" }), scrollY);
}

/**
 * A JPEG photo-like test image rendered in the page (a canvas with a
 * gradient, a label and per-image noise), so the two sides differ byte-wise
 * and comfortably exceed the API's 1 KB minimum.
 */
async function renderJpeg(page, label, hue) {
  const dataUrl = await page.evaluate(
    ({ label, hue }) => {
      const canvas = document.createElement("canvas");
      canvas.width = 1012;
      canvas.height = 638;
      const ctx = canvas.getContext("2d");
      const g = ctx.createLinearGradient(0, 0, canvas.width, canvas.height);
      g.addColorStop(0, `hsl(${hue} 60% 85%)`);
      g.addColorStop(1, `hsl(${(hue + 60) % 360} 50% 55%)`);
      ctx.fillStyle = g;
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      for (let i = 0; i < 1500; i++) {
        ctx.fillStyle = `hsla(${Math.floor(Math.random() * 360)} 50% 40% / 0.35)`;
        ctx.fillRect(Math.random() * canvas.width, Math.random() * canvas.height, 6, 6);
      }
      ctx.fillStyle = "#111";
      ctx.font = "bold 56px sans-serif";
      ctx.fillText(label, 60, 120);
      ctx.font = "28px monospace";
      ctx.fillText("INSTITUTO NACIONAL ELECTORAL (TEST IMAGE)", 60, 200);
      return canvas.toDataURL("image/jpeg", 0.9);
    },
    { label, hue },
  );
  return Buffer.from(dataUrl.slice(dataUrl.indexOf(",") + 1), "base64");
}

const browser = await chromium.launch({
  args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream"],
});
try {
  for (const colorScheme of ["light", "dark"]) {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 1000 },
      colorScheme,
      permissions: ["camera"],
    });
    const page = await context.newPage();
    // The resource URL matters: "Failed to load resource" messages do not name it in their text.
    page.on("console", (m) => {
      if (m.type() === "error") errors.push(`[${colorScheme}] ${m.text()} (${m.location().url || "no url"})`);
    });
    page.on("pageerror", (e) => errors.push(`[${colorScheme}] ${e.message}`));

    if (colorScheme === "dark") {
      // Second pass. Every vault WebSocket the page opens goes through this route:
      // forwarded to the real backend (same handshake), recorded, and closable by
      // the test as the server would close it.
      const vaultSockets = [];
      const frames = [];
      await page.routeWebSocket(/\/api\/v1\/vault\/ws$/, (ws) => {
        const server = ws.connectToServer();
        server.onMessage((message) => {
          frames.push(JSON.parse(String(message)));
          ws.send(message);
        });
        vaultSockets.push(ws);
      });

      // A verified client signs in and lands directly on the dashboard.
      await page.goto(`${BASE}/auth/login`);
      await page.fill("#email", email);
      await page.fill("#password", password);
      await page.click("button[type=submit]");
      await page.waitForURL("**/dashboard");
      check(true, "a verified client signs in straight to the dashboard (no onboarding detour)");
      // The Vault home: 15,000 cash plus 10,000 invested, under the reserve seal of the first pass's cut.
      await page.getByTestId("vault-total").filter({ hasText: "25,000" }).waitFor();
      await page.getByTestId("reserve-badge").and(page.locator("[data-status=verified]")).waitFor();
      check(
        // Case-blind: the seal is set in capitals by CSS, which innerText reports.
        /100% reserve verified/i.test(await page.getByTestId("reserve-badge").innerText()),
        "the vault shows the verified reserve seal",
      );
      await page.getByTestId("kyc-header-badge").and(page.locator("[data-status=VERIFICADO]")).waitFor();
      check((await page.getByTestId("kyc-banner").count()) === 0, "second pass: no KYC banner for the verified client");
      // The allocation: cash and the fixed-term contract, nothing in the market yet, in the
      // exact shares the vault's own figures give (they add up to 100.00 %).
      let split = await expectedAllocation(await apiLogin(email, password));
      await allocationShows(page, split);
      check(
        JSON.stringify(split.parts.map(([key]) => key)) === JSON.stringify(["cash", "fixed-term"]) &&
          split.bar.reduce((sum, share) => sum + share, 0) === 10_000 &&
          (await page.getByTestId("allocation-split").innerText()).includes(split.parts[0][1]),
        `the Vault's allocation splits it exactly: cash ${split.parts[0][1]}, fixed term ${split.parts[1][1]}`,
      );
      await streamStatus(page, "live");
      check(frames[0]?.type === "vault.snapshot", "the vault stream opens with a snapshot");
      await shot(page, "vault-desktop");

      // Live: a deposit made elsewhere reaches this screen over the WebSocket. REST
      // for the vault is blocked meanwhile, so nothing else could have brought it.
      await page.route("**/api/v1/vault", (route) => route.abort());
      const otherDevice = await apiLogin(email, password);
      check((await apiDeposit(otherDevice, "1234.56")) === 201, "a deposit of 1,234.56 is made outside this browser");
      await page.getByTestId("vault-total").filter({ hasText: "26,234" }).waitFor({ timeout: 5_000 });
      check(true, "the total updates live, without a reload or a REST call");
      const spei = frames.find((m) => m.type === "spei.transfer");
      check(
        spei?.transfer.direction === "inbound" && spei.transfer.amount === "1234.56000000" && spei.seq > frames[0].seq,
        `the deposit arrived as spei.transfer at a newer version (seq ${frames[0]?.seq} -> ${spei?.seq})`,
      );
      // The rest of the dashboard follows: the chart's cash reads the new balance.
      await page.getByTestId("portfolio-chart").filter({ hasText: "16,234.56" }).waitFor({ timeout: 5_000 });
      check(true, "the cash chart follows the deposit");
      split = await expectedAllocation(otherDevice);
      await allocationShows(page, split);
      check(true, `the allocation follows the deposit live: cash ${split.parts[0][1]}, fixed term ${split.parts[1][1]}`);
      await page.unroute("**/api/v1/vault");

      // A deploy: the server closes with 1001; the dashboard reconnects and catches up.
      await vaultSockets.at(-1).close({ code: 1001, reason: "el servidor se reinicia" });
      await streamStatus(page, "reconnecting", 5_000);
      await streamStatus(page, "live");
      check(vaultSockets.length === 2, "after the server goes away (1001) the dashboard reconnects on its own");
      await page.getByTestId("vault-total").filter({ hasText: "26,234" }).waitFor();

      await openTab(page, "Account");
      await page.getByTestId("vault-available").filter({ hasText: "16,234.56" }).waitFor();
      check(true, "the Account tab shows the cash balance, deposit included");
      check(vaultSockets.length === 2, "switching tabs keeps the same socket");

      // -------------------------------------------------------------------
      // Trading. REST for the vault stays blocked: the asset list, the
      // position and the cash on screen can only follow the WebSocket (an
      // order's commit raises the vault's seq), never a write by the page.
      // -------------------------------------------------------------------
      await openTab(page, "Vault");
      await page.getByTestId("asset-list-empty").waitFor();
      await page.route("**/api/v1/vault", (route) => route.abort());
      const elsewhere = await apiOrder(otherDevice, { ticker: "AUG1", side: "buy", amount: "1250.00" });
      check(
        elsewhere.status === 201 &&
          elsewhere.receipt.status === "filled" &&
          elsewhere.receipt.quantity === "0.77520000" &&
          elsewhere.receipt.amount === "1249.93000000",
        "a buy of 1,250.00 of gold on another device fills 0.7752 g for 1,249.93",
      );
      const goldRow = page.getByTestId("asset-row").and(page.locator("[data-ticker=AUG1]"));
      await goldRow.waitFor({ timeout: 5_000 });
      check((await goldRow.innerText()).includes("Tokenized gold"), "the holding appears in the asset list live");
      split = await expectedAllocation(otherDevice);
      await allocationShows(page, split);
      const goldPart = page.getByTestId("allocation-part").and(page.locator("[data-key=AUG1]"));
      check(
        JSON.stringify(split.parts.map(([key]) => key)) === JSON.stringify(["cash", "fixed-term", "AUG1"]) &&
          split.bar.reduce((sum, share) => sum + share, 0) === 10_000 &&
          (await goldPart.getByRole("link").getAttribute("href")) === "/dashboard/asset/AUG1",
        `the allocation gains AUG1 live (${split.parts[2][1]}), still adding up to 100.00 %; its row opens the asset`,
      );
      await page.getByTestId("allocation-segment").and(page.locator("[data-key=AUG1]")).hover();
      check(
        /^AUG1 ?Tokenized gold, 1 g/.test(flat(await page.getByTestId("allocation-readout").innerText())),
        "pointing at AUG1's block of the bar reads its name, value and share under it",
      );
      await page.mouse.move(0, 0);

      // A click anywhere on the row (here, its value) opens the asset's page inside the dashboard.
      await goldRow.locator("td").last().click();
      await page.waitForURL("**/dashboard/asset/AUG1");
      const detail = page.getByTestId("asset-detail");
      await detail.getByText("0.7752 AUG1").waitFor();
      check(vaultSockets.length === 2, "the row opens AUG1's page on the same socket, with the position (0.7752)");

      // Buy 1,250.00 more on the numpad. The first answer is lost on its way
      // back although the order filled; the retry carries the same
      // idempotency key and gets that order back, so it fills only once.
      await page.getByTestId("asset-buy").click();
      const checkout = page.getByTestId("order-checkout");
      for (const key of ["1", "2", "5", "0"]) await checkout.getByRole("button", { name: key, exact: true }).click();
      check(
        (await checkout.getByTestId("order-summary").innerText()).includes("0.7752 AUG1"),
        "the drawer estimates 0.7752 AUG1 for 1,250.00",
      );
      const sent = [];
      let retryStatus = 0;
      await page.route("**/api/v1/market/order", async (route) => {
        if (route.request().method() !== "POST") return route.continue();
        sent.push(JSON.parse(route.request().postData() ?? "{}"));
        const response = await route.fetch();
        if (sent.length === 1) return route.abort("connectionreset");
        retryStatus = response.status();
        return route.fulfill({ response });
      });
      expectFailure("/api/v1/market/order");
      await checkout.getByTestId("order-confirm").click();
      await checkout.getByRole("alert").filter({ hasText: "Cannot reach the bank's servers" }).waitFor();
      await checkout.getByTestId("order-confirm").click();
      const receipt = checkout.getByTestId("order-receipt");
      await receipt.waitFor();
      await page.unroute("**/api/v1/market/order");
      check(
        sent.length === 2 &&
          sent.every((o) => o.ticker === "AUG1" && o.side === "buy" && o.amount === "1250.00") &&
          /^[0-9a-f-]{36}$/.test(sent[0].idempotency_key) &&
          sent[1].idempotency_key === sent[0].idempotency_key,
        "after a lost answer, the retry sends the same order with the same idempotency key",
      );
      check(retryStatus === 200, `the server answers the retry with the order it already filled (${retryStatus})`);
      const receiptText = await receipt.innerText();
      check(
        receiptText.includes("Order filled") && receiptText.includes("Bought 0.7752 AUG1 at $1,612.40."),
        "the receipt reads: bought 0.7752 AUG1 at $1,612.40",
      );
      await shot(page, "asset-receipt");
      await checkout.getByRole("button", { name: "Done" }).click();
      await checkout.waitFor({ state: "detached" });
      await detail.getByText("1.5504 AUG1").waitFor({ timeout: 5_000 });
      const latest = frames.findLast((m) => m.type === "vault.snapshot");
      check(
        latest?.vault.holdings.some((h) => h.ticker === "AUG1" && h.shares === "1.55040000") === true &&
          latest.vault.balance.available_cash === "13734.70000000",
        "the WebSocket brings the position (1.5504: filled once) and the cash (13,734.70)",
      );
      await page.mouse.move(0, 0);
      await shot(page, "asset-desktop-position");

      // Refusals, as the API would answer them, show their reason in the drawer.
      await page.getByTestId("asset-buy").click();
      await checkout.getByText("Available: $13,734.70").waitFor();
      check(true, "the drawer offers the cash left after both buys ($13,734.70)");
      for (const key of ["5", "0", "0"]) await checkout.getByRole("button", { name: key, exact: true }).click();
      for (const [status, codigo, reason] of [
        [400, "VALIDACION", "Enter an amount in pesos, with at most two decimals."],
        [409, "CLAVE_IDEMPOTENCIA_REUTILIZADA", "This request conflicts with a previous operation."],
        [422, "SALDO_INSUFICIENTE", "Insufficient available balance in your vault."],
        [503, "MERCADO_NO_DISPONIBLE", "The market opens once live prices are connected. No order was placed."],
      ]) {
        await page.route("**/api/v1/market/order", (route) =>
          route.request().method() === "POST" ? route.fulfill(apiRefusal(status, codigo)) : route.continue(),
        );
        expectFailure("/api/v1/market/order");
        await checkout.getByTestId("order-confirm").click();
        await checkout.getByRole("alert").filter({ hasText: reason }).waitFor();
        await page.unroute("**/api/v1/market/order");
      }
      check(true, "refused orders (400, 409, 422, 503) show their reason in the drawer");
      await checkout.getByRole("button", { name: "Close" }).click();
      await checkout.waitFor({ state: "detached" });
      check((await detail.getByText("1.5504 AUG1").count()) === 1, "a refused order leaves the position as it was");

      await page.getByTestId("asset-back").click();
      await page.waitForURL("**/dashboard");
      await goldRow.filter({ hasText: "1.5504" }).waitFor();
      await page.getByTestId("vault-total").filter({ hasText: "26,234" }).waitFor();
      check(vaultSockets.length === 2, "back on the Vault, the row reads 1.5504 and the total is unchanged");
      await page.mouse.move(0, 0);
      await shot(page, "vault-desktop-holding");
      await page.unroute("**/api/v1/vault");

      // A phone: the sidebar gives way to the bottom tab bar.
      await page.setViewportSize({ width: 390, height: 844 });
      const tabBar = mainNav(page);
      const box = await tabBar.boundingBox();
      check(box !== null && box.y + box.height >= 844 - 1 && box.width === 390, "phones get the bottom tab bar");
      await openTab(page, "Discover");
      await page.getByTestId("market-row").first().waitFor();
      const tabWidths = await tabBar.getByRole("link").evaluateAll((links) => links.map((l) => l.getBoundingClientRect().width));
      check(
        tabWidths.length === 5 && tabWidths.every((w) => w >= 44),
        `the tab bar fits five tabs, Discover included, each at least 44 px wide (${tabWidths.map(Math.round).join(", ")})`,
      );
      await shot(page, "discover-mobile");
      check(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        "Discover fits a phone's width: long names truncate, nothing scrolls sideways",
      );
      check(
        (await page.getByTestId("top-movers").locator("ul").evaluate((row) => row.scrollWidth > row.clientWidth)) &&
          (await page.getByTestId("collections").locator("ul").evaluate((row) => row.scrollWidth > row.clientWidth + 200)),
        "on a phone the top movers and the collections are rows that swipe sideways, inside the page",
      );
      await openTab(page, "Vault");
      await page.getByTestId("vault-total").filter({ hasText: "26,234" }).waitFor();
      await page.getByTestId("portfolio-line").waitFor();
      await shot(page, "vault-mobile");
      await openTab(page, "Account");
      await page.getByTestId("cash-yield-counter").waitFor();
      await page.getByTestId("card-face").waitFor();
      check(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        "on a phone the Account tab fits the width: the yield, the card and the account stack",
      );
      await shot(page, "account-mobile");
      // The Solvency Explorer on a fresh snapshot that holds this client's
      // positions: each one is a leaf in its asset's tree.
      const positionsCut = await freshCut(`e2e-${runId}-por2@example.mx`, password);
      const ownProof = await (
        await fetch(`${API}/api/v1/reserva/prueba`, {
          headers: { authorization: `Bearer ${await apiLogin(email, password)}` },
        })
      ).json();
      const held = ownProof.posiciones.map((x) => x.hoja.ticker);
      check(
        ownProof.corte.id === positionsCut.id && held.length > 0 && positionsCut.activos.length >= held.length,
        `the fresh snapshot has a tree per asset and this client's positions (${held.join(", ")})`,
      );
      await openTab(page, "Reserve");
      await page.getByTestId("position-row").first().waitFor();
      const listed = await page
        .getByTestId("position-row")
        .evaluateAll((rows) => rows.filter((r) => r.dataset.status === "included").map((r) => r.dataset.ticker));
      check(
        held.every((t) => listed.includes(t)),
        `the leaf auditor lists each position in the snapshot (${listed.join(", ")})`,
      );
      await page.getByTestId("verify-inclusion").click();
      await page.getByTestId("proof-verdict").and(page.locator("[data-status=verified]")).waitFor({ timeout: 15_000 });
      const tabs = page.getByTestId("trace-tab");
      check((await tabs.count()) === held.length + 1, "one proof trace per tree: the cash and each asset");
      for (const ticker of held) {
        await tabs.and(page.locator(`[data-key="${ticker}"]`)).click();
        await page.getByTestId("match-seal").and(page.locator("[data-status=match]")).waitFor();
        check(
          (await page.getByTestId("computed-root").innerText()).trim() ===
            positionsCut.activos.find((a) => a.ticker === ticker)?.merkle_root,
          `the ${ticker} position hashes up to the published ${ticker} root`,
        );
      }
      check(
        (await page.getByTestId("position-row").evaluateAll((rows) => rows.map((r) => r.innerText).join(" "))).includes(
          "Verified",
        ),
        "the verified positions are marked in the leaf auditor",
      );
      check(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        "on a phone the Solvency Explorer fits the width",
      );
      await page.waitForTimeout(500);
      await shot(page, "reserve-mobile");
      check(true, "the dashboard works on a phone-sized screen");

      // On the phone: the holding's name opens its page, and "Max" in the
      // bottom sheet sells the whole position (REST for the vault blocked again).
      await page.route("**/api/v1/vault", (route) => route.abort());
      await openTab(page, "Vault");
      const goldLink = page.getByTestId("asset-row-link").filter({ hasText: "Tokenized gold" });
      await goldLink.waitFor();
      check(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        "the Vault with a holding fits a phone's width",
      );
      await goldLink.click();
      await page.waitForURL("**/dashboard/asset/AUG1");
      await detail.getByText("1.5504 AUG1").waitFor();
      await shot(page, "asset-mobile");
      await page.getByTestId("chart-mode").getByRole("button", { name: "Candles" }).click();
      await page.getByTestId("pro-chart").and(page.locator("[data-mode=candles]")).waitFor();
      check(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        "on a phone the chart fits the width, as a line and as candles",
      );
      await shot(page, "asset-candles-mobile");
      await page.getByTestId("chart-mode").getByRole("button", { name: "Line" }).click();
      await page.getByRole("button", { name: "Sell", exact: true }).click();
      await checkout.getByTestId("order-type").getByText("Limit", { exact: true }).click();
      await checkout.getByTestId("order-price").waitFor();
      await shot(page, "checkout-limit-mobile");
      check(
        await checkout.evaluate((sheet) => sheet.scrollWidth <= sheet.clientWidth),
        "on a phone the order types and the price fit the sheet's width",
      );
      await checkout.getByTestId("order-type").getByText("Market", { exact: true }).click();
      await checkout.getByRole("button", { name: "Max" }).click();
      check(
        (await checkout.getByTestId("order-amount").inputValue()) === "2,499.86" &&
          (await checkout.getByTestId("order-summary").innerText()).includes("1.5504 AUG1"),
        "Max offers the position's whole value (2,499.86) and estimates all of it (1.5504 AUG1)",
      );
      await shot(page, "checkout-mobile");
      await checkout.getByTestId("order-confirm").click();
      await receipt.waitFor();
      check((await receipt.innerText()).includes("Sold 1.5504 AUG1 at $1,612.40."), "the sale fills the whole position");
      await checkout.getByRole("button", { name: "Done" }).click();
      await checkout.waitFor({ state: "detached" });
      await detail.getByText("Your position").waitFor({ state: "detached", timeout: 5_000 });
      check(await page.getByRole("button", { name: "Sell", exact: true }).isDisabled(), "with the position sold, Sell is disabled");
      await page.getByTestId("asset-back").click();
      await page.waitForURL("**/dashboard");
      await page.getByTestId("asset-list-empty").waitFor();
      await page.getByTestId("vault-total").filter({ hasText: "26,234" }).waitFor();
      await page.unroute("**/api/v1/vault");
      check(vaultSockets.length === 2, "the sale empties the asset list live; the whole loop ran on one socket");

      // The ledger lists each order's cash leg: the first pass's STSLA buy and
      // sale, its two savings plans' first purchases and their sales, then this
      // pass's two gold buys and the sale.
      await openTab(page, "Account");
      await page.getByText("Market sale", { exact: true }).nth(3).waitFor();
      check(
        (await page.getByText("Market buy", { exact: true }).count()) === 5 &&
          (await page.getByText("Market sale", { exact: true }).count()) === 4,
        "the ledger lists five market buys (two of them a savings plan's) and four market sales",
      );
      // A limit buy from another device: it waits below the price with its
      // cash held, and cancelling it releases the cash. Both legs reach this
      // tab live (after the holds of the other pass's orders, if it ran first).
      const holdRows = page.getByText("Held for order", { exact: true });
      const releaseRows = page.getByText("Order hold released", { exact: true });
      const [holdsBefore, releasesBefore] = [await holdRows.count(), await releaseRows.count()];
      const limitBuy = await apiOrder(otherDevice, {
        ticker: "AUG1",
        side: "buy",
        amount: "100.00",
        order_type: "limit",
        limit_price: "1500.0000",
      });
      check(
        limitBuy.status === 201 && limitBuy.receipt.status === "pending" && limitBuy.receipt.held_cash === "100.00000000",
        `a limit buy of 100.00 at 1,500 waits with its cash held (${limitBuy.status} ${limitBuy.receipt.status})`,
      );
      await page
        .getByTestId("vault-held-for-orders")
        .filter({ hasText: "100.00 MXN held for open orders" })
        .waitFor({ timeout: 5_000 });
      await holdRows.nth(holdsBefore).waitFor({ timeout: 5_000 });
      check(true, "the Account tab shows the cash held for the order, and the hold in the ledger");

      // Withdraw, while the limit buy holds 100.00: the off-ramp, by SPEI to the linked account in
      // the client's own name (in demo mode, BBVA México ending in 1234). Only the available cash
      // can leave (never what open orders hold), and it shows as processing. On a desktop
      // first: this pass is on a phone since its tab bar checks, and goes back to it after.
      await page.setViewportSize({ width: 1440, height: 1000 });
      const withdrawDrawer = page.getByTestId("withdrawal-drawer");
      const withdrawOpen = page.getByTestId("withdraw-open");
      // The holder is the verified identity's name (kyc.clientes), in capitals as a bank keeps it.
      const holder = [identity.profile.nombres, identity.profile.apellido_paterno, identity.profile.apellido_materno]
        .filter(Boolean)
        .join(" ")
        .toUpperCase();
      await withdrawOpen.click();
      const accountOption = withdrawDrawer.getByTestId("withdraw-account");
      await accountOption.waitFor();
      check(
        (await accountOption.count()) === 1 &&
          (await accountOption.getAttribute("data-checked")) === "true" &&
          (await accountOption.innerText()).includes("BBVA México") &&
          /in your name/i.test(await accountOption.innerText()) &&
          (await withdrawDrawer.getByTestId("withdraw-account-clabe").innerText()) === "•••• 1234" &&
          (await withdrawDrawer.getByTestId("withdraw-account-holder").innerText()) === holder,
        `Withdraw starts at the linked account, already selected: BBVA México •••• 1234, in the client's verified name (${holder})`,
      );
      await page.mouse.move(0, 0);
      await shot(page, "withdraw-account-desktop", { fullPage: false });
      const withdrawContinue = withdrawDrawer.getByTestId("withdraw-continue");
      await withdrawContinue.click();
      const withdrawAmount = withdrawDrawer.getByTestId("withdraw-amount");
      await withdrawAmount.waitFor();
      check(
        (await withdrawDrawer.getByTestId("withdraw-available").innerText()) === "$16,134.56" &&
          (await withdrawDrawer.getByTestId("withdraw-held").innerText()).startsWith("$100.00 held by your open orders") &&
          !(await withdrawContinue.isEnabled()) &&
          (await withdrawAmount.evaluate((el) => el === document.activeElement)),
        "the amount step offers only the available 16,134.56 (the 100.00 the limit buy holds stays), takes the keyboard's focus, and needs an amount",
      );
      await withdrawDrawer.getByTestId("withdraw-max").click();
      check((await withdrawAmount.inputValue()) === "16,134.56", "Max enters all the available cash, and nothing held");
      await withdrawAmount.fill("16134.57");
      check(
        (await withdrawDrawer.getByTestId("withdraw-amount-hint").innerText()).includes("More than your available cash") &&
          !(await withdrawContinue.isEnabled()),
        "one cent more than the available cash cannot go on",
      );
      await withdrawAmount.fill("");
      for (const key of ["1", "2", "5", "0", "Decimal point", "5"]) {
        await withdrawDrawer.getByRole("button", { name: key, exact: true }).click();
      }
      check((await withdrawAmount.inputValue()) === "1,250.5", "the numpad types 1,250.5");
      await withdrawContinue.click();
      const review = withdrawDrawer.getByTestId("withdraw-step-review");
      await review.waitFor();
      const withdrawSummary = await withdrawDrawer.getByTestId("withdraw-summary").innerText();
      check(
        (await withdrawDrawer.getByTestId("withdraw-review-amount").innerText()).replace(/\s+/g, "") === "$1,250.50MXN" &&
          withdrawSummary.includes("BBVA México •••• 1234") &&
          withdrawSummary.includes(holder) &&
          withdrawSummary.includes("$0.00") &&
          (await withdrawDrawer.getByTestId("withdraw-after").innerText()).includes("$14,884.06"),
        "Review: 1,250.50 to BBVA México •••• 1234 in the client's name, no fee, 14,884.06 left",
      );
      const confirmWithdrawal = withdrawDrawer.getByTestId("withdraw-confirm");
      await page.mouse.move(0, 0);
      check(
        (await confirmWithdrawal.evaluate((el) => getComputedStyle(el).backgroundColor)) === "rgb(234, 179, 8)" &&
          (await confirmWithdrawal.evaluate((el) => getComputedStyle(el).color)) === "rgb(0, 0, 0)" &&
          /confirm withdrawal/i.test(await confirmWithdrawal.innerText()),
        "Confirm withdrawal is the stark action: black on Synthetic Capital Yellow",
      );
      await shot(page, "withdraw-review-desktop", { fullPage: false });
      // Money leaving needs a fresh check (step-up, roadmap item 22): this session has none, so the
      // server sends nothing (403) and "Confirm it's you" opens above the drawer, naming the withdrawal.
      expectFailure("/api/v1/wealth/withdraw");
      await confirmWithdrawal.click();
      const stepUp = page.getByTestId("step-up-modal");
      await stepUp.and(page.locator("[data-method=password]")).waitFor();
      const stepUpPassword = stepUp.getByTestId("step-up-password");
      await stepUpPassword.waitFor();
      const waiting = await stepUp.getByTestId("step-up-action").innerText();
      check(
        waiting.includes("Withdraw $1,250.50") &&
          waiting.includes("To BBVA México •••• 1234, by SPEI") &&
          (await withdrawDrawer.isVisible()) &&
          (await stepUpPassword.evaluate((el) => el === document.activeElement)) &&
          !(await stepUp.getByTestId("step-up-submit").isEnabled()),
        "Confirm withdrawal first asks to confirm it's you: the withdrawal waits behind, named, and the password field has the focus",
      );
      check(
        (await settledStyle(stepUp, "borderTopColor")) === "rgb(234, 179, 8)" &&
          (await stepUp.evaluate((el) => getComputedStyle(el).borderRadius)) === "0px" &&
          (await settledStyle(stepUp.getByTestId("step-up-submit"), "backgroundColor")) === "rgb(234, 179, 8)",
        "the check is square, under a gold rule, with one gold action",
      );
      await shot(page, "step-up-withdraw-desktop", { fullPage: false });
      // Cancel: nothing is sent, the drawer says so, and Confirm asks again (the same withdrawal, the same key).
      await stepUp.getByTestId("step-up-cancel").click();
      await stepUp.waitFor({ state: "detached" });
      await withdrawDrawer.getByRole("alert").filter({ hasText: "Not confirmed: nothing was sent." }).waitFor();
      check(
        (await withdrawDrawer.getByTestId("withdraw-step-review").isVisible()) && (await confirmWithdrawal.isEnabled()),
        "Cancel closes the check: nothing was sent, the drawer says so and keeps the review to confirm again",
      );
      expectFailure("/api/v1/wealth/withdraw");
      await confirmWithdrawal.click();
      await stepUpPassword.waitFor();
      // A wrong password: refused and counted toward the sign-in lockout, and the screen says how many tries are left.
      expectFailure("/api/v1/auth/step-up");
      await stepUpPassword.fill("not-my-password-1926");
      await stepUp.getByTestId("step-up-submit").click();
      await stepUp.getByTestId("step-up-error").waitFor();
      check(
        (await stepUp.getByTestId("step-up-error").innerText()).includes("That password is not correct.") &&
          (await stepUp.getByTestId("step-up-attempts").innerText()) ===
            "4 attempts left before your account locks for 15 minutes." &&
          (await stepUpPassword.inputValue()) === "" &&
          (await stepUpPassword.evaluate((el) => el === document.activeElement)),
        "a wrong password is refused: 4 tries left before the lockout, the field cleared and focused again",
      );
      await shot(page, "step-up-refused-desktop", { fullPage: false });
      await stepUpPassword.fill(password);
      await stepUp.getByTestId("step-up-submit").click();
      await stepUp.waitFor({ state: "detached" });
      check(true, "the right password closes the check, and the same withdrawal goes on by itself");
      const withdrawReceipt = withdrawDrawer.getByTestId("withdraw-receipt");
      await withdrawReceipt.waitFor();
      const trackingKey = await withdrawDrawer.getByTestId("withdraw-tracking-key").locator("dd").innerText();
      check(
        (await withdrawReceipt.getAttribute("data-status")) === "processing" &&
          (await withdrawReceipt.innerText()).includes("$1,250.50 is on its way to BBVA México •••• 1234.") &&
          /^[A-Z0-9]{10,30}$/.test(trackingKey),
        `the receipt says processing, with the SPEI tracking key (${trackingKey})`,
      );
      // The vault's socket brings the new balance: the amount left the available cash at once.
      await page.getByTestId("vault-available").filter({ hasText: "14,884.06" }).waitFor();
      check(true, "the withdrawal leaves the available cash at once (14,884.06), pushed by the vault's socket");
      await shot(page, "withdraw-receipt-desktop", { fullPage: false });
      await withdrawDrawer.getByTestId("withdraw-done").click();
      await withdrawDrawer.waitFor({ state: "detached" });
      check(
        await withdrawOpen.evaluate((el) => el === document.activeElement),
        "Done closes the drawer and hands the focus back to Withdraw",
      );
      await page.getByRole("cell", { name: "Withdrawal" }).first().waitFor();
      check(true, "the ledger lists the withdrawal");

      // On a phone the drawer is a bottom sheet with a large numpad, as wide as the screen.
      await page.setViewportSize({ width: 390, height: 844 });
      await withdrawOpen.scrollIntoViewIfNeeded();
      await withdrawOpen.click();
      await accountOption.waitFor();
      await withdrawContinue.click();
      await withdrawAmount.waitFor();
      const sheet = await withdrawDrawer.boundingBox();
      const key = await withdrawDrawer.getByRole("button", { name: "5", exact: true }).boundingBox();
      check(
        sheet !== null &&
          Math.round(sheet.width) === 390 &&
          Math.abs(sheet.y + sheet.height - 844) <= 1 &&
          key !== null &&
          key.height >= 56 &&
          (await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)),
        "on a phone Withdraw opens as a bottom sheet across the width, with numpad keys 56 px tall",
      );
      await shot(page, "withdraw-amount-mobile", { fullPage: false });
      await page.keyboard.press("Escape");
      await withdrawDrawer.waitFor({ state: "detached" });
      const cancelled = await fetch(`${API}/api/v1/market/orders/${limitBuy.receipt.order_id}/cancel`, {
        method: "POST",
        headers: { authorization: `Bearer ${otherDevice}` },
      });
      const cancelledOrder = await cancelled.json();
      check(
        cancelled.status === 200 && cancelledOrder.status === "cancelled" && cancelledOrder.held_cash === "0.00000000",
        "cancelling the limit buy releases its cash",
      );
      await releaseRows.nth(releasesBefore).waitFor({ timeout: 5_000 });
      await page.getByTestId("vault-held-for-orders").waitFor({ state: "detached" });
      check(true, "the release reaches the ledger and nothing stays held");
      // A new Proof of Reserve cut still rebuilds every balance from the
      // ledger, the orders' legs included: a new account's first proof makes
      // the server cut again, over every account.
      const newcomer = `e2e-${runId}-cut@example.mx`;
      check((await apiRegister(newcomer, password)) === 201, "a new client registers through the API");
      const cutAfterTrading = await fetch(`${API}/api/v1/reserva/prueba`, {
        headers: { authorization: `Bearer ${await apiLogin(newcomer, password)}` },
      });
      check(
        cutAfterTrading.status === 200,
        `after trading, a new reserve cut covers every account (proof answered ${cutAfterTrading.status})`,
      );

      // The server ends the session (4001: expired or revoked): back to sign-in, no reconnection.
      await vaultSockets.at(-1).close({ code: 4001, reason: "sesión revocada" });
      await page.waitForURL("**/auth/login?expired=1");
      await page.waitForTimeout(1500);
      check(vaultSockets.length === 2, "a session the server ends (4001) signs out instead of reconnecting");
      await context.close();
      continue;
    }

    // Landing page with live reserve.
    await page.goto(BASE);
    await page.getByText("Reserve ratio").waitFor();
    await shot(page, "landing");
    check(await page.getByText("100%").first().isVisible(), "landing shows the live 100% reserve ratio");

    // Protected route redirects to login.
    await page.goto(`${BASE}/dashboard`);
    await page.waitForURL("**/auth/login");
    check(true, "dashboard redirects unauthenticated visitors to login");

    // Registration: every new client starts with a PENDIENTE KYC file.
    await page.goto(`${BASE}/auth/register`);
    await page.fill("#nombre", "Murray");
    await page.fill("#paterno", "Rothbard");
    await page.fill("#email", email);
    await page.fill("#password", password);
    await page.fill("#confirm", password);
    await page.click("button[type=submit]");
    await page.waitForURL("**/dashboard/verify");
    await page.getByTestId("onboarding-step-1").waitFor();
    check(true, "sign-up lands on the verification wizard (/dashboard/verify, step 1)");
    // The wizard lives in the dashboard: its header, not the banner that leads to it.
    await page.getByTestId("kyc-header-badge").and(page.locator("[data-status=PENDIENTE]")).waitFor();
    check(
      (await page.getByTestId("kyc-banner").count()) === 0,
      "the wizard does not repeat the KYC banner that leads to it",
    );
    // Old links (e-mails, bookmarks) still reach it.
    await page.goto(`${BASE}/auth/onboarding`);
    await page.waitForURL("**/dashboard/verify");
    await page.getByTestId("onboarding-step-1").waitFor();
    check(true, "the old /auth/onboarding address redirects to /dashboard/verify");

    // The dashboard is reachable but money movements are locked.
    await page.goto(`${BASE}/dashboard`);
    const banner = page.getByTestId("kyc-banner");
    await banner.waitFor();
    check((await banner.getAttribute("data-status")) === "PENDIENTE", "dashboard shows the KYC banner (pending)");
    const headerBadge = page.getByTestId("kyc-header-badge");
    await headerBadge.waitFor();
    check(
      (await headerBadge.getAttribute("data-status")) === "PENDIENTE" && (await headerBadge.innerText()).includes("Pending"),
      "header badge reads Pending",
    );
    // The vault stream is live for a brand-new account: its snapshot carries version 0.
    await streamStatus(page, "live");
    check(true, "the vault stream is live for a new account (snapshot at version 0)");
    // The Vault home offers no CLABE to deposit to: the bank would return the transfer.
    check(
      (await mainNav(page).getByRole("link", { name: "Vault", exact: true }).getAttribute("aria-current")) === "page",
      "the dashboard opens on the Vault tab",
    );
    await page.getByTestId("vault-deposit-locked").waitFor();
    check((await page.getByTestId("spei-copy").count()) === 0, "the vault offers no CLABE to copy until verified");
    await shot(page, "dashboard-kyc-pending");

    await openTab(page, "Account");
    await page.getByTestId("vault-available").filter({ hasText: "0.00" }).waitFor();
    check(await page.getByText("100% Fully Backed in Custody").isVisible(), "vault shows the 100% backed badge");
    check(
      (await page.getByTestId("deposit-locked").getAttribute("aria-disabled")) === "true",
      "deposit control is disabled until verified",
    );
    // Playwright treats aria-disabled as disabled, so force the click to prove it does nothing.
    await page.getByTestId("deposit-locked").click({ force: true });
    check((await page.locator("#demo-amount").count()) === 0, "clicking the locked deposit control opens no deposit form");
    await openTab(page, "Invest");
    check(await page.locator("#bridge-amount").isDisabled(), "investment amount is disabled until verified");
    check(
      await page.getByRole("button", { name: /Lock funds/ }).isDisabled(),
      "investment submit is disabled until verified",
    );

    // ---------------------------------------------------------------------
    // Onboarding step 1: personal data (a unique, valid identity).
    // ---------------------------------------------------------------------
    await page.getByTestId("kyc-banner-cta").click();
    await page.waitForURL("**/dashboard/verify");
    const step1 = page.getByTestId("onboarding-step-1");
    await step1.waitFor();
    check(true, "Complete verification opens the wizard");
    // A gold progress bar of four square segments; the first is current.
    const progress = await page.getByTestId("verify-progress").evaluate((nav) =>
      [...nav.querySelectorAll("li")].map((li) => {
        const bar = getComputedStyle(li.querySelector("span[aria-hidden]"));
        return { current: li.getAttribute("aria-current"), color: bar.backgroundColor, radius: bar.borderRadius };
      }),
    );
    check(
      progress.length === 4 &&
        progress[0].current === "step" &&
        progress[0].color === "rgb(234, 179, 8)" &&
        progress.slice(1).every((s) => s.current === null && s.color !== "rgb(234, 179, 8)") &&
        progress.every((s) => s.radius === "0px"),
      "a four-step progress bar fills step 1 in gold, with square segments",
    );
    const p = identity.profile;
    const d = p.domicilio;
    await page.fill("#nombres", p.nombres);
    await page.fill("#apellido-paterno", p.apellido_paterno);
    await page.fill("#apellido-materno", p.apellido_materno ?? "");
    await page.fill("#curp", p.curp);
    await page.fill("#rfc", p.rfc);
    await page.fill("#calle", d.calle);
    await page.fill("#numero-exterior", d.numero_exterior);
    await page.fill("#colonia", d.colonia);
    await page.fill("#municipio", d.municipio);
    await page.fill("#estado", d.estado);
    await page.fill("#codigo-postal", d.codigo_postal);
    await step1.getByText("Check digit OK").waitFor();
    check(
      (await step1.locator("[aria-invalid=true]").count()) === 0,
      `step 1 accepts the identity ${p.curp} / ${p.rfc} with no field errors`,
    );
    const born = new Intl.DateTimeFormat("en-US", {
      year: "numeric",
      month: "long",
      day: "numeric",
      timeZone: "UTC",
    }).format(new Date(`${identity.fechaNacimiento}T00:00:00Z`));
    check(
      (await step1.getByTestId("identity-birth-date").innerText()) === born &&
        (await step1.getByTestId("identity-nationality").innerText()) === "Mexican",
      `step 1 reads the date of birth (${born}) and the nationality from the CURP`,
    );
    await shot(page, "onboarding-1-personal");
    await page.getByRole("button", { name: "Save and continue" }).click();

    // ---------------------------------------------------------------------
    // Step 2: both sides of the INE (two different JPEGs) and its details.
    // ---------------------------------------------------------------------
    const step2 = page.getByTestId("onboarding-step-2");
    await step2.waitFor();
    check(true, "step 1 saves without warnings and advances to the INE step");
    check(
      (await step2.locator("input[name=documento][value=INE]").isChecked()) &&
        (await step2.locator("input[name=documento][value=PASAPORTE]").isDisabled()) &&
        (await step2.getByTestId("passport-note").isVisible()),
      "the INE is the document; the passport is shown as not yet available",
    );
    // Camera privacy: hiding the page turns the INE camera off, and closing it hands focus back.
    await step2.locator("#ine-frente-camera").click();
    const ineVideo = step2.locator("video[aria-label='Camera preview for the INE front']");
    await ineVideo.waitFor();
    await page.waitForFunction(
      () => {
        const v = document.querySelector("video[aria-label='Camera preview for the INE front']");
        return v instanceof HTMLVideoElement && v.srcObject instanceof MediaStream && v.videoWidth > 0;
      },
      undefined,
      { timeout: 15_000 },
    );
    const liveTracks = await page.evaluate(() => {
      const v = document.querySelector("video[aria-label='Camera preview for the INE front']");
      // Keep a handle on the stream to check its tracks after the page hides.
      window.__ineStream = v.srcObject;
      return window.__ineStream.getTracks().filter((t) => t.readyState === "live").length;
    });
    check(liveTracks > 0, "the INE camera preview is live");
    await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "hidden" });
      Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await step2.getByText("The camera was turned off").waitFor();
    const afterHide = await page.evaluate(() => {
      const v = document.querySelector("video[aria-label='Camera preview for the INE front']");
      const stillLive = window.__ineStream.getTracks().filter((t) => t.readyState === "live").length;
      delete window.__ineStream;
      delete document.visibilityState;
      delete document.hidden;
      return { srcObject: v?.srcObject ?? null, stillLive };
    });
    check(afterHide.srcObject === null && afterHide.stillLive === 0, "hiding the page stops the INE camera (no live tracks)");
    await step2.getByRole("button", { name: "Cancel" }).click();
    await page.waitForFunction(() => document.activeElement?.id === "ine-frente-camera");
    check(true, "closing the INE camera returns keyboard focus to the Use camera button");

    const front = await renderJpeg(page, "INE FRENTE", 200);
    const back = await renderJpeg(page, "INE REVERSO", 20);
    check(!front.equals(back) && front.length > 1024 && back.length > 1024, "generated two different JPEGs > 1 KB");
    await page.setInputFiles("#ine-frente-file", { name: "ine-frente.jpg", mimeType: "image/jpeg", buffer: front });
    // The back is dragged onto its card: the same path as an upload.
    const dropped = await page.evaluateHandle((b64) => {
      const bytes = Uint8Array.from(atob(b64), (ch) => ch.charCodeAt(0));
      const transfer = new DataTransfer();
      transfer.items.add(new File([bytes], "ine-reverso.jpg", { type: "image/jpeg" }));
      return transfer;
    }, back.toString("base64"));
    await step2.getByTestId("ine-reverso-dropzone").dispatchEvent("drop", { dataTransfer: dropped });
    await step2.getByAltText("Captured front of your INE").waitFor();
    await step2.getByAltText("Captured back of your INE").waitFor();
    check(
      (await step2.getByTestId("ine-frente-dropzone").getAttribute("data-captured")) === "true" &&
        (await step2.getByTestId("ine-reverso-dropzone").getAttribute("data-captured")) === "true",
      "the front is uploaded and the back dropped onto its card, both previewed at once",
    );
    const thumbs = await step2.locator("img[alt^='Captured']").evaluateAll((imgs) => imgs.map((i) => i.src));
    check(
      thumbs.length === 2 && thumbs.every((s) => s.startsWith("data:image/jpeg;base64,")) && thumbs[0] !== thumbs[1],
      "both INE photos are re-encoded in the browser as distinct JPEGs",
    );
    const c = identity.credential;
    await page.fill("#clave-elector", c.clave_elector);
    await page.fill("#cic", c.cic);
    await page.fill("#identificador", c.identificador_ciudadano);
    await page.fill("#emision", String(c.emision));
    await page.fill("#vigencia", String(c.vigencia));
    await step2.getByText("Birth date and sex match your CURP").waitFor();
    check(
      (await step2.locator("[aria-invalid=true]").count()) === 0 && (await step2.getByText("Please double-check").count()) === 0,
      `INE details (voter key ${c.clave_elector}, ${c.emision}-${c.vigencia}) match the CURP with no warnings`,
    );
    await shot(page, "onboarding-2-ine");
    await step2.getByRole("button", { name: "Continue" }).click();

    // ---------------------------------------------------------------------
    // Step 3: biometric consent, the liveness check with the fake camera, and
    // the declarations. Nothing is submitted yet: the frames wait in the page.
    // ---------------------------------------------------------------------
    const step3 = page.getByTestId("onboarding-step-3");
    await step3.waitFor();
    const start = step3.getByRole("button", { name: "Start liveness check" });
    check(await start.isDisabled(), "liveness check cannot start without biometric consent");
    await page.check("#consentimiento");
    check(await start.isEnabled(), "biometric consent enables the liveness check");
    await shot(page, "onboarding-3-consent");
    let verifySent = false;
    page.on("request", (r) => {
      if (r.url().includes("/kyc/verificar") && r.method() === "POST") verifySent = true;
    });
    await start.click();
    // The countdown runs while the camera preview is live; then a random challenge.
    await step3.locator("video").and(page.locator("[aria-label='Camera preview (mirrored)']")).waitFor();
    await page.waitForFunction(
      () => {
        const v = document.querySelector("[data-testid=onboarding-step-3] video");
        return v instanceof HTMLVideoElement && v.videoWidth > 0 && !v.paused;
      },
      undefined,
      { timeout: 15_000 },
    );
    await shot(page, "onboarding-3-liveness-camera");
    await step3.getByTestId("liveness-captured").waitFor({ timeout: 45_000 });
    check(!verifySent, "the liveness check captures the frames without submitting them (that happens at the review)");
    // Camera released and nothing biometric persisted in the browser.
    const privacy = await page.evaluate(() => {
      const live = [...document.querySelectorAll("video")].some(
        (v) => v.srcObject instanceof MediaStream && v.srcObject.getTracks().some((t) => t.readyState === "live"),
      );
      const dump = (store) => Object.keys(store).map((k) => `${k}=${store.getItem(k)}`).join("\n");
      return {
        live,
        sessionKeys: Object.keys(sessionStorage),
        localKeys: Object.keys(localStorage),
        images: /data:image/.test(dump(sessionStorage) + dump(localStorage)),
      };
    });
    check(!privacy.live, "the camera is released once the frames are captured (no live track)");
    check(
      privacy.sessionKeys.length === 1 &&
        privacy.sessionKeys[0] === "synthetic-capital-session" &&
        privacy.localKeys.length === 0 &&
        !privacy.images,
      `browser storage holds only the session and no image data (session: ${privacy.sessionKeys.join(",")}; local: ${privacy.localKeys.join(",")})`,
    );
    // The three declarations have no defaults: Continue says what is missing.
    await step3.getByTestId("step3-continue").click();
    await step3.locator("#ocupacion[aria-invalid=true]").waitFor();
    check(
      (await step3.locator("#origen-recursos[aria-invalid=true]").count()) === 1 &&
        (await step3.locator("#declara-pep-error").count()) === 1 &&
        (await page.evaluate(() => document.activeElement?.id)) === "ocupacion",
      "occupation, source of funds and PEP must be answered: Continue flags all three and focuses the first",
    );
    await page.fill("#ocupacion", "Economist");
    await page.selectOption("#origen-recursos", "SALARIO");
    await page.check("#declara-pep-si");
    await step3.getByTestId("pep-note").waitFor();
    check(true, "a yes to PEP explains that a person on the team approves the file");
    await page.check("#declara-pep-no");
    check(
      (await step3.getByTestId("pep-note").count()) === 0 && (await step3.locator("[aria-invalid=true]").count()) === 0,
      "the declarations are complete (occupation, salary, not a PEP)",
    );
    await shot(page, "onboarding-3-declarations");
    await step3.getByTestId("step3-continue").click();

    // ---------------------------------------------------------------------
    // Step 4: review, the three agreements, and Submit for Verification.
    // ---------------------------------------------------------------------
    const step4 = page.getByTestId("onboarding-step-4");
    await step4.waitFor();
    const sections = step4.getByTestId("review-section");
    check(
      (await sections.count()) === 3 &&
        (await sections.nth(0).innerText()).includes(p.curp) &&
        (await sections.nth(1).innerText()).includes(c.clave_elector) &&
        (await sections.nth(2).getByText("Economist").isVisible()) &&
        (await sections.nth(2).getByText("Salary or wages").isVisible()),
      "the review shows identity, ID and liveness/funds as entered, each with Edit",
    );
    const submit = step4.getByTestId("submit-verification");
    check(await submit.isDisabled(), "Submit for Verification stays disabled until the three agreements are accepted");
    check(
      (await step4.getByTestId("agreement").count()) === 3 &&
        (await step4.getByTestId("agreements-version").innerText()).includes("2026-11"),
      "the three agreements in force (version 2026-11) are listed, each to accept on its own",
    );
    await page.check("#acuerdo-contrato-cuenta");
    await page.check("#acuerdo-aviso-privacidad");
    check(await submit.isDisabled(), "two of three agreements are not enough");
    await page.check("#acuerdo-declaracion-veracidad");
    check(await submit.isEnabled(), "accepting the three agreements enables Submit for Verification");
    await page.mouse.move(0, 0);
    const submitStyle = await submit.evaluate((el) => ({
      color: getComputedStyle(el).backgroundColor,
      radius: getComputedStyle(el).borderRadius,
    }));
    check(
      submitStyle.color === "rgb(234, 179, 8)" && submitStyle.radius === "0px",
      "Submit for Verification is a square, solid gold button",
    );
    await shot(page, "onboarding-4-review");
    const verify = page.waitForResponse((r) => r.url().includes("/kyc/verificar") && r.request().method() === "POST", {
      timeout: 45_000,
    });
    // The file the page reloads right after the verification (GET /kyc/expediente).
    const reloaded = page.waitForResponse(
      (r) => verifySent && r.url().includes("/kyc/expediente") && r.request().method() === "GET",
      { timeout: 60_000 },
    );
    // Gone if anything reloads the page from here on: the unlock must not need one.
    await page.evaluate(() => {
      window.__sameDocument = true;
    });
    await submit.click();
    const response = await verify;
    const sent = JSON.parse(response.request().postData() ?? "{}");
    const frames = sent.selfie?.frames ?? [];
    check(
      sent.consentimiento_biometrico === true && frames.length >= 3 && frames.length <= 8 && new Set(frames).size === frames.length,
      `POST /kyc/verificar carries consent and ${frames.length} distinct camera frames`,
    );
    check(
      sent.ine?.frente !== sent.ine?.reverso && !frames.includes(sent.ine?.frente) && !frames.includes(sent.ine?.reverso),
      "INE front, back and selfie frames are all different images",
    );
    check(
      sent.declaraciones?.ocupacion === "Economist" &&
        sent.declaraciones?.origen_recursos === "SALARIO" &&
        sent.declaraciones?.declara_pep === false,
      "POST /kyc/verificar carries the declarations (occupation, source of funds, not a PEP)",
    );
    check(
      sent.contratos?.version === "2026-11" &&
        [...(sent.contratos?.documentos ?? [])].sort().join() ===
          "AVISO_PRIVACIDAD,CONTRATO_CUENTA,DECLARACION_VERACIDAD",
      "POST /kyc/verificar carries the three agreements accepted at version 2026-11",
    );
    const verdict = await response.json();
    check(
      response.status() === 200 && verdict.resultado?.resultado === "APROBADO",
      `verification answered ${response.status()} ${
        verdict.resultado?.resultado ?? [verdict.error?.codigo, verdict.error?.mensaje].filter(Boolean).join(": ")
      }`.trim(),
    );
    const file = await (await reloaded).json();
    // Score or threshold as a decimal ("0.6512") inside any finding would leak them too.
    const findings = [
      ...(verdict.resultado.detalles ?? []),
      ...(file.ultimo_resultado?.detalles_fallo ?? []),
      ...(file.motivo_rechazo ? [{ mensaje: file.motivo_rechazo }] : []),
    ];
    check(
      verdict.resultado.score_similitud === null &&
        file.estado_kyc === "VERIFICADO" &&
        file.ultimo_resultado?.score_similitud === null &&
        file.ultimo_resultado?.umbral_aplicado === null &&
        findings.every((d) => !/\b0\.\d{2,}/.test(d.mensaje)),
      "the face-match score and threshold are not disclosed to the client (fields or findings)",
    );
    check(
      file.ocupacion === "Economist" &&
        file.origen_recursos === "SALARIO" &&
        file.declara_pep === false &&
        file.contratos_version === "2026-11" &&
        typeof file.contratos_aceptados_en === "string",
      `the file keeps the declarations and the agreements' version and time (${file.contratos_aceptados_en})`,
    );

    // ---------------------------------------------------------------------
    // Status: verified at once in demo mode, and unlocked in place.
    // ---------------------------------------------------------------------
    const status = page.getByTestId("onboarding-status");
    await status.waitFor();
    const statusBadge = status.getByTestId("kyc-status-badge");
    await statusBadge.and(page.locator("[data-status=VERIFICADO]")).waitFor();
    check(true, "status badge reads VERIFICADO");
    await status.getByText("Your identity is verified").waitFor();
    check(await status.getByText("Approved", { exact: true }).isVisible(), "last verification shows Approved");
    check(
      (await status.getByText(/^Face match/).count()) === 0,
      "status page shows no face-match score or row (the API withholds it)",
    );
    // The dashboard reloads its summary by itself: the header follows without a page reload.
    await page.getByTestId("kyc-header-badge").and(page.locator("[data-status=VERIFICADO]")).waitFor();
    check(
      await page.evaluate(() => window.__sameDocument === true),
      "the header reads Verified as soon as the file is approved, with no page reload",
    );
    check(
      (await status.getByTestId("verified-add-funds").getAttribute("href")) === "/dashboard/account",
      "the verified status offers to add funds (Account tab)",
    );
    await shot(page, "onboarding-5-status");
    await status.getByRole("button", { name: "Go to dashboard" }).click();
    await page.waitForURL("**/dashboard");

    // ---------------------------------------------------------------------
    // Dashboard, unlocked.
    // ---------------------------------------------------------------------
    await page.getByTestId("kyc-header-badge").and(page.locator("[data-status=VERIFICADO]")).waitFor();
    check((await page.getByTestId("kyc-banner").count()) === 0, "KYC banner is gone once verified");

    // The Vault offers the CLABE: one tap copies it, on a solid yellow button.
    // (The pointer moves away first: a hover would brighten the yellow.)
    await page.mouse.move(0, 0);
    const copy = page.getByTestId("spei-copy");
    await copy.waitFor();
    check(
      (await copy.evaluate((el) => getComputedStyle(el).backgroundColor)) === "rgb(234, 179, 8)",
      "Copy CLABE is solid Synthetic Capital Yellow",
    );
    await copy.click();
    await copy.filter({ hasText: "Copied!" }).waitFor();
    check(true, "Copy CLABE confirms the copy");
    check((await page.getByTestId("asset-list-empty").count()) === 1, "the asset list shows its empty state");
    check(
      (await page.getByTestId("asset-list-discover").getAttribute("href")) === "/dashboard/discover",
      "with nothing held, the asset list points to Discover",
    );

    await openTab(page, "Account");
    check((await page.getByTestId("deposit-locked").count()) === 0, "deposit control is unlocked");
    check(
      (await page.getByTestId("withdraw-locked").count()) === 0 &&
        (await page.getByTestId("withdraw-open").count()) === 1,
      "the Withdraw button is unlocked",
    );
    check(
      await page.evaluate(() => window.__sameDocument === true),
      "deposits and withdrawals unlocked in the same document the verification ran in (no reload)",
    );

    // Demo SPEI deposit.
    await page.getByRole("button", { name: /Add funds/ }).click();
    await page.fill("#demo-amount", "25,000");
    await page.getByRole("button", { name: "Deposit", exact: true }).click();
    await page.getByTestId("vault-available").filter({ hasText: "25,000.00" }).waitFor();
    check(true, "simulated deposit credits the vault");

    // IFPE -> SOFOM bridge.
    await openTab(page, "Invest");
    check(await page.locator("#bridge-amount").isEnabled(), "investment form is enabled once verified");
    check(
      await page.evaluate(() => window.__sameDocument === true),
      "the investment form unlocked with no page reload either",
    );
    await page.fill("#bridge-amount", "10000");
    await page.getByText("182d").click();
    await page.getByText("You receive").waitFor();
    check(await page.getByText("366.53").first().isVisible(), "bridge preview shows ACT/360 interest (366.53)");
    await page.getByLabel(/no early withdrawal/).check();
    await page.getByRole("button", { name: /Lock funds/ }).click();
    await page.getByText(/Contract #\d+ created/).waitFor();
    await page.getByTestId("sofom-locked").filter({ hasText: "10,000.00" }).waitFor();
    await openTab(page, "Account");
    await page.getByTestId("vault-available").filter({ hasText: "15,000.00" }).waitFor();
    check(true, "bridge moves 10,000 from the vault into a mutuum contract");

    // Sovereign Yield: a preview, ticking up live on the 15,000.00 available. What it shows
    // is the exact accrual (lib/wealth.ts) at the instant it is read, to within a second.
    const reading = async () => Number((await page.getByTestId("cash-yield-counter").innerText()).replace(/[^\d.]/g, ""));
    const expectedAt = (at) =>
      Number(formatTruncated(accrued("15000.00", SOVEREIGN_YIELD_PCT, at - monthWindow(at).start), 6).replace(/,/g, ""));
    const readFrom = Date.now();
    const firstReading = await reading();
    const readTo = Date.now();
    await page.waitForTimeout(1_200);
    const secondReading = await reading();
    check(
      /preview · 7\.25% a year/i.test(await page.getByTestId("cash-yield-preview").innerText()) &&
        (await page.getByTestId("cash-yield-available").innerText()) === "$15,000.00" &&
        (await page.getByTestId("cash-yield-per-day").innerText()) === "$3.02" &&
        firstReading >= expectedAt(readFrom - 1_000) &&
        firstReading <= expectedAt(readTo + 1_000) &&
        secondReading > firstReading,
      `Sovereign Yield, a preview at 7.25 %, ticks up live on the 15,000.00 available ($${firstReading} → $${secondReading}, $3.02 a day)`,
    );

    // The debit card, a preview: lock (kept across tabs), the PIN's place, settings, saveback.
    const cardFace = page.getByTestId("card-face");
    check(
      /^[A-Z][A-Z .'-]+$/.test(await page.getByTestId("card-holder").innerText()) &&
        (await page.getByTestId("card-number").innerText()).includes("•••• •••• •••• ••••"),
      "the card carries the holder's name in capitals, and no number until it is issued",
    );
    await page.getByTestId("card-lock").click();
    await cardFace.and(page.locator("[data-locked=true]")).waitFor();
    check(
      (await page.getByTestId("card-lock").getAttribute("aria-pressed")) === "true" &&
        (await page.getByTestId("card-status").innerText()).startsWith("Locked"),
      "Lock frosts the card over, keeps its button pressed and says payments are declined",
    );
    await openTab(page, "Vault");
    await openTab(page, "Account");
    await cardFace.and(page.locator("[data-locked=true]")).waitFor();
    check(true, "the card is still locked back from another tab");
    await page.getByTestId("card-lock").click();
    await cardFace.and(page.locator("[data-locked=false]")).waitFor();
    await page.getByTestId("card-pin").click();
    const pinPanel = page.getByTestId("card-pin-panel");
    await pinPanel.waitFor();
    check(
      (await page.getByTestId("card-pin").getAttribute("aria-expanded")) === "true" &&
        (await pinPanel.innerText()).includes("No PIN yet"),
      "Show PIN opens the PIN's place, which says the preview card has none yet",
    );
    await pinPanel.waitFor({ state: "detached", timeout: 12_000 });
    check(
      (await page.getByTestId("card-pin").getAttribute("aria-expanded")) === "false",
      "the PIN's place closes by itself after 10 seconds",
    );
    await page.getByTestId("card-settings").click();
    const abroad = page.getByTestId("card-use").and(page.locator("[data-use=abroad]"));
    const abroadBefore = await abroad.getAttribute("aria-checked");
    await abroad.click();
    check(
      (await page.getByTestId("card-use").count()) === 4 &&
        abroadBefore === "false" &&
        (await abroad.getAttribute("aria-checked")) === "true",
      "Settings lists four uses as switches, and Payments abroad turns on",
    );
    check(
      /^\$[\d,]+\.\d{2}$/.test(await page.getByTestId("saveback-earned").innerText()) &&
        (await page.getByTestId("saveback").innerText()).includes("of $300.00"),
      "the saveback tracker shows this month's simulated saveback toward its $300.00 cap",
    );
    await page.mouse.move(0, 0);
    await shot(page, "account-wealth-desktop");

    // Proof of Reserve: the Solvency Explorer, on a fresh snapshot that holds
    // the deposit and the transfer to fixed term (15,000 cash, no positions).
    const snapshot = await freshCut(`e2e-${runId}-por1@example.mx`, password);
    await openTab(page, "Reserve");
    await page.getByTestId("solvency-header").and(page.locator("[data-status=loaded]")).waitFor();
    check(
      (await page.getByTestId("reserve-root").innerText()).trim() === snapshot.merkle_root,
      "the solvency header shows the latest published reserve root",
    );
    check(
      (await page.getByTestId("reserve-ratio").innerText()).trim() === "100.00%",
      "the reserve ratio reads 100.00%",
    );
    const custody = await page.getByTestId("custody-total").getAttribute("data-amount");
    const owed = await page.getByTestId("liabilities-total").getAttribute("data-amount");
    check(
      custody === snapshot.total_reserva && owed === snapshot.total_pasivo && custody === owed,
      `custody assets equal client liabilities (${owed})`,
    );
    const headerText = await page.getByTestId("solvency-header").innerText();
    check(
      snapshot.secuencia_mercado !== null &&
        headerText.includes(`#${snapshot.secuencia_ledger}`) &&
        headerText.includes(`#${snapshot.secuencia_mercado}`),
      `the snapshot is anchored to the cash and market ledgers (#${snapshot.secuencia_ledger}, #${snapshot.secuencia_mercado})`,
    );
    // What Copy root puts on the clipboard is the root.
    await page.evaluate(() => {
      const write = navigator.clipboard.writeText.bind(navigator.clipboard);
      navigator.clipboard.writeText = (text) => {
        window.__copiedRoot = text;
        return write(text);
      };
    });
    await page.getByTestId("copy-root").click();
    await page.getByTestId("copy-root").filter({ hasText: "Copied" }).waitFor();
    check(
      (await page.evaluate(() => window.__copiedRoot)) === snapshot.merkle_root,
      "Copy root copies the published root",
    );
    // The leaf auditor: the leaf as published and what it commits to.
    const leafAuditor = page.getByTestId("leaf-auditor");
    await leafAuditor.getByTestId("leaf-hash").waitFor();
    const publishedLeaf = (await leafAuditor.getByTestId("leaf-hash").innerText()).trim();
    check(
      /^[0-9a-f]{64}$/.test(publishedLeaf) && /as published/i.test(await leafAuditor.getByTestId("leaf-hash-source").innerText()),
      "the leaf auditor shows the leaf hash as published",
    );
    check(
      (await leafAuditor.getByTestId("leaf-total").innerText()).includes("15,000.00") &&
        (await leafAuditor.getByTestId("balance-line").count()) === 1,
      "the leaf commits to 15,000.00, all of it available cash",
    );
    check(
      (await page.getByTestId("position-row").count()) === 0,
      "no asset positions yet: nothing else in the snapshot is this client's",
    );
    await page.getByTestId("trace-idle").waitFor();
    const verifyButton = page.getByTestId("verify-inclusion");
    await page.mouse.move(0, 0);
    const verifyStyle = await verifyButton.evaluate((el) => ({
      color: getComputedStyle(el).backgroundColor,
      radius: getComputedStyle(el).borderRadius,
    }));
    check(
      verifyStyle.color === "rgb(234, 179, 8)" && verifyStyle.radius === "0px",
      "Verify My Inclusion is a square, solid gold button",
    );
    await shot(page, "reserve-before");
    await verifyButton.click();
    const proof = page.getByTestId("proof-verdict");
    await proof.and(page.locator("[data-status=verified]")).waitFor({ timeout: 15_000 });
    check((await proof.innerText()).includes("Verified"), "browser verifies the Merkle proof against the published root");
    await page.getByTestId("match-seal").and(page.locator("[data-status=match]")).waitFor();
    check(
      (await page.getByTestId("computed-root").innerText()).trim() === snapshot.merkle_root &&
        (await page.getByTestId("published-root").innerText()).trim() === snapshot.merkle_root,
      "the trace ends on the published root, under the cryptographic match seal",
    );
    check(
      (await leafAuditor.getByTestId("leaf-hash").innerText()).trim() === publishedLeaf &&
        /recomputed in your browser/i.test(await leafAuditor.getByTestId("leaf-hash-source").innerText()),
      "the browser recomputes the same leaf hash",
    );
    const levels = await page.getByTestId("trace-level").count();
    check(
      levels >= 1 && levels <= Math.ceil(Math.log2(snapshot.num_hojas)),
      `the trace shows each level from the leaf to the root (${levels} for ${snapshot.num_hojas} leaves)`,
    );
    await page.getByText("Server check: agrees").waitFor();
    check(true, "server-side verification agrees");
    await shot(page, "reserve-verified");

    // Immutable ledger feed.
    await openTab(page, "Account");
    const rows = page.locator("tbody tr");
    await rows.nth(1).waitFor();
    check((await rows.count()) === 2, "ledger feed lists the deposit and the transfer to fixed term");
    check(await page.getByText("To fixed term").isVisible(), "ledger labels the bridge movement");
    await rows.first().click();
    await page.getByText("Previous hash (chain link)").waitFor();
    check(true, "ledger row expands to show the hash chain");
    await shot(page, "dashboard-account");

    // The Vault home after it all: 15,000 cash and 10,000 invested, and the cash line drawn in yellow.
    await openTab(page, "Vault");
    await page.getByRole("button", { name: "Refresh balances" }).click();
    await page.getByTestId("vault-total").filter({ hasText: "25,000" }).waitFor();
    const line = page.getByTestId("portfolio-line");
    await line.waitFor();
    check(
      (await line.evaluate((el) => getComputedStyle(el).stroke)) === "rgb(234, 179, 8)",
      "the balance line is drawn in Synthetic Capital Yellow",
    );
    check(
      (await page.getByTestId("portfolio-chart").innerText()).includes("15,000.00"),
      "the chart reads the current cash balance",
    );
    await shot(page, "vault-desktop-first-pass");

    // A dishonest server: a private root (differs from the public copy of the
    // cut) or a leaf for another account must both fail in the browser.
    await openTab(page, "Reserve");
    await page.getByTestId("verify-inclusion").click();
    await page.getByTestId("proof-verdict").waitFor({ timeout: 15_000 });
    const tamper = async (pattern, edit) => {
      await page.route(pattern, async (route) => {
        const response = await route.fetch();
        await route.fulfill({ response, json: edit(await response.json()) });
      });
      await page.getByRole("button", { name: "Verify again" }).click();
      await page.getByTestId("proof-verdict").filter({ hasText: "Verification failed" }).waitFor({ timeout: 15_000 });
      const issues = await page.getByTestId("proof-issues").innerText();
      await page.unroute(pattern);
      return issues;
    };
    let issues = await tamper("**/api/v1/reserva/cortes/*", (cut) => ({ ...cut, merkle_root: "00".repeat(32) }));
    check(issues.includes("public copy"), "a root that differs from the public cut fails verification");
    check(
      (await page.getByTestId("match-seal").getAttribute("data-status")) === "mismatch",
      "against a private root the trace ends without the match seal",
    );
    issues = await tamper("**/api/v1/reserva/prueba", (pr) => ({
      ...pr,
      hoja: { ...pr.hoja, cuenta_id: "00000000-0000-4000-8000-000000000000" },
    }));
    check(issues.includes("different account"), "a proof for another account fails verification");

    // Overdraft attempt through the bridge is blocked client-side. Back on the
    // Invest tab the form starts fresh (the receipt stayed with the old page).
    await openTab(page, "Invest");
    await page.fill("#bridge-amount", "15000.01");
    check(await page.getByText("More than your available vault balance.").isVisible(), "bridge blocks amounts above the balance");

    // ---------------------------------------------------------------------
    // Asset pages, by their address (/dashboard/asset/{ticker}).
    // ---------------------------------------------------------------------
    const quoteCalls = [];
    page.on("request", (r) => {
      if (r.method() === "GET" && r.url().includes("/api/v1/market/quote/")) quoteCalls.push(r);
    });
    await page.goto(`${BASE}/dashboard/asset/AUG1`);
    const assetDetail = page.getByTestId("asset-detail");
    await assetDetail.waitFor();
    check(
      quoteCalls.length === 1 &&
        quoteCalls[0].url().endsWith("/api/v1/market/quote/AUG1") &&
        /^Bearer \S+$/.test((await quoteCalls[0].allHeaders()).authorization ?? ""),
      "the asset's page loads its quote with the session's token",
    );
    check(
      (await assetDetail.innerText()).includes("$1,612.40 MXN, +0.84% in 24 hours") &&
        (await page.getByTestId("asset-simulated").isVisible()),
      "AUG1 shows its quote ($1,612.40, +0.84%) and says the prices are simulated",
    );
    check(
      (await mainNav(page).getByRole("link", { name: "Vault", exact: true }).getAttribute("aria-current")) === "page" &&
        (await page.getByRole("heading", { level: 1 }).innerText()) === "Vault",
      "an asset's page belongs to the Vault (current tab and title)",
    );
    check(await page.getByRole("button", { name: "Sell", exact: true }).isDisabled(), "Sell is disabled without a position");
    await shot(page, "asset-desktop");

    // The pro chart: a gold line over the month by default; candles on demand, over the same window and axes.
    const chart = page.getByTestId("pro-chart");
    const legend = page.getByTestId("chart-legend");
    check(
      (await chart.getAttribute("data-mode")) === "line" &&
        (await page.getByTestId("asset-line").count()) === 1 &&
        (await page.getByTestId("chart-volume").isVisible()) &&
        (await page.getByTestId("chart-last-price").innerText()) === "$1,612.40" &&
        /^High \$[\d,.]+ Low \$[\d,.]+ Range [+−]?[\d.]+%$/.test(flat(await legend.innerText())),
      "the chart opens as a gold line over the month: high, low and change above, volume below, $1,612.40 tagged",
    );
    await page.getByTestId("chart-mode").getByRole("button", { name: "Candles" }).click();
    await chart.and(page.locator("[data-mode=candles]")).waitFor();
    const days = await page.getByTestId("candle").count();
    check(
      (await chart.getAttribute("data-interval")) === "1d" &&
        days >= 30 &&
        days <= 32 &&
        (await page.getByTestId("asset-line").count()) === 0 &&
        /^DAILY O \$[\d,.]+ H \$[\d,.]+ L \$[\d,.]+ C \$1,612\.40 /.test(flat(await legend.innerText())),
      `Candles draws the month as ${days} daily candles; the legend reads the forming one, closing at $1,612.40`,
    );
    const plot = page.getByTestId("chart-plot");
    const plotBox = await plot.boundingBox();
    await page.mouse.move(plotBox.x + plotBox.width * 0.5, plotBox.y + plotBox.height * 0.4);
    await page.getByTestId("chart-crosshair").waitFor();
    const tagged = await page.getByTestId("chart-price-tag").innerText();
    check(
      /^\$[\d,]+\.\d{2}$/.test(tagged) &&
        tagged === (await page.getByTestId("asset-price").innerText()) &&
        (await page.getByTestId("chart-time-tag").innerText()) === (await page.getByTestId("asset-focus-date").innerText()) &&
        flat(await legend.innerText()).includes(`C ${tagged} `),
      `the crosshair snaps to a candle: its close (${tagged}) on the price axis and in the headline, its day on both, its prices in the legend`,
    );
    await shot(page, "asset-candles-desktop");
    await page.mouse.move(0, 0);
    await page.getByTestId("chart-crosshair").waitFor({ state: "detached" });
    await plot.focus();
    await page.keyboard.press("End");
    await chart.locator("p[aria-live]").filter({ hasText: "daily candle: opened" }).filter({ hasText: "closed $1,612.40" }).waitFor();
    await page.keyboard.press("Escape");
    check(true, "the keyboard reads the candles too: End announces the forming one, which closes at $1,612.40");
    for (const [range, interval, low, high, chip] of [
      ["the last day", "30m", 48, 49, "30 MIN"],
      ["the last week", "4h", 42, 43, "4 HOURS"],
      ["the last year", "1w", 52, 54, "WEEKLY"],
    ]) {
      await page.getByRole("button", { name: `Show ${range}` }).click();
      await chart.and(page.locator(`[data-interval="${interval}"]`)).waitFor();
      const count = await page.getByTestId("candle").count();
      check(
        count >= low && count <= high && flat(await legend.innerText()).startsWith(`${chip} O `),
        `over ${range} the candles last ${chip.toLowerCase()} (${count} of them)`,
      );
    }
    await page.getByRole("button", { name: "Show the last month" }).click();
    await page.getByTestId("asset-back").click();
    await page.waitForURL("**/dashboard");
    check(true, "Back to vault returns to the Vault");

    expectFailure("/api/v1/market/quote/ZZZ9");
    await page.goto(`${BASE}/dashboard/asset/ZZZ9`);
    await page.getByTestId("asset-notice").filter({ hasText: "There is no asset with the ticker ZZZ9." }).waitFor();
    check(quoteCalls.length === 2, "an unknown ticker shows No such asset (the API answers 404)");
    await page.goto(`${BASE}/dashboard/asset/gold`);
    await page.getByTestId("asset-notice").filter({ hasText: "This address does not name an asset." }).waitFor();
    check(quoteCalls.length === 2, "an address that is not a ticker shows No such asset without calling the API");
    await page.route("**/api/v1/market/quote/AUG1", (route) =>
      route.request().method() === "GET" ? route.fulfill(apiRefusal(503, "MERCADO_NO_DISPONIBLE")) : route.continue(),
    );
    expectFailure("/api/v1/market/quote/AUG1");
    await page.goto(`${BASE}/dashboard/asset/AUG1`);
    await page.getByTestId("asset-notice").filter({ hasText: "The market is not open yet" }).waitFor();
    await page.unroute("**/api/v1/market/quote/AUG1");
    check(true, "without live prices (503) the asset's page says the market is not open yet");
    await page.getByRole("link", { name: "Go to your vault" }).click();
    await page.waitForURL("**/dashboard");

    // ---------------------------------------------------------------------
    // Discover: the catalog, its search, and a first buy from it.
    // ---------------------------------------------------------------------
    await openTab(page, "Discover");
    const marketRows = page.getByTestId("market-row");
    await marketRows.first().waitFor();
    /** Waits until the list shows exactly these tickers, in this order. */
    const listing = (tickers) =>
      page.waitForFunction(
        (expected) =>
          JSON.stringify(
            [...document.querySelectorAll("[data-testid=market-row]")].map((r) => r.getAttribute("data-ticker")),
          ) === expected,
        JSON.stringify(tickers),
      );
    const TRENDING = ["STSLA", "AUG1", "CETES28", "AGT1", "SCT28"];
    await listing(TRENDING);
    check(true, "Discover lists the whole catalog, most traded first (STSLA, AUG1, CETES28, AGT1, SCT28)");
    check(
      (await page.getByRole("heading", { level: 1 }).innerText()) === "Discover" &&
        (await page.getByTestId("market-list").getByRole("heading", { name: "Trending" }).count()) === 1 &&
        (await page.getByTestId("discover-simulated").isVisible()),
      "the Discover tab is titled, its list is Trending, and it says the prices are simulated",
    );
    // The day's top movers and the collections, from the catalog alone.
    const quotesBefore = quoteCalls.length;
    const mover = (kind) => page.getByTestId("top-mover").and(page.locator(`[data-kind=${kind}]`));
    check(
      (await mover("gainer").getAttribute("data-ticker")) === "AUG1" &&
        (await mover("gainer").getByTestId("top-mover-change").innerText()) === "+0.84%" &&
        (await mover("gainer").getAttribute("href")) === "/dashboard/asset/AUG1" &&
        (await mover("loser").getAttribute("data-ticker")) === "STSLA" &&
        (await mover("loser").getByTestId("top-mover-change").innerText()) === "−2.40%",
      "Top movers: gold leads the day (+0.84%), the synthetic Tesla trails it (−2.40%); each opens its asset",
    );
    const collection = (id) => page.getByTestId("collection").and(page.locator(`[data-collection=${id}]`));
    check(
      (await collection("hard-assets").getByTestId("collection-tickers").innerText()) === "AUG1 · AGT1" &&
        (await collection("hard-assets").getByTestId("collection-change").innerText()) === "+0.16%" &&
        (await collection("synthetic-equities").getByTestId("collection-tickers").innerText()) === "STSLA" &&
        (await collection("fixed-income").getByTestId("collection-tickers").innerText()) === "CETES28 · SCT28" &&
        (await collection("fixed-income").getByTestId("collection-change").innerText()) === "+0.01%",
      "Collections list their assets, most traded first, and their equal-weight change (Hard Assets +0.16%, Fixed Income +0.01%)",
    );
    await page.mouse.move(0, 0);
    await shot(page, "discover-desktop");

    const search = page.getByTestId("market-search");
    for (const [query, tickers] of [
      ["gold", ["AUG1"]],
      ["CETES", ["CETES28"]],
      ["fixed income", ["CETES28", "SCT28"]],
      ["tsla", ["STSLA"]],
    ]) {
      await search.fill(query);
      await listing(tickers);
    }
    check(
      (await page.getByTestId("top-movers").count()) === 0 && (await page.getByTestId("collections").count()) === 0,
      "the search finds assets by name, ticker or class, whatever the case; the highlights step aside meanwhile",
    );
    await search.fill("bitcoin");
    await page.getByTestId("discover-no-match").waitFor();
    check(
      (await page.getByTestId("discover-status").textContent()) === "No assets match bitcoin",
      "a search with no match says so, on screen and to screen readers",
    );
    await page.getByTestId("discover-no-match").getByRole("button", { name: "Clear search" }).click();
    await listing(TRENDING);
    check(
      await search.evaluate((field) => field === document.activeElement && field.value === ""),
      "Clear search empties the field and leaves the focus in it",
    );

    // The screener: a collection presets the asset class, the toggles narrow further,
    // and each toggle shows what it would leave (the search and the other row included).
    const option = (value) => page.getByTestId("screener-option").and(page.locator(`[data-value="${value}"]`));
    const leaves = (value) => option(value).getByTestId("screener-option-count").innerText();
    const shownOf = async () => flat(await page.getByTestId("screener-count").innerText());
    await collection("hard-assets").click();
    await listing(["AUG1", "AGT1"]);
    check(
      (await collection("hard-assets").getAttribute("aria-pressed")) === "true" &&
        (await option("Commodity").getAttribute("aria-pressed")) === "true" &&
        (await shownOf()) === "2 of 5 assets" &&
        (await page.getByTestId("market-list").getByRole("heading", { name: "Screener" }).count()) === 1 &&
        (await leaves("up")) === "1" &&
        (await leaves("down")) === "1",
      "Hard Assets sets the screener to Commodities: gold and silver, one rising and one falling",
    );
    await option("up").click();
    await listing(["AUG1"]);
    await page.getByTestId("discover-status").filter({ hasText: "1 asset matches these filters" }).waitFor();
    check(
      (await shownOf()) === "1 of 5 assets" &&
        flat(await page.getByTestId("market-list-summary").innerText()) ===
          "Commodities · Positive 24h · most traded first" &&
        (await option("Equity").isDisabled()) &&
        (await leaves("Equity")) === "0",
      "Positive narrows Hard Assets to gold; Equities, all falling, would leave nothing and waits disabled",
    );
    await page.mouse.move(0, 0);
    await shot(page, "discover-screener-desktop");
    await collection("fixed-income").click();
    await listing(["CETES28"]);
    check(
      (await collection("hard-assets").getAttribute("aria-pressed")) === "false" &&
        (await collection("fixed-income").getAttribute("aria-pressed")) === "true" &&
        (await option("up").getAttribute("aria-pressed")) === "true",
      "Fixed Income replaces Hard Assets and keeps Positive: CETES rose; the flat T-bill (0.00%) stays out",
    );
    await search.fill("tesla");
    await page.getByTestId("discover-no-match").waitFor();
    check(
      (await page.getByTestId("discover-status").textContent()) === "No assets match tesla with these filters",
      "a search the filters exclude says so, on screen and to screen readers",
    );
    await page.getByTestId("discover-no-match").getByRole("button", { name: "Clear filters" }).click();
    await listing(["STSLA"]);
    check(
      (await page.getByTestId("market-list").getByRole("heading", { name: "Results" }).count()) === 1 &&
        (await page.getByTestId("screener-clear").count()) === 0,
      "Clear filters keeps the search: the Tesla share",
    );
    await option("Equity").click();
    await listing(["STSLA"]);
    check(quoteCalls.length === quotesBefore, "movers, collections and the screener need no request beyond the catalog");

    // A first buy, from Discover: a fraction of a synthetic Tesla share.
    await search.fill("tesla");
    await listing(["STSLA"]);
    await marketRows.first().click();
    await page.waitForURL("**/dashboard/asset/STSLA");
    const assetBack = page.getByTestId("asset-back");
    await page.getByTestId("asset-detail").waitFor();
    check(
      (await mainNav(page).getByRole("link", { name: "Discover", exact: true }).getAttribute("aria-current")) === "page" &&
        (await page.getByRole("heading", { level: 1 }).innerText()) === "Discover" &&
        (await assetBack.getAttribute("href")) === "/dashboard/discover" &&
        /back to discover/i.test(await assetBack.innerText()),
      "an asset opened from Discover stays under Discover, and its Back link returns there",
    );
    // AUG1's page was left in candles: this device remembers it.
    await page.getByTestId("pro-chart").and(page.locator("[data-mode=candles]")).waitFor();
    check(true, "the chart type is remembered on this device: STSLA opens as candles, as AUG1 was left");
    await page.getByTestId("chart-mode").getByRole("button", { name: "Line" }).click();
    await page.getByTestId("asset-line").waitFor();
    // The order types first. A limit or a stop takes its price on the same
    // numpad, a savings plan its frequency. Every type is live; this order
    // ends as a limit buy (savings plans have their own section, later).
    let ordersSent = 0;
    page.on("request", (r) => {
      if (r.method() === "POST" && r.url().endsWith("/api/v1/market/order")) ordersSent += 1;
    });
    await page.getByTestId("asset-buy").click();
    const drawer = page.getByTestId("order-checkout");
    const orderTypes = drawer.getByTestId("order-type");
    const chooseType = (label) => orderTypes.getByText(label, { exact: true }).click();
    const typeKeys = async (keys) => {
      for (const key of keys) await drawer.getByRole("button", { name: key, exact: true }).click();
    };
    const confirmButton = drawer.getByTestId("order-confirm");
    const summary = drawer.getByTestId("order-summary");
    const footnote = drawer.getByTestId("order-footnote");
    const buttonText = async () => (await confirmButton.innerText()).toLowerCase();
    check(
      JSON.stringify(await orderTypes.getByRole("radio").evaluateAll((radios) => radios.map((r) => r.value))) ===
        JSON.stringify(["market", "limit", "stop", "savings_plan"]) &&
        (await orderTypes.getByRole("radio", { name: "Market" }).isChecked()),
      "the buy drawer offers Market, Limit, Stop and Savings plan, Market first",
    );

    await chooseType("Limit");
    const priceField = drawer.getByTestId("order-price");
    await priceField.waitFor();
    await typeKeys(["4", "2", "0", "0"]);
    await drawer.getByTestId("order-amount").click();
    await typeKeys(["1", "0", "0", "0"]);
    const limitState = {
      price: await priceField.inputValue(),
      amount: await drawer.getByTestId("order-amount").inputValue(),
      summary: (await summary.innerText()).replace(/\s+/g, " "),
    };
    const limitOk =
      limitState.price === "4,200" &&
      limitState.amount === "1,000" &&
      limitState.summary.includes("$4,200.00") &&
      limitState.summary.includes("0.238 STSLA");
    check(
      limitOk,
      `a limit takes its price on the numpad, then the amount: 1,000.00 at 4,200.00 buys 0.238 STSLA${
        limitOk ? "" : ` (${JSON.stringify(limitState)})`
      }`,
    );
    check(
      (await buttonText()) === "place limit order" &&
        (await confirmButton.isEnabled()) &&
        (await drawer.getByTestId("order-not-live").count()) === 0 &&
        (await drawer.getByTestId("order-held").innerText()).includes("$1,000.00") &&
        (await footnote.innerText()).includes("only at $4,200.00 or lower") &&
        (await footnote.innerText()).includes("held from your cash until it fills or you cancel it"),
      "Place Limit Order is live, and the summary says the 1,000.00 stays held until it fills",
    );

    await chooseType("Stop");
    await drawer.getByText("To buy, a stop must be above the current price.").waitFor();
    check(await confirmButton.isDisabled(), "a stop to buy below the current price (4,200 < 4,415.30) is refused");
    for (let i = 0; i < 4; i++) await drawer.getByRole("button", { name: "Delete last digit" }).click();
    await typeKeys(["4", "6", "0", "0"]);
    check(
      (await footnote.innerText()).includes("when the price rises to $4,600.00") &&
        (await buttonText()) === "place stop order" &&
        (await confirmButton.isEnabled()),
      "a stop to buy above the price says when it triggers, and can be placed",
    );

    await chooseType("Savings plan");
    await drawer.getByTestId("order-frequency").getByText("Every 2 weeks", { exact: true }).click();
    check(
      (await priceField.count()) === 0 &&
        (await buttonText()) === "create savings plan" &&
        (await confirmButton.isEnabled()) &&
        (await drawer.getByTestId("order-not-live").count()) === 0 &&
        (await summary.innerText()).includes("$2,166.67"),
      "a savings plan asks how often (1,000.00 every 2 weeks is about $2,166.67 a month), and can be created",
    );
    await shot(page, "checkout-plan-desktop");

    // A limit buy below the price waits, its 1,000.00 held.
    await chooseType("Limit");
    for (let i = 0; i < 4; i++) await drawer.getByRole("button", { name: "Delete last digit" }).click();
    await typeKeys(["4", "2", "0", "0"]);
    check(ordersSent === 0, "nothing is sent while the order is set up");
    await confirmButton.click();
    const firstReceipt = drawer.getByTestId("order-receipt");
    await firstReceipt.waitFor();
    const placedLimit = await firstReceipt.innerText();
    check(
      (await firstReceipt.getAttribute("data-outcome")) === "pending" &&
        placedLimit.includes("Limit buy placed") &&
        placedLimit.includes("Buys STSLA at $4,200.00 or lower.") &&
        (await drawer.getByTestId("receipt-held").innerText()).includes("$1,000.00") &&
        placedLimit.includes("Open orders on your Vault") &&
        ordersSent === 1,
      "Place Limit Order: the limit buy waits below the price with its 1,000.00 held",
    );
    await shot(page, "checkout-limit-placed-desktop");
    await drawer.getByRole("button", { name: "Done" }).click();
    await drawer.waitFor({ state: "detached" });

    // Then a market buy of 1,000.00, from the 14,000.00 left available (the
    // vault's WebSocket brings the new available cash).
    await page.getByTestId("asset-buy").click();
    await drawer.getByText("Available: $14,000.00").waitFor({ timeout: 5_000 });
    await typeKeys(["1", "0", "0", "0"]);
    check(
      (await buttonText()) === "confirm order" && (await drawer.getByTestId("order-amount").inputValue()) === "1,000",
      "a new order starts as a market order, and the cash held by the limit is not available",
    );
    await confirmButton.click();
    await firstReceipt.waitFor();
    check(
      (await firstReceipt.innerText()).includes("Bought 0.2264 STSLA at $4,415.30."),
      "a first buy from Discover: 1,000.00 buys 0.2264 STSLA",
    );
    await drawer.getByRole("button", { name: "Done" }).click();
    await drawer.waitFor({ state: "detached" });
    await page.getByTestId("asset-detail").getByText("0.2264 STSLA", { exact: true }).waitFor({ timeout: 5_000 });
    await assetBack.click();
    await page.waitForURL("**/dashboard/discover");
    await listing(["STSLA"]);
    check(
      (await search.inputValue()) === "tesla" && (await option("Equity").getAttribute("aria-pressed")) === "true",
      "back on Discover, the search, the screener and their results are as they were left",
    );
    await page.getByTestId("screener-clear").click();
    await page.getByTestId("market-search-clear").click();
    await listing(TRENDING);

    // The Vault: the open limit buy sits above the assets, with the cash it holds.
    await openTab(page, "Vault");
    const openOrders = page.getByTestId("open-orders");
    const openRows = openOrders.getByTestId("open-order");
    const limitRow = openRows.filter({ hasText: "Limit buy" });
    await limitRow.waitFor({ timeout: 5_000 });
    check(
      (await openRows.count()) === 1 &&
        (await limitRow.getAttribute("data-ticker")) === "STSLA" &&
        (await limitRow.getByTestId("open-order-trigger").innerText()) === "at $4,200.00 or lower" &&
        (await limitRow.innerText()).includes("≈ 0.238 STSLA") &&
        (await limitRow.getByTestId("open-order-held").innerText()).includes("$1,000.00"),
      "the Vault lists the open limit buy: STSLA, at $4,200.00 or lower, about 0.238 STSLA, 1,000.00 held",
    );
    const assetList = page.getByTestId("asset-list");
    check(
      (await openOrders.boundingBox()).y < (await assetList.boundingBox()).y &&
        (await openOrders.getByTestId("open-orders-held").innerText()).includes("$1,000.00") &&
        (await page.getByTestId("vault-held-cash").innerText()).includes("1,000.00 MXN held for open orders"),
      "open orders come before the assets, and the balance says what they hold",
    );

    // A stop-loss placed on another device appears live, newest first, with the units it holds.
    const elsewhere = await apiLogin(email, password);
    const stopLoss = await apiOrder(elsewhere, {
      ticker: "STSLA",
      side: "sell",
      amount: "500.00",
      order_type: "stop",
      stop_price: "4000.0000",
    });
    check(
      stopLoss.status === 201 && stopLoss.receipt.status === "pending" && stopLoss.receipt.held_quantity === "0.12500000",
      `a stop-loss of 500.00 at 4,000.00 placed on another device holds 0.125 STSLA (${stopLoss.status})`,
    );
    const stopRow = openOrders.locator(`[data-order-id="${stopLoss.receipt.order_id}"]`);
    await stopRow.waitFor({ timeout: 5_000 });
    check(
      (await openRows.count()) === 2 &&
        (await openRows.first().getAttribute("data-order-id")) === stopLoss.receipt.order_id &&
        (await stopRow.innerText()).includes("Stop-loss") &&
        (await stopRow.getByTestId("open-order-trigger").innerText()) === "when the price falls to $4,000.00" &&
        (await stopRow.getByTestId("open-order-held").innerText()).includes("0.125 STSLA"),
      "the stop-loss appears live, newest first, holding 0.125 STSLA",
    );
    await page.mouse.move(0, 0);
    await shot(page, "open-orders-desktop");

    // On a phone the rows fit the screen: the amount and what is held move under the order.
    await page.setViewportSize({ width: 390, height: 844 });
    await openOrders.scrollIntoViewIfNeeded();
    check(
      (await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)) &&
        (await stopRow.getByTestId("open-order-cancel").isVisible()) &&
        (await stopRow.innerText()).includes("Held 0.125 STSLA"),
      "on a phone the open orders fit the width, Cancel included, and still say what each one holds",
    );
    await shot(page, "open-orders-mobile");
    await page.setViewportSize({ width: 1440, height: 1000 });

    // Cancel: the row leaves at once, and the WebSocket brings back what it held.
    const notice = openOrders.getByTestId("open-orders-notice");
    await stopRow.getByTestId("open-order-cancel").click();
    await stopRow.waitFor({ state: "detached", timeout: 2_000 });
    await notice.filter({ hasText: "Stop-loss of STSLA cancelled" }).waitFor();
    check(
      (await notice.innerText()).includes("Your 0.125 STSLA are free to sell again.") && (await openRows.count()) === 1,
      "Cancel removes the stop-loss at once and says its units are free again",
    );
    await limitRow.getByTestId("open-order-cancel").click();
    await limitRow.waitFor({ state: "detached", timeout: 2_000 });
    await notice.filter({ hasText: "Limit buy of STSLA cancelled. $1,000.00 is back in your cash." }).waitFor();
    await page.getByTestId("vault-held-cash").waitFor({ state: "detached", timeout: 5_000 });
    check(true, "cancelling the limit buy returns its 1,000.00: the balance no longer shows cash held for orders");
    await openOrders.waitFor({ state: "detached", timeout: 10_000 });
    check(true, "with nothing open, the Open orders section leaves the Vault once its notice is read");

    // The Vault lists STSLA; opened from there, the page is the Vault's, and Max sells it all.
    const teslaRow = page.getByTestId("asset-row").and(page.locator("[data-ticker=STSLA]"));
    await teslaRow.getByTestId("asset-row-link").click();
    await page.waitForURL("**/dashboard/asset/STSLA");
    await page.getByTestId("asset-detail").getByText("0.2264 STSLA", { exact: true }).waitFor();
    check(
      (await assetBack.getAttribute("href")) === "/dashboard" && /back to vault/i.test(await assetBack.innerText()),
      "opened from the Vault's holdings, the asset's Back link returns to the Vault",
    );
    await page.getByRole("button", { name: "Sell", exact: true }).click();
    check(
      JSON.stringify(await orderTypes.getByRole("radio").evaluateAll((radios) => radios.map((r) => r.value))) ===
        JSON.stringify(["market", "limit", "stop"]),
      "the sell drawer offers Market, Limit and Stop (a savings plan only buys)",
    );
    await chooseType("Stop");
    await typeKeys(["4", "0", "0", "0"]);
    await drawer.getByRole("button", { name: "Max" }).click();
    check(
      (await drawer.getByTestId("order-amount").inputValue()) === "905.6" &&
        (await buttonText()) === "place stop-loss" &&
        (await confirmButton.isEnabled()) &&
        (await drawer.getByTestId("order-held").innerText()).includes("0.2264 STSLA"),
      "a stop-loss at 4,000.00 sells at most what the position is worth there (905.60), and would hold all 0.2264 STSLA",
    );
    await chooseType("Market");
    await drawer.getByRole("button", { name: "Max" }).click();
    await drawer.getByTestId("order-confirm").click();
    await firstReceipt.waitFor();
    check((await firstReceipt.innerText()).includes("Sold 0.2264 STSLA at $4,415.30."), "Max sells the whole STSLA position");
    await drawer.getByRole("button", { name: "Done" }).click();
    await drawer.waitFor({ state: "detached" });
    await assetBack.click();
    await page.waitForURL("**/dashboard");
    await page.getByTestId("asset-list-empty").waitFor();
    await openTab(page, "Account");
    await page.getByTestId("vault-available").filter({ hasText: "15,000.00" }).waitFor();
    check(true, "buying and selling all of it at the same price returns exactly the cash paid (15,000.00)");

    // ---------------------------------------------------------------------
    // Documents (roadmap item 24): a trade confirmation for every executed
    // order and the month's statement, derived from the ledgers
    // ---------------------------------------------------------------------
    const accountToken = await pageToken(page);
    /** An API call with this browser's session, from outside the page. */
    const asClient = async (path, init = {}) => {
      const response = await fetch(`${API}${path}`, {
        ...init,
        headers: {
          authorization: `Bearer ${accountToken}`,
          ...(init.body ? { "content-type": "application/json" } : {}),
        },
      });
      return { status: response.status, body: await response.json().catch(() => null) };
    };
    const inbox = (await asClient("/api/v1/wealth/documentos")).body;
    const cashLedger = (await asClient("/api/v1/ifpe/movimientos?limite=200")).body;
    const settlements = cashLedger.filter((m) => m.tipo === "COMPRA_MERCADO" || m.tipo === "VENTA_MERCADO");
    const confirmations = inbox.documents.filter((d) => d.kind === "trade_confirmation");
    const statements = inbox.documents.filter((d) => d.kind === "account_statement");
    const mexicoMonth = (iso) => new Date(Date.parse(iso) - 6 * 3_600_000).toISOString().slice(0, 7);
    const activeMonths = new Set(cashLedger.map((m) => mexicoMonth(m.creado_en)));
    check(
      settlements.length >= 2 &&
        confirmations.length === settlements.length &&
        statements.length === activeMonths.size &&
        statements[0].provisional &&
        !inbox.truncated,
      `the inbox holds a trade confirmation for each of the ${settlements.length} executed orders (one per settlement in the cash ledger) and a statement per active month, this one in progress`,
    );

    await openTab(page, "Documents");
    check(
      JSON.stringify(await mainNav(page).locator("ul").nth(1).getByRole("link").allInnerTexts()) ===
        JSON.stringify(["Documents", "Taxes", "Beneficiaries", "Security"]),
      "the sidebar's Account group lists Documents, Taxes, Beneficiaries and Security",
    );
    const docRows = page.getByTestId("document-row");
    await docRows.nth(inbox.documents.length - 1).waitFor();
    const newestRow = flat(await docRows.first().innerText());
    check(
      (await docRows.count()) === inbox.documents.length &&
        JSON.stringify(await docRows.evaluateAll((rows) => rows.map((r) => r.getAttribute("data-id")))) ===
          JSON.stringify(inbox.documents.map((d) => d.id)) &&
        newestRow.includes(inbox.documents[0].folio) &&
        newestRow.includes(inbox.documents[0].reference.slice(0, 8)),
      "the inbox lists every document the API serves, newest first, each with its folio and reference hash",
    );
    const inboxFilter = page.getByTestId("documents-filter");
    await inboxFilter.getByText(`Confirmations · ${confirmations.length}`).click();
    await page.waitForFunction(
      (n) => document.querySelectorAll("[data-testid=document-row]").length === n,
      confirmations.length,
    );
    check(
      await docRows.evaluateAll((rows) => rows.every((r) => r.getAttribute("data-kind") === "trade_confirmation")),
      "the Confirmations filter keeps only the trade confirmations",
    );
    await shot(page, "documents-inbox");

    // The newest confirmation: the sale of the whole STSLA position.
    const latest = confirmations[0];
    const sale = (await asClient(`/api/v1/wealth/documentos/${latest.id}`)).body;
    check(
      sale.side === "sell" &&
        sale.ticker === "STSLA" &&
        toUnits(sale.quantity) === toUnits("0.2264") &&
        isZero(sale.commission) &&
        sale.net_amount === sale.gross_amount &&
        sale.reference === latest.reference &&
        sale.settlement_sequence === settlements.find((m) => m.tipo === "VENTA_MERCADO")?.secuencia,
      "the newest confirmation is the sale of all 0.2264 STSLA, settled by the latest sale in the cash ledger, with zero commission: net equals gross",
    );
    const viewer = page.getByTestId("document-viewer");
    await docRows.first().click();
    await viewer.getByTestId("document-screen").waitFor();
    await page.waitForURL(`**/dashboard/documents?id=${latest.id}`);
    const headline = flat(await viewer.getByTestId("confirmation-headline").innerText());
    check(headline === "Sold 0.2264 STSLA at $4,415.30 MXN", `the confirmation reads "${headline}"`);
    check(
      (await viewer.getByTestId("document-folio").innerText()) === sale.folio &&
        (await viewer.getByTestId("document-reference").getAttribute("data-value")) === sale.reference &&
        (await viewer.getByTestId("confirmation-commission").innerText()).includes("$0.00") &&
        (await viewer.getByTestId("commission-breakdown").getByText("Zero commission").isVisible()) &&
        (await viewer.getByTestId("confirmation-net").innerText()).includes(formatMoney(sale.net_amount)),
      "the viewer shows the folio, the reference hash, a $0.00 commission marked Zero commission and the net credited",
    );
    check(
      (await viewer.getByTestId("reserve-seal").getAttribute("data-status")) === sale.reserve_seal.status,
      `before a new Proof of Reserve snapshot covers it, the seal reads ${sale.reserve_seal.status}`,
    );
    await viewer.getByRole("button", { name: "Close" }).click();
    await viewer.waitFor({ state: "detached" });
    await page.waitForURL((url) => url.pathname === "/dashboard/documents" && url.search === "");

    // A new snapshot seals it: the first one whose anchors reach its entries.
    await freshCut(`e2e-${runId}-docs@example.mx`, password);
    const seal = (await asClient(`/api/v1/wealth/documentos/${latest.id}`)).body.reserve_seal;
    const sealingCut = await (await fetch(`${API}/api/v1/reserva/cortes/${seal.snapshot_id}`)).json();
    check(
      seal.status === "sealed" &&
        seal.reserve_ratio === "100.00" &&
        seal.root === sealingCut.merkle_root &&
        seal.ledger_anchor === sealingCut.secuencia_ledger &&
        sealingCut.secuencia_ledger >= sale.settlement_sequence &&
        (sealingCut.secuencia_mercado ?? 0) >= sale.market_sequence,
      "after a new Proof of Reserve snapshot the confirmation is sealed by it: its Merkle root, its anchors past the trade, 100.00% backed",
    );
    await docRows.first().click();
    await viewer.getByTestId("reserve-seal").and(page.locator("[data-status=sealed]")).waitFor();
    check(
      (await viewer.getByTestId("seal-root").getAttribute("data-value")) === seal.root &&
        flat(await viewer.getByTestId("reserve-seal").textContent()).includes("Reserve verified · 100.00% backed"),
      "the viewer shows the seal: Reserve verified · 100.00% backed, with the snapshot's Merkle root",
    );
    await shot(page, "document-confirmation", { fullPage: false });

    // Download (.json): the document exactly as the API serves it.
    const [download] = await Promise.all([
      page.waitForEvent("download"),
      viewer.getByTestId("document-download").click(),
    ]);
    const downloadPath = `${OUT}/${download.suggestedFilename()}`;
    await download.saveAs(downloadPath);
    const served = (await asClient(`/api/v1/wealth/documentos/${latest.id}`)).body;
    check(
      download.suggestedFilename() === `synthetic-capital-${served.folio}.json` &&
        isDeepStrictEqual(JSON.parse(readFileSync(downloadPath, "utf8")), served),
      `Download (.json) saves ${download.suggestedFilename()}: the confirmation exactly as the API serves it`,
    );

    // Printed: the document alone, black on white, every hash in full.
    await page.emulateMedia({ media: "print" });
    const paper = await page.evaluate(() => {
      const copy = document.querySelector("body > .document-print-copy");
      return {
        copy: copy ? getComputedStyle(copy).display : null,
        rest: [...document.body.children]
          .filter((el) => el !== copy)
          .every((el) => getComputedStyle(el).display === "none"),
        dialog: getComputedStyle(document.querySelector("dialog")).display,
        paper: copy ? getComputedStyle(copy.querySelector(".print-document")).backgroundColor : null,
        ink: copy ? getComputedStyle(copy.querySelector("[data-testid=confirmation-headline]")).color : null,
        hash: copy?.querySelector("[data-testid=document-reference]")?.textContent ?? null,
      };
    });
    check(
      paper.copy === "block" &&
        paper.rest &&
        paper.dialog === "none" &&
        paper.paper === "rgb(255, 255, 255)" &&
        paper.ink === "rgb(0, 0, 0)" &&
        paper.hash === served.reference,
      "printed, the page is the document alone, black on white, with the reference hash in full",
    );
    const confirmationPdf = await page.pdf({ path: `${OUT}/trade-confirmation.pdf`, format: "A4" });
    await page.emulateMedia({ media: null });
    check(
      confirmationPdf.subarray(0, 5).toString() === "%PDF-" && confirmationPdf.length > 10_000,
      `Print / Save as PDF renders the confirmation (${Math.round(confirmationPdf.length / 1024)} KB)`,
    );
    await viewer.getByRole("button", { name: "Close" }).click();
    await viewer.waitFor({ state: "detached" });

    // The month's statement: the account's balance, every movement, zero commissions.
    await inboxFilter.getByText(`Statements · ${statements.length}`).click();
    await page.waitForFunction(
      (n) => document.querySelectorAll("[data-testid=document-row]").length === n,
      statements.length,
    );
    const statement = (await asClient(`/api/v1/wealth/documentos/${statements[0].id}`)).body;
    const account = (await asClient("/api/v1/tablero")).body.boveda;
    // Holds and order commitments move cash inside the account: the statement lists what changes its balance.
    const BALANCE_MOVEMENTS = [
      "DEPOSITO",
      "RETIRO",
      "TRANSFERENCIA",
      "TRASPASO_A_SOFOM",
      "RETORNO_DE_SOFOM",
      "COMPRA_MERCADO",
      "VENTA_MERCADO",
    ];
    const monthLedger = cashLedger.filter(
      (m) => mexicoMonth(m.creado_en) === statement.period && BALANCE_MOVEMENTS.includes(m.tipo),
    );
    const tradesThisMonth = settlements.filter((m) => mexicoMonth(m.creado_en) === statement.period).length;
    check(
      toUnits(statement.closing_balance) ===
        toUnits(account.saldo_disponible) +
          toUnits(account.saldo_bloqueado) +
          toUnits(account.saldo_comprometido ?? "0") &&
        toUnits(statement.closing_breakdown.available) === toUnits(account.saldo_disponible) &&
        toUnits(statement.opening_balance) + toUnits(statement.credits) - toUnits(statement.debits) ===
          toUnits(statement.closing_balance) &&
        statement.entries.length === monthLedger.length &&
        statement.trades.buys + statement.trades.sells === tradesThisMonth &&
        isZero(statement.trades.commission),
      `the statement closes at the account's balance (opening + credits − debits), lists all ${monthLedger.length} movements of the month and its ${tradesThisMonth} trades, with zero commissions`,
    );
    await docRows.first().click();
    await viewer.getByTestId("statement-closing").waitFor();
    check(
      (await viewer.getByTestId("document-folio").innerText()) === statement.folio &&
        (await viewer.getByTestId("statement-closing").innerText()).includes(formatMoney(statement.closing_balance)) &&
        (await viewer.getByTestId("statement-commission").innerText()).includes("$0.00") &&
        (await viewer.getByTestId("statement-entries").locator("tbody tr").count()) === statement.entries.length,
      "the viewer shows the statement's folio, closing balance, every movement and $0.00 in commissions",
    );
    await page.emulateMedia({ media: "print" });
    const statementPdf = await page.pdf({ path: `${OUT}/account-statement.pdf`, format: "A4" });
    await page.emulateMedia({ media: null });
    check(
      statementPdf.subarray(0, 5).toString() === "%PDF-" && statementPdf.length > 10_000,
      `Print / Save as PDF renders the statement (${Math.round(statementPdf.length / 1024)} KB)`,
    );
    await viewer.getByRole("button", { name: "Close" }).click();
    await viewer.waitFor({ state: "detached" });

    // A document's address opens it; Escape closes it and the address returns to the inbox.
    await page.goto(`${BASE}/dashboard/documents?id=${statement.id}`);
    await viewer.getByTestId("statement-closing").waitFor();
    check(true, "a document's address (?id=) opens it in the viewer");
    await page.keyboard.press("Escape");
    await viewer.waitFor({ state: "detached" });
    await page.waitForURL((url) => url.pathname === "/dashboard/documents" && url.search === "");
    check(true, "Escape closes the viewer, and the address returns to the inbox");

    // On a phone: the account pages as a strip of links, the viewer full screen.
    await page.setViewportSize({ width: 390, height: 844 });
    const accountPages = page.getByRole("navigation", { name: "Account pages" });
    const documentsLink = accountPages.getByRole("link", { name: "Documents" });
    await documentsLink.waitFor();
    check(
      JSON.stringify(await accountPages.getByRole("link").allInnerTexts()) ===
        JSON.stringify(["Documents", "Taxes", "Beneficiaries", "Security"]) &&
        (await documentsLink.getAttribute("aria-current")) === "page" &&
        (await settledStyle(documentsLink, "backgroundColor")) === "rgb(234, 179, 8)" &&
        (await mainNav(page).getByRole("link").count()) === 5,
      "on a phone the account pages are a strip of links, Documents gold while current; the tab bar keeps its five tabs",
    );
    check(
      await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      "the inbox fits a phone's width",
    );
    await shot(page, "documents-phone");
    await docRows.and(page.locator("[data-kind=trade_confirmation]")).first().click();
    await viewer.getByTestId("document-screen").waitFor();
    // Measured once the sheet has slid into place.
    await viewer.evaluate((d) => Promise.all(d.getAnimations().map((a) => a.finished.catch(() => undefined))));
    const sheet = await viewer.boundingBox();
    check(
      sheet.x === 0 &&
        sheet.y === 0 &&
        sheet.width === 390 &&
        sheet.height === 844 &&
        (await viewer.evaluate((d) => d.scrollWidth <= d.clientWidth)),
      "on a phone the viewer takes the whole screen, without sideways scrolling",
    );
    await shot(page, "document-confirmation-phone", { fullPage: false });
    await page.keyboard.press("Escape");
    await viewer.waitFor({ state: "detached" });
    await page.setViewportSize({ width: 1440, height: 1000 });

    // ---------------------------------------------------------------------
    // Legal beneficiaries (roadmap item 25): shares that add up to exactly
    // 100%, saved as immutable, hash-chained versions
    // ---------------------------------------------------------------------
    const spouseCurp = `LORJ900101HDFPJS0${curpCheckDigit("LORJ900101HDFPJS0")}`;
    await openTab(page, "Beneficiaries");
    const allocationText = page.getByTestId("allocation-label");
    const allocationMeter = page.getByTestId("allocation-bar");
    const saveDesignation = page.getByTestId("save-beneficiaries");
    const designationStatus = page.getByTestId("designation-status");
    const beneficiaryCards = page.getByTestId("beneficiary-card");
    const addBeneficiary = page.getByRole("button", { name: "Add beneficiary" });
    const shares = () =>
      beneficiaryCards.getByTestId("beneficiary-percent").evaluateAll((inputs) => inputs.map((i) => i.value));
    await page.getByText("No beneficiaries yet.", { exact: false }).waitFor();
    check(
      (await allocationText.innerText()) === "No beneficiaries" && !(await saveDesignation.isEnabled()),
      "with no beneficiaries yet the allocation is empty and there is nothing to save",
    );
    await addBeneficiary.click();
    const firstHeir = beneficiaryCards.nth(0);
    check(
      (await firstHeir.getByTestId("beneficiary-percent").inputValue()) === "100" &&
        (await firstHeir.getByLabel("Full legal name").evaluate((el) => el === document.activeElement)) &&
        (await allocationText.innerText()) === "100% allocated",
      "the first beneficiary takes 100%, and the cursor waits in their name",
    );
    await firstHeir.getByLabel("Full legal name").fill("María López Rojas");
    await firstHeir.getByLabel("Relationship").selectOption("HIJO");
    await firstHeir.getByLabel("Date of birth").fill("2005-06-07");
    await firstHeir.getByTestId("beneficiary-percent").fill("60");
    await addBeneficiary.click();
    const secondHeir = beneficiaryCards.nth(1);
    check(
      (await secondHeir.getByTestId("beneficiary-percent").inputValue()) === "40",
      "a second beneficiary takes what is left: 40%",
    );
    await secondHeir.getByLabel("Full legal name").fill("José López Rojas");
    await secondHeir.getByLabel("Relationship").selectOption("CONYUGE");
    await secondHeir.getByLabel("CURP").fill(spouseCurp);
    check(
      (await secondHeir.getByLabel("Date of birth").inputValue()) === "1990-01-01",
      "a valid CURP fills in the date of birth it carries",
    );
    await secondHeir.getByTestId("beneficiary-percent").fill("30");
    check(
      (await allocationText.innerText()) === "90% allocated · 10% left" &&
        (await allocationMeter.getAttribute("aria-valuetext")) === "90% allocated · 10% left" &&
        (await allocationMeter.getAttribute("data-status")) === "under" &&
        !(await saveDesignation.isEnabled()) &&
        (await designationStatus.innerText()) === "The percentages must add up to exactly 100%.",
      "60% + 30% reads 90% allocated · 10% left, and Save stays off until the shares add up to exactly 100%",
    );
    await secondHeir.getByTestId("beneficiary-percent").fill("40");
    check(
      (await allocationText.innerText()) === "100% allocated" &&
        (await settledStyle(allocationMeter, "borderTopColor")) === "rgb(234, 179, 8)" &&
        (await saveDesignation.isEnabled()) &&
        (await designationStatus.innerText()) === "Ready to save.",
      "60% + 40%: 100% allocated, the bar outlined in gold, and Save turns on",
    );
    await shot(page, "beneficiaries-draft");
    // Saving needs a fresh check (step-up, roadmap item 22): this session has none yet, so the
    // server saves nothing (403) and "Confirm it's you" names who is being designated.
    expectFailure("/api/v1/kyc/beneficiarios");
    await saveDesignation.click();
    const designationCheck = page.getByTestId("step-up-modal");
    await designationCheck.getByTestId("step-up-password").waitFor();
    const designationWaiting = await designationCheck.getByTestId("step-up-action").innerText();
    check(
      designationWaiting.includes("Save 2 beneficiaries") &&
        designationWaiting.includes("María López Rojas 60% · José López Rojas 40%") &&
        (await page.getByText("Saved as version 1").count()) === 0,
      "Save first asks to confirm it's you, naming the two beneficiaries and their shares; nothing is saved yet",
    );
    await shot(page, "step-up-beneficiaries", { fullPage: false });
    await designationCheck.getByTestId("step-up-password").fill(password);
    await designationCheck.getByTestId("step-up-submit").click();
    await designationCheck.waitFor({ state: "detached" });
    await page.getByText("Saved as version 1").waitFor();
    check(true, "confirmed: the same save goes on, as version 1");
    const firstVersion = (await asClient("/api/v1/kyc/beneficiarios")).body;
    check(
      firstVersion.version === 1 &&
        JSON.stringify(
          firstVersion.vigente.beneficiarios.map((b) => [
            b.nombre_completo,
            b.parentesco,
            b.curp,
            b.fecha_nacimiento,
            b.porcentaje,
          ]),
        ) ===
          JSON.stringify([
            ["MARÍA LÓPEZ ROJAS", "HIJO", null, "2005-06-07", "60.00"],
            ["JOSÉ LÓPEZ ROJAS", "CONYUGE", spouseCurp, "1990-01-01", "40.00"],
          ]) &&
        firstVersion.cadena_verificada,
      "saved as version 1: legal names in capitals, 60.00% and 40.00%, the chain verified by the server",
    );
    check(
      (await page.getByTestId("designation-version").innerText()) === "Version 1" &&
        (await page.getByTestId("chain-seal").getAttribute("data-verified")) === "true" &&
        (await page.getByTestId("history-row").count()) === 1 &&
        (await designationStatus.innerText()) === "No unsaved changes.",
      "the record shows version 1 with its chain verified, and its history",
    );
    await page.reload();
    await beneficiaryCards.nth(1).waitFor();
    check(
      JSON.stringify(await shares()) === JSON.stringify(["60", "40"]) &&
        (await allocationText.innerText()) === "100% allocated",
      "after a reload the editor holds what is on record: 60% and 40%",
    );

    // A third beneficiary, and the shares split evenly to the cent.
    await addBeneficiary.click();
    const thirdHeir = beneficiaryCards.nth(2);
    check(
      (await thirdHeir.getByTestId("beneficiary-percent").inputValue()) === "",
      "with nothing left, a third beneficiary starts without a share",
    );
    await thirdHeir.getByLabel("Full legal name").fill("Ana López Rojas");
    await thirdHeir.getByLabel("Relationship").selectOption("NIETO");
    await thirdHeir.getByLabel("Date of birth").fill("2015-03-04");
    await page.getByRole("button", { name: "Split evenly" }).click();
    check(
      JSON.stringify(await shares()) === JSON.stringify(["33.34", "33.33", "33.33"]) &&
        (await allocationText.innerText()) === "100% allocated",
      "Split evenly gives 33.34%, 33.33% and 33.33%: exactly 100%",
    );
    await saveDesignation.click();
    await page.getByText("Saved as version 2").waitFor();
    check(
      (await page.getByTestId("step-up-modal").count()) === 0,
      "within the five minutes the next save goes straight through: no second check",
    );

    // Another device saves first: this page's save is refused (409); the newer
    // version shows and the edits stay, to be saved again on top of it.
    await thirdHeir.getByRole("button", { name: "Remove beneficiary 3" }).click();
    await page.getByRole("button", { name: "Split evenly" }).click();
    const otherDevice = await apiLogin(email, password, WINDOWS_UA);
    const otherDeviceHeaders = {
      "content-type": "application/json",
      authorization: `Bearer ${otherDevice}`,
      "user-agent": WINDOWS_UA,
    };
    const saveElsewhere = () =>
      fetch(`${API}/api/v1/kyc/beneficiarios`, {
        method: "POST",
        headers: otherDeviceHeaders,
        body: JSON.stringify({
          beneficiarios: [
            {
              nombre_completo: "María López Rojas",
              parentesco: "HIJO",
              fecha_nacimiento: "2005-06-07",
              porcentaje: "100",
            },
          ],
          version_base: 2,
        }),
      });
    // A window belongs to the session that opened it: the other device has to confirm too.
    const unconfirmedElsewhere = await saveElsewhere();
    const unconfirmedBody = await unconfirmedElsewhere.json();
    const confirmedElsewhere = await fetch(`${API}/api/v1/auth/step-up`, {
      method: "POST",
      headers: otherDeviceHeaders,
      body: JSON.stringify({ password }),
    });
    check(
      unconfirmedElsewhere.status === 403 &&
        unconfirmedBody.error.codigo === "STEP_UP_REQUERIDO" &&
        confirmedElsewhere.status === 200,
      "another device's session cannot save without its own check (403 STEP_UP_REQUERIDO), and confirms with the password",
    );
    const savedElsewhere = await saveElsewhere();
    check(savedElsewhere.status === 201, "meanwhile another device saves version 3");
    expectFailure("/api/v1/kyc/beneficiarios");
    await saveDesignation.click();
    await page.getByText("Your beneficiaries were changed from another session.", { exact: false }).waitFor();
    check(
      (await page.getByTestId("designation-version").innerText()) === "Version 3" &&
        JSON.stringify(await shares()) === JSON.stringify(["50", "50"]) &&
        (await saveDesignation.isEnabled()),
      "this page's save on top of version 2 is refused (409): version 3 shows on record, the 50/50 edits stay",
    );
    await saveDesignation.click();
    await page.getByText("Saved as version 4").waitFor();
    check(
      (await page.getByTestId("history-row").count()) === 4,
      "saved again, as version 4: four versions in the history",
    );
    const refusedSum = await asClient("/api/v1/kyc/beneficiarios", {
      method: "POST",
      body: JSON.stringify({
        beneficiarios: [
          {
            nombre_completo: "María López Rojas",
            parentesco: "HIJO",
            fecha_nacimiento: "2005-06-07",
            porcentaje: "60",
          },
          { nombre_completo: "José López Rojas", parentesco: "CONYUGE", curp: spouseCurp, porcentaje: "39" },
        ],
        version_base: 4,
      }),
    });
    const afterRefusal = (await asClient("/api/v1/kyc/beneficiarios")).body;
    check(
      refusedSum.status === 422 &&
        refusedSum.body.error.codigo === "BENEFICIARIOS_PORCENTAJE" &&
        afterRefusal.version === 4 &&
        afterRefusal.cadena_verificada,
      "the API refuses shares that add up to 99% (422 BENEFICIARIOS_PORCENTAJE): still version 4, the chain intact",
    );
    const designationEvents = (await asClient("/api/v1/auth/security/events")).body.eventos.filter(
      (e) => e.evento === "BENEFICIARIOS_DESIGNADOS",
    );
    check(
      JSON.stringify(designationEvents.map((e) => [e.detalle, e.dispositivo])) ===
        JSON.stringify([
          ["4", designationEvents[0]?.dispositivo ?? null],
          ["3", "Edge on Windows"],
          ["2", designationEvents[0]?.dispositivo ?? null],
          ["1", designationEvents[0]?.dispositivo ?? null],
        ]) && designationEvents[0].sesion_actual,
      "every version is in the security log, with the device that saved it (version 3 from the other device)",
    );
    await fetch(`${API}/api/v1/auth/logout`, { method: "POST", headers: { authorization: `Bearer ${otherDevice}` } });

    await page.setViewportSize({ width: 390, height: 844 });
    const beneficiariesLink = page
      .getByRole("navigation", { name: "Account pages" })
      .getByRole("link", { name: "Beneficiaries" });
    await beneficiariesLink.waitFor();
    check(
      (await beneficiariesLink.getAttribute("aria-current")) === "page" &&
        (await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)),
      "on a phone the Beneficiaries page fits the width, gold in the account pages' strip",
    );
    await shot(page, "beneficiaries-phone");
    await page.setViewportSize({ width: 1440, height: 1000 });
    await shot(page, "beneficiaries-saved");

    // ---------------------------------------------------------------------
    // Tax Center (roadmap item 26): the fiscal year from the ledgers, the
    // W-8BEN, the reconciled accounting CSV and the Constancia preview
    // ---------------------------------------------------------------------
    const fiscalYear = Number(mexicoMonth(new Date().toISOString()).slice(0, 4));
    const taxes = (await asClient("/api/v1/wealth/impuestos")).body;
    const sales = cashLedger.filter((m) => m.tipo === "VENTA_MERCADO" && mexicoMonth(m.creado_en).startsWith(`${fiscalYear}-`));
    const purchases = cashLedger.filter((m) => m.tipo === "COMPRA_MERCADO" && mexicoMonth(m.creado_en).startsWith(`${fiscalYear}-`));
    const units = (rows) => rows.reduce((sum, m) => sum + toUnits(m.monto), 0n);
    check(
      taxes.summary.year === fiscalYear &&
        taxes.summary.in_progress &&
        taxes.summary.market.sells === sales.length &&
        taxes.summary.market.buys === purchases.length &&
        toUnits(taxes.summary.market.proceeds) === units(sales) &&
        toUnits(taxes.summary.market.bought) === units(purchases) &&
        isZero(taxes.summary.market.commissions) &&
        taxes.summary.fixed_term.contracts_open === 1 &&
        isZero(taxes.summary.isr.withheld),
      `the ${fiscalYear} summary matches the cash ledger: ${sales.length} sale(s), ${purchases.length} purchase(s), zero commissions, the fixed-term contract still open, nothing withheld`,
    );
    await openTab(page, "Taxes");
    const taxSummary = page.getByTestId("tax-summary");
    await taxSummary.waitFor();
    check(
      (await page.getByTestId("tax-proceeds").innerText()).includes(formatMoney(taxes.summary.market.proceeds)) &&
        (await page.getByTestId("tax-cost-basis").innerText()).includes(formatMoney(taxes.summary.market.cost_basis)) &&
        (await page.getByTestId("tax-withheld").innerText()).includes("$0.00") &&
        (await page.getByTestId("tax-estimated").innerText()).includes(formatMoney(taxes.summary.isr.estimated.total)) &&
        (await page.getByTestId("tax-year").inputValue()) === String(fiscalYear) &&
        (await page.getByTestId("tax-asset-row").count()) === taxes.summary.market.by_asset.length,
      "the summary shows the year's gross sales, cost basis, ISR withheld ($0.00), the estimate and each asset",
    );
    check(
      (await page.getByTestId("fiscal-rfc").innerText()) === taxes.profile.rfc &&
        (await page.getByTestId("fiscal-residence").innerText()) === "México" &&
        (await page.getByTestId("w8ben-panel").getAttribute("data-status")) === "pending" &&
        (await page.getByTestId("w8ben-status").textContent()) === "Not certified" &&
        (await page.getByTestId("w8ben-us-rate").innerText()) === "30%",
      "fiscal status: the RFC on file, residence in México, no W-8BEN yet (US dividends at 30%)",
    );
    await shot(page, "taxes");

    // Certify the W-8BEN: Part I from the file, the three statements, the signature.
    await page.getByTestId("w8ben-certify").click();
    const w8Dialog = page.getByTestId("w8ben-dialog");
    const details = w8Dialog.getByTestId("w8ben-step-details");
    await details.waitFor();
    const detailsText = await details.innerText();
    check(
      detailsText.includes(taxes.profile.name) && detailsText.includes(taxes.profile.rfc) && detailsText.includes(taxes.profile.address),
      "the W-8BEN's Part I is the verified file: legal name, RFC as the foreign tax number, permanent address",
    );
    const continueButton = w8Dialog.getByTestId("w8ben-continue");
    await continueButton.click();
    await w8Dialog.getByTestId("w8ben-step-statements").waitFor();
    check(!(await continueButton.isEnabled()), "the statements must all be confirmed to continue");
    for (const box of ["w8ben-check-beneficial-owner", "w8ben-check-not-us-person", "w8ben-check-treaty-resident"]) {
      await w8Dialog.getByTestId(box).check();
    }
    await continueButton.click();
    const signatureInput = w8Dialog.getByTestId("w8ben-signature");
    const signButton = w8Dialog.getByTestId("w8ben-sign");
    await signatureInput.fill("Somebody Else");
    check(
      (await w8Dialog.getByTestId("w8ben-signature-match").getAttribute("data-matches")) === "false" &&
        !(await signButton.isEnabled()),
      "a signature that is not the legal name cannot sign",
    );
    await signatureInput.fill(`  ${taxes.profile.name.toLowerCase()} `);
    check(
      (await w8Dialog.getByTestId("w8ben-signature-match").getAttribute("data-matches")) === "true" &&
        (await signButton.isEnabled()),
      "the legal name, typed in lower case with spaces, matches as the server compares it",
    );
    await shot(page, "w8ben-sign", { fullPage: false });
    await signButton.click();
    const done = w8Dialog.getByTestId("w8ben-step-done");
    await done.waitFor();
    const validUntil = `Dec 31, ${fiscalYear + 3}`;
    check(
      (await done.innerText()).includes("Version 1") && (await done.innerText()).includes(`valid through ${validUntil}`),
      `certified: version 1, valid through ${validUntil} (the end of the third year after signing)`,
    );
    await w8Dialog.getByTestId("w8ben-done").click();
    await w8Dialog.waitFor({ state: "detached" });
    check(
      (await page.getByTestId("w8ben-panel").getAttribute("data-status")) === "active" &&
        (await page.getByTestId("w8ben-us-rate").innerText()) === "10%" &&
        (await page.getByTestId("w8ben-validity").innerText()).includes(`Valid through ${validUntil}`) &&
        (await page.getByTestId("w8ben-chain").getAttribute("data-verified")) === "true",
      "the W-8BEN is active: US dividends at the treaty's 10%, valid through the date, its chain verified",
    );
    const renewal = await asClient("/api/v1/wealth/impuestos/w8ben", {
      method: "POST",
      body: JSON.stringify({ beneficial_owner: true, not_us_person: true, treaty_resident: true, signature: taxes.profile.name }),
    });
    const w8Events = (await asClient("/api/v1/auth/security/events")).body.eventos.filter((e) => e.evento === "W8BEN_CERTIFICADO");
    check(
      renewal.status === 201 &&
        renewal.body.current.version === 2 &&
        renewal.body.chain_verified &&
        JSON.stringify(w8Events.map((e) => e.detalle)) === JSON.stringify(["2", "1"]),
      "a renewal is version 2, chained; both certifications are in the security log",
    );

    // The accounting CSV: reconciled with the summary, then saved.
    const [csvFile] = await Promise.all([page.waitForEvent("download"), page.getByTestId("tax-csv").click()]);
    await page.getByTestId("tax-csv-verified").waitFor();
    const csvPath = `${OUT}/${csvFile.suggestedFilename()}`;
    await csvFile.saveAs(csvPath);
    const csvText = readFileSync(csvPath, "utf8");
    const csvRows = parseCsv(csvText);
    const servedCsv = await (
      await fetch(`${API}/api/v1/wealth/impuestos/export.csv?year=${fiscalYear}`, {
        headers: { authorization: `Bearer ${accountToken}` },
      })
    ).text();
    const yearMovements = cashLedger.filter(
      (m) => mexicoMonth(m.creado_en).startsWith(`${fiscalYear}-`) && BALANCE_MOVEMENTS.includes(m.tipo),
    );
    check(
      csvFile.suggestedFilename() === `synthetic-capital-taxes-${fiscalYear}.csv` &&
        csvText === servedCsv &&
        JSON.stringify(csvRows[0]) === JSON.stringify(CSV_HEADER) &&
        csvRows.length - 1 === yearMovements.length &&
        (await page.getByTestId("tax-csv-verified").innerText()).includes(`${yearMovements.length} movement`),
      `the accounting CSV is saved only after it reconciles: ${yearMovements.length} movements, RFC 4180, exactly what the API serves`,
    );

    // The Constancia de Retenciones preview, printed like the Document Inbox.
    await page.getByTestId("tax-constancia").click();
    const constancia = page.getByTestId("constancia-viewer");
    await constancia.getByTestId("constancia-screen").waitFor();
    check(
      (await constancia.getByTestId("constancia-year").innerText()) === String(fiscalYear) &&
        (await constancia.getByTestId("constancia-withheld").innerText()).includes("$0.00") &&
        /^[0-9a-f]{64}$/.test((await constancia.getByTestId("constancia-reference").getAttribute("data-value")) ?? "") &&
        (await constancia.innerText()).includes("It is not a CFDI"),
      "the Constancia preview shows the fiscal year, the ISR withheld, the summary's hash, and says it is not a CFDI",
    );
    await shot(page, "constancia", { fullPage: false });
    await page.emulateMedia({ media: "print" });
    const taxPaper = await page.evaluate(() => {
      const copy = document.querySelector("body > .document-print-copy");
      return {
        copy: copy ? getComputedStyle(copy).display : null,
        dialog: getComputedStyle(document.querySelector("dialog")).display,
        paper: copy ? getComputedStyle(copy.querySelector(".print-document")).backgroundColor : null,
        hash: copy?.querySelector("[data-testid=constancia-reference]")?.textContent ?? "",
      };
    });
    const constanciaPdf = await page.pdf({ path: `${OUT}/constancia-de-retenciones.pdf`, format: "A4" });
    await page.emulateMedia({ media: null });
    check(
      taxPaper.copy === "block" &&
        taxPaper.dialog === "none" &&
        taxPaper.paper === "rgb(255, 255, 255)" &&
        /^[0-9a-f]{64}$/.test(taxPaper.hash) &&
        constanciaPdf.subarray(0, 5).toString() === "%PDF-",
      "printed, the Constancia is the paper copy alone, its hash in full, and saves as PDF",
    );
    await page.keyboard.press("Escape");
    await constancia.waitFor({ state: "detached" });

    // On a phone: the account pages as a 2×2 strip, Taxes gold; the page fits.
    await page.setViewportSize({ width: 390, height: 844 });
    const taxesLink = page.getByRole("navigation", { name: "Account pages" }).getByRole("link", { name: "Taxes" });
    await taxesLink.waitFor();
    check(
      (await taxesLink.getAttribute("aria-current")) === "page" &&
        (await settledStyle(taxesLink, "backgroundColor")) === "rgb(234, 179, 8)" &&
        (await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)),
      "on a phone the Tax Center fits the width, Taxes gold in the account pages' strip",
    );
    await shot(page, "taxes-phone");
    await page.setViewportSize({ width: 1440, height: 1000 });

    // ---------------------------------------------------------------------
    // Savings plans (roadmap item 4): the same purchase every week, two weeks
    // or month, the first at once; listed on the Vault with Pause, Resume and
    // Cancel, live over the vault's WebSocket
    // ---------------------------------------------------------------------
    await page.goto(`${BASE}/dashboard/asset/AUG1`);
    await page.getByTestId("asset-detail").waitFor();
    const cashBeforePlan = (await asClient("/api/v1/tablero")).body.boveda.saldo_disponible;
    await page.getByTestId("asset-buy").click();
    await chooseType("Savings plan");
    await drawer.getByTestId("order-frequency").getByText("Weekly", { exact: true }).click();
    // More than the cash: the first purchase, due now, would be skipped; the drawer says so before anything is sent.
    await typeKeys(["9", "9", "9", "9", "9", "9", "9"]);
    const planCashNote = drawer.getByTestId("order-plan-cash");
    await planCashNote.waitFor();
    check(
      flat(await planCashNote.innerText()) ===
        `The first purchase is due now: with ${formatMoney(cashBeforePlan)} available, it would be skipped.` &&
        (await confirmButton.isEnabled()),
      "a plan larger than the cash can still be created, but the drawer warns that its first purchase would be skipped",
    );
    for (let i = 0; i < 7; i++) await drawer.getByRole("button", { name: "Delete last digit" }).click();
    await typeKeys(["5", "0", "0"]);
    const planSummary = flat(await summary.innerText());
    check(
      (await planCashNote.count()) === 0 &&
        planSummary.includes("Each purchase $500.00") &&
        planSummary.includes("Frequency Weekly") &&
        planSummary.includes("First purchase Now") &&
        planSummary.includes("Then Every week · 9:00 AM") &&
        planSummary.includes("About a month $2,166.67") &&
        (await footnote.innerText()).includes("9:00 a.m. Mexico City time"),
      "a weekly 500.00 plan of gold: the first purchase now, then every week at 9:00 AM, about $2,166.67 a month",
    );
    const sentBeforePlan = ordersSent;
    await confirmButton.click();
    const planReceipt = drawer.getByTestId("order-receipt");
    await planReceipt.waitFor();
    const planCreated = (await asClient("/api/v1/market/savings-plans")).body.plans[0];
    check(
      (await planReceipt.getAttribute("data-outcome")) === "plan" &&
        (await planReceipt.innerText()).includes("Savings plan created") &&
        (await planReceipt.innerText()).includes("Buys $500.00 of AUG1 every week.") &&
        flat(await drawer.getByTestId("receipt-first-purchase").innerText()) === "First purchase 0.31 AUG1 at $1,612.40" &&
        flat(await drawer.getByTestId("receipt-next-purchase").innerText()) ===
          `Next purchase ${formatPlanDate(planCreated.next_execution_at)}` &&
        (await planReceipt.innerText()).includes("$499.84") &&
        ordersSent === sentBeforePlan + 1,
      `Create Savings Plan: the first purchase at once (0.31 AUG1 for $499.84), the next one ${formatPlanDate(planCreated.next_execution_at)}`,
    );
    check(
      planCreated.status === "active" &&
        planCreated.frequency === "weekly" &&
        Date.parse(planCreated.next_execution_at) > Date.now() &&
        new Date(planCreated.next_execution_at).getUTCHours() === 15 &&
        Date.parse(planCreated.upcoming[1]) - Date.parse(planCreated.upcoming[0]) === 7 * 86_400_000,
      "the backend owns the calendar: the next purchase a week from today at 9:00 AM Mexico City (15:00 UTC), then weekly",
    );
    await shot(page, "checkout-plan-receipt");
    await drawer.getByRole("button", { name: "Done" }).click();
    await drawer.waitFor({ state: "detached" });
    await page.getByTestId("asset-detail").getByText("0.31 AUG1", { exact: true }).waitFor({ timeout: 5_000 });
    check(true, "the first purchase reaches the asset's page through the vault's WebSocket: 0.31 AUG1");

    // The Vault: the plan after the open orders and before the assets.
    await openTab(page, "Vault");
    const plansPanel = page.getByTestId("savings-plans");
    const planRows = plansPanel.getByTestId("savings-plan");
    const goldPlan = planRows.and(page.locator(`[data-plan-id="${planCreated.plan_id}"]`));
    await goldPlan.waitFor({ timeout: 5_000 });
    check(
      (await planRows.count()) === 1 &&
        (await goldPlan.getAttribute("data-status")) === "active" &&
        (await goldPlan.getByTestId("savings-plan-cadence").innerText()) === "$500.00 weekly" &&
        (await goldPlan.getByTestId("savings-plan-next-column").innerText()) ===
          formatPlanDate(planCreated.next_execution_at) &&
        (await goldPlan.getByTestId("savings-plan-last").innerText()) ===
          `Bought 0.31 AUG1 at $1,612.40 on ${formatPlanDay(planCreated.last_purchase.executed_at)}` &&
        (await goldPlan.getByTestId("savings-plan-invested").innerText()) === "$499.84" &&
        (await plansPanel.getByTestId("savings-plans-count").innerText()) === "1 active" &&
        (await plansPanel.getByTestId("savings-plans-monthly").innerText()) === "$2,166.67" &&
        (await plansPanel.boundingBox()).y < (await page.getByTestId("asset-list").boundingBox()).y,
      "the Vault lists the plan before the assets: $500.00 weekly, its next date, the purchase made, $499.84 invested",
    );

    // A plan created on another device appears live, newest first: CETES, 100.00 a month (10 titles for 99.44).
    const planDevice = await apiLogin(email, password);
    const cetesPlan = await apiOrder(planDevice, {
      ticker: "CETES28",
      side: "buy",
      amount: "100.00",
      order_type: "savings_plan",
      frequency: "monthly",
    });
    check(
      cetesPlan.status === 201 &&
        cetesPlan.receipt.last_result === "executed" &&
        cetesPlan.receipt.last_purchase.quantity === "10.00000000" &&
        cetesPlan.receipt.last_purchase.amount === "99.44000000",
      `a monthly 100.00 CETES plan from another device buys 10 titles for 99.44 at once (${cetesPlan.status})`,
    );
    const cetesRow = planRows.and(page.locator(`[data-plan-id="${cetesPlan.receipt.plan_id}"]`));
    await cetesRow.waitFor({ timeout: 5_000 });
    check(
      (await planRows.count()) === 2 &&
        (await planRows.first().getAttribute("data-plan-id")) === cetesPlan.receipt.plan_id &&
        (await cetesRow.getByTestId("savings-plan-cadence").innerText()) === "$100.00 monthly" &&
        (await plansPanel.getByTestId("savings-plans-count").innerText()) === "2 active" &&
        (await plansPanel.getByTestId("savings-plans-monthly").innerText()) === "$2,266.67",
      "it appears live, newest first, and the month's total becomes $2,266.67",
    );
    await page.mouse.move(0, 0);
    await shot(page, "savings-plans-desktop");

    // Pause: instant, no next date; Resume: the next date of its calendar.
    const plansNotice = plansPanel.getByTestId("savings-plans-notice");
    await goldPlan.getByTestId("savings-plan-pause").click();
    await goldPlan.and(page.locator("[data-status=paused]")).waitFor({ timeout: 1_000 });
    await plansNotice.filter({ hasText: "Savings plan for AUG1 paused: no purchases until you resume it." }).waitFor();
    check(
      (await goldPlan.getByTestId("savings-plan-next-column").innerText()) === "Paused" &&
        (await goldPlan.getByTestId("savings-plan-resume").isVisible()) &&
        (await plansPanel.getByTestId("savings-plans-count").innerText()) === "1 active · 1 paused" &&
        (await plansPanel.getByTestId("savings-plans-monthly").innerText()) === "$100.00",
      "Pause shows at once: Paused, Resume offered, and the month's total counts only the active plan",
    );
    await goldPlan.getByTestId("savings-plan-resume").click();
    await plansNotice
      .filter({ hasText: `Savings plan for AUG1 resumed. Next purchase: ${formatPlanDate(planCreated.next_execution_at)}.` })
      .waitFor();
    check(
      (await goldPlan.getAttribute("data-status")) === "active" &&
        (await goldPlan.getByTestId("savings-plan-next-column").innerText()) ===
          formatPlanDate(planCreated.next_execution_at),
      "Resume brings back the next date of its calendar (it had not come yet)",
    );

    // On a phone the rows fit the screen, the actions included.
    await page.setViewportSize({ width: 390, height: 844 });
    await plansPanel.scrollIntoViewIfNeeded();
    check(
      (await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)) &&
        (await goldPlan.getByTestId("savings-plan-pause").isVisible()) &&
        (await goldPlan.getByTestId("savings-plan-cancel").isVisible()) &&
        (await goldPlan.getByTestId("savings-plan-next").innerText()).startsWith("Next "),
      "on a phone the savings plans fit the width, Pause and Cancel included, the next date under the plan",
    );
    await shot(page, "savings-plans-mobile");
    await page.setViewportSize({ width: 1440, height: 1000 });

    // Cancel is final: it asks once more, in place. Keep keeps it.
    await cetesRow.getByTestId("savings-plan-cancel").click();
    const keep = cetesRow.getByTestId("savings-plan-cancel-keep");
    await keep.waitFor();
    check(
      (await cetesRow.innerText()).includes("Cancel for good?") &&
        (await page.evaluate(() => document.activeElement?.getAttribute("data-testid"))) === "savings-plan-cancel-keep",
      "Cancel asks once more in place, the focus on Keep",
    );
    await keep.click();
    await cetesRow.getByTestId("savings-plan-cancel").click();
    await cetesRow.getByTestId("savings-plan-cancel-confirm").click();
    await cetesRow.waitFor({ state: "detached", timeout: 2_000 });
    await plansNotice
      .filter({ hasText: "Savings plan for CETES28 cancelled. What it bought stays in your vault." })
      .waitFor();
    const cancelledCetes = await fetch(`${API}/api/v1/market/savings-plans/${cetesPlan.receipt.plan_id}/resume`, {
      method: "POST",
      headers: { authorization: `Bearer ${planDevice}` },
    });
    check(
      (await planRows.count()) === 1 &&
        cancelledCetes.status === 409 &&
        (await cancelledCetes.json()).error.codigo === "PLAN_CANCELADO",
      "Yes, cancel removes it at once, for good: resuming it from another device is refused (409)",
    );
    await goldPlan.getByTestId("savings-plan-cancel").click();
    await goldPlan.getByTestId("savings-plan-cancel-confirm").click();
    await goldPlan.waitFor({ state: "detached", timeout: 2_000 });
    await plansPanel.waitFor({ state: "detached", timeout: 10_000 });
    check(true, "with no plans left, the Savings plans section leaves the Vault once its notice is read");
    const afterPlans = (await asClient("/api/v1/tablero")).body.boveda.saldo_disponible;
    check(
      toUnits(afterPlans) === toUnits(cashBeforePlan) - toUnits("499.84") - toUnits("99.44"),
      "the two first purchases (499.84 and 99.44) are all the plans charged; cancelling moves no money",
    );
    // What the plans bought is the client's like any purchase: sold back at the same prices, the cash is
    // exactly what it was (and the second pass starts, as before, with nothing in the market).
    const goldBack = await apiOrder(planDevice, { ticker: "AUG1", side: "sell", amount: "499.84" });
    const cetesBack = await apiOrder(planDevice, { ticker: "CETES28", side: "sell", amount: "99.44" });
    const cashAfterSales = (await asClient("/api/v1/tablero")).body.boveda.saldo_disponible;
    check(
      goldBack.status === 201 &&
        goldBack.receipt.quantity === "0.31000000" &&
        cetesBack.status === 201 &&
        cetesBack.receipt.quantity === "10.00000000" &&
        toUnits(cashAfterSales) === toUnits(cashBeforePlan),
      "the plans' 0.31 AUG1 and 10 CETES28 sell like any position: the cash is back to the cent",
    );

    // ---------------------------------------------------------------------
    // Security (roadmap items 20 and 21): devices, the password, the log
    // ---------------------------------------------------------------------
    // A phone and a work laptop sign in through the API: sessions of their own.
    const phoneToken = await apiLogin(email, password, IPHONE_UA);
    const laptopToken = await apiLogin(email, password, WINDOWS_UA);
    await openTab(page, "Security");
    const securityLink = mainNav(page).getByRole("link", { name: "Security", exact: true });
    check(
      (await settledStyle(securityLink, "color")) === "rgb(234, 179, 8)",
      "Security sits in the sidebar's Account group, Sovereign Gold while current",
    );
    const sessionRows = page.getByTestId("session-row");
    const listed = await (
      await fetch(`${API}/api/v1/auth/sessions`, { headers: { authorization: `Bearer ${laptopToken}` } })
    ).json();
    await sessionRows.nth(listed.sesiones.length - 1).waitFor();
    const devices = await sessionRows.getByTestId("session-device").allInnerTexts();
    const badge = await page
      .getByTestId("current-device-badge")
      .evaluate((el) => ({ background: getComputedStyle(el).backgroundColor, radius: getComputedStyle(el).borderRadius }));
    check(
      (await sessionRows.count()) === listed.sesiones.length &&
        (await sessionRows.first().getAttribute("data-session-id")) === sidOf(await pageToken(page)) &&
        (await page.getByTestId("current-device-badge").count()) === 1 &&
        (await sessionRows.first().getByTestId("current-device-badge").isVisible()) &&
        badge.background === "rgb(234, 179, 8)" &&
        badge.radius === "0px" &&
        devices.includes("Safari on iPhone") &&
        devices.includes("Edge on Windows"),
      `Security lists all ${listed.sesiones.length} sessions: this browser first, with the only gold, square "Current device" badge; the phone and the laptop by name`,
    );
    check(
      await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      "the Security page fits the desktop width",
    );
    await page.mouse.move(0, 0);
    await shot(page, "security-desktop");

    // Revoke: the phone is out at once, the laptop stays.
    await sessionRows.filter({ hasText: "Safari on iPhone" }).getByTestId("revoke-session").click();
    await page.getByTestId("sessions-status").filter({ hasText: "Safari on iPhone was signed out." }).waitFor();
    check(
      (await apiStatus("/api/v1/yo", phoneToken)) === 401 &&
        (await apiStatus("/api/v1/yo", laptopToken)) === 200 &&
        !(await sessionRows.getByTestId("session-device").allInnerTexts()).includes("Safari on iPhone"),
      "Revoke signs the phone out at once (its token answers 401) and leaves the laptop signed in",
    );

    // The panic button: one confirmation, with the count, then every other device is out.
    const others = (await sessionRows.count()) - 1;
    const othersText = others === 1 ? "1 other device" : `${others} other devices`;
    await page.getByTestId("panic-button").click();
    check(
      (await page.getByTestId("panic-confirm-text").innerText()) === `Sign out ${othersText} now?`,
      `"Sign out all other devices" asks once before acting: "Sign out ${othersText} now?"`,
    );
    await page.getByTestId("panic-zone").scrollIntoViewIfNeeded();
    await shot(page, "security-panic-confirm", { fullPage: false });
    await page.getByTestId("panic-confirm").click();
    await page.getByTestId("no-other-devices").waitFor();
    check(
      (await sessionRows.count()) === 1 &&
        (await page.getByTestId("sessions-status").innerText()).startsWith(
          others === 1 ? "1 other device was signed out." : `${others} other devices were signed out.`,
        ) &&
        (await apiStatus("/api/v1/yo", laptopToken)) === 401 &&
        (await page.getByTestId("panic-button").isDisabled()),
      `the other ${othersText} are signed out (the laptop's token answers 401); only this browser remains`,
    );

    // The inactivity timeout: saved on the server as soon as it is picked.
    const timeoutControl = page.getByTestId("timeout-control");
    const chosenTimeout = () => timeoutControl.locator("input:checked").getAttribute("value");
    check((await chosenTimeout()) === "30", "the inactivity timeout starts at 30 minutes");
    await timeoutControl.getByText("15 min", { exact: true }).click();
    await page
      .getByTestId("timeout-status")
      .filter({ hasText: "Saved: sessions now end after 15 min without activity." })
      .waitFor();
    await page.reload();
    await timeoutControl.waitFor();
    check((await chosenTimeout()) === "15", "15 minutes is saved on the server: it survives a reload");
    await timeoutControl.getByText("30 min", { exact: true }).click();
    await page.getByTestId("timeout-status").filter({ hasText: "after 30 min" }).waitFor();

    // The password: a wrong current one is refused (and counted); then the change.
    const newPassword = "hayek-road-to-serfdom-1944";
    expectFailure("/api/v1/auth/password/change");
    await page.fill("#current-password", "not-my-password-1926");
    await page.fill("#new-password", newPassword);
    await page.fill("#confirm-password", newPassword);
    check(
      JSON.stringify(await page.getByTestId("password-rule").evaluateAll((rules) => rules.map((r) => r.dataset.met))) ===
        JSON.stringify(["true", "true", "true"]),
      "the checklist shows the new password meets the rules: length, different, confirmed",
    );
    await page.getByTestId("password-submit").click();
    await page.getByTestId("password-error").filter({ hasText: "Your current password is not correct." }).waitFor();
    check(
      (await page.inputValue("#current-password")) === "" && (await page.inputValue("#new-password")) === newPassword,
      "a wrong current password is refused, and only that field clears",
    );
    await page.fill("#current-password", password);
    await page.getByTestId("password-submit").click();
    await page
      .getByTestId("password-status")
      .filter({ hasText: "Password changed. No other device was signed in." })
      .waitFor();
    const formerPassword = password;
    password = newPassword;
    const oldSignIn = await fetch(`${API}/api/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email, password: formerPassword }),
    });
    check(
      oldSignIn.status === 401 && (await apiStatus("/api/v1/yo", await apiLogin(email, password))) === 200,
      "after the change the old password no longer signs in, and the new one does",
    );

    // The log: what just happened, newest first, from this device.
    const logTitles = await page.getByTestId("security-event-title").allInnerTexts();
    check(
      [
        "Password changed",
        "Password change refused",
        "Inactivity timeout changed",
        "Signed out all other devices",
        "Device signed out",
        "Signed in",
        "Beneficiaries updated",
        "Identity confirmed",
        "W-8BEN certified",
      ].every((title) => logTitles.includes(title)) && logTitles[0] === "Password changed",
      "the security log shows the change, the refused try, the timeout, both sign-outs, the sign-ins, the new beneficiaries, the identity check and the W-8BEN, newest first",
    );
    await page.mouse.move(0, 0);
    await shot(page, "security-after-changes");

    // Two-step verification from Security, and the inactivity sign-out, on a
    // phone, for a client of its own. A clock the test can move: the timeout
    // passes without waiting for it.
    const guarded = `e2e-${runId}-2fa@example.mx`;
    check((await apiRegister(guarded, password)) === 201, "another client registers through the API");
    const phoneContext = await browser.newContext({ viewport: { width: 390, height: 844 }, colorScheme });
    const phonePage = await phoneContext.newPage();
    // After the inactivity sign-out, a request already on its way may still meet the ended session (401).
    let idleSignedOut = false;
    phonePage.on("console", (m) => {
      if (m.type() !== "error" || (idleSignedOut && /status of 401/.test(m.text()))) return;
      errors.push(`[${colorScheme} phone] ${m.text()} (${m.location().url || "no url"})`);
    });
    phonePage.on("pageerror", (e) => errors.push(`[${colorScheme} phone] ${e.message}`));
    await phonePage.clock.install();
    await phonePage.goto(`${BASE}/auth/login`);
    await phonePage.fill("#email", guarded);
    await phonePage.fill("#password", password);
    await phonePage.click("button[type=submit]");
    await phonePage.waitForURL("**/dashboard/verify");
    const lockButton = phonePage.getByTestId("header-security");
    await lockButton.click();
    await phonePage.waitForURL("**/dashboard/security");
    await phonePage.getByTestId("session-row").first().waitFor();
    check(
      (await lockButton.getAttribute("aria-current")) === "page" &&
        (await settledStyle(lockButton, "backgroundColor")) === "rgb(234, 179, 8)" &&
        (await mainNav(phonePage).getByRole("link").count()) === 5 &&
        (await mainNav(phonePage).getByRole("link", { name: "Security" }).count()) === 0,
      "on a phone, Security opens from the header's lock (gold while current); the tab bar keeps its five tabs",
    );
    check(
      await phonePage.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      "the Security page fits a phone's width",
    );
    await shot(phonePage, "security-mobile");

    await phonePage.getByTestId("two-step-setup").click();
    await phonePage.waitForURL("**/auth/mfa-setup");
    const authenticatorKey = base32Decode(await phonePage.getByTestId("mfa-secret").innerText());
    const enrolledStep = Math.floor(Date.now() / 30_000);
    await phonePage.fill("#totp-code", totpCode(authenticatorKey, enrolledStep));
    await phonePage.getByRole("button", { name: "Turn on two-step verification" }).click();
    await phonePage.waitForURL("**/dashboard/security");
    await phonePage.getByText("Every sign-in now asks for a code from your authenticator app.").waitFor();
    await phonePage.locator("#password-code").waitFor();
    check(
      (await phonePage.getByTestId("two-step-status").getByText("On", { exact: true }).isVisible()) &&
        /two-step/i.test(await phonePage.getByTestId("session-row").first().innerText()) &&
        (await phonePage.getByTestId("session-row").count()) === 1,
      "two-step verification turned on from Security: back there, On, a new session that verified the code, and the password form asks for it",
    );

    await phonePage.getByTestId("timeout-control").getByText("5 min", { exact: true }).click();
    await phonePage.getByTestId("timeout-status").filter({ hasText: "after 5 min" }).waitFor();
    const guardedToken = await pageToken(phonePage);
    // With two-step verification on, the security check before a sensitive action asks for the
    // authenticator code, not the password (step-up, roadmap item 22), and a code counts once.
    const guardedStepUp = async (body) => {
      const response = await fetch(`${API}/api/v1/auth/step-up`, {
        method: body ? "POST" : "GET",
        headers: { authorization: `Bearer ${guardedToken}`, ...(body ? { "content-type": "application/json" } : {}) },
        body: body ? JSON.stringify(body) : undefined,
      });
      return { status: response.status, body: await response.json() };
    };
    const twoStepCheck = await guardedStepUp();
    const passwordOnly = await guardedStepUp({ password });
    const freshStep = Math.max(Math.floor(Date.now() / 30_000), enrolledStep + 1);
    const byCode = await guardedStepUp({ codigo: totpCode(authenticatorKey, freshStep) });
    const codeAgain = await guardedStepUp({ codigo: totpCode(authenticatorKey, freshStep) });
    check(
      twoStepCheck.body.metodo === "TOTP" &&
        twoStepCheck.body.valido_hasta === null &&
        passwordOnly.status === 422 &&
        passwordOnly.body.error.codigo === "CODIGO_MFA_REQUERIDO" &&
        byCode.status === 200 &&
        byCode.body.metodo === "TOTP" &&
        byCode.body.segundos === 300 &&
        codeAgain.status === 422 &&
        codeAgain.body.error.codigo === "CODIGO_MFA_INVALIDO" &&
        codeAgain.body.error.intentos_restantes === 4,
      "two-step on: the check asks for the app's code (the password alone is refused, uncounted), a code works once, a reused one is a counted miss",
    );
    await phonePage.clock.fastForward("04:00");
    check(
      new URL(phonePage.url()).pathname === "/dashboard/security",
      "four minutes without a tap: still signed in",
    );
    idleSignedOut = true;
    await phonePage.clock.fastForward("02:00");
    await phonePage.waitForURL("**/auth/login?expired=idle");
    await phonePage.getByText("Signed out after inactivity").waitFor();
    let guardedStatus = 0;
    for (let i = 0; i < 20 && guardedStatus !== 401; i++) {
      guardedStatus = await apiStatus("/api/v1/yo", guardedToken);
      if (guardedStatus !== 401) await new Promise((resolve) => setTimeout(resolve, 250));
    }
    check(
      guardedStatus === 401,
      "six minutes without a tap at a 5-minute timeout: signed out here and on the server, and the sign-in page says why",
    );
    await shot(phonePage, "security-idle-signed-out", { fullPage: false });
    await phoneContext.close();

    // Sign out.
    await page.getByRole("button", { name: "Sign out", exact: true }).click();
    await page.waitForURL("**/auth/login");
    check(true, "sign out returns to login");
    await context.close();
  }
} finally {
  await browser.close();
}

// No console error is tolerated except the browser's own request for a
// favicon the app does not ship (a 404 on /favicon.ico, not app code) and one
// "Failed to load resource" line per API call the test failed on purpose.
const benign = (e) => /Failed to load resource: .*404/.test(e) && /\/favicon\.ico\)$/.test(e);
const excused = [...deliberateFailures];
const deliberate = (e) => {
  const i = excused.findIndex((path) => /Failed to load resource/.test(e) && e.endsWith(`${path})`));
  if (i !== -1) excused.splice(i, 1);
  return i !== -1;
};
const favicons = errors.filter(benign).length;
const relevant = errors.filter((e) => !benign(e) && !deliberate(e));
const excusedCount = deliberateFailures.length - excused.length;
if (favicons + excusedCount > 0) {
  console.log(`(ignored ${favicons} favicon 404 and ${excusedCount} deliberate API failure message(s))`);
}
if (relevant.length) {
  console.error("Console errors:\n" + relevant.join("\n"));
  process.exit(1);
}
console.log("All end-to-end checks passed.");
