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
 * First pass: landing -> sign-up -> KYC-locked dashboard -> full onboarding
 * (personal data, INE photos and details, biometric consent + liveness) ->
 * VERIFICADO -> deposit, fixed-term investment, Proof of Reserve, ledger,
 * moving between the dashboard's tabs (Vault, Discover, Account, Invest,
 * Reserve). It ends on asset pages opened by their address (AUG1, an unknown
 * ticker, an address that is not a ticker, a market without live prices) and
 * on Discover: the catalog, its search, and a first buy from it (a fraction
 * of STSLA), sold back from the Vault with "Max".
 * Second pass: signing in again lands directly on the Vault, which is live
 * over its WebSocket: a deposit made outside the browser reaches the screen
 * with REST blocked, a server restart (1001) reconnects, and a session the
 * server ends (4001) returns to sign-in. In between, trading with REST for
 * the vault blocked: a gold buy on another device appears in the asset list,
 * its row opens the asset's page, a buy whose answer is lost is retried with
 * the same idempotency key and fills once, refusals show their reason; then,
 * on a phone-sized screen (bottom tab bar instead of the sidebar, Discover
 * included), "Max" sells the whole position.
 * The theme is dark only ("Sovereign Gold"); the passes still differ in the
 * browser's preferred color scheme, which must not change it.
 */
import { createRequire } from "node:module";
import { mkdirSync } from "node:fs";

// Node >= 22.18 strips TypeScript types, so the app's own (framework-free)
// KYC library builds an identity that passes every frontend and backend check.
import { sampleIdentity, SAMPLE_IDENTITY_SPACE, todayInMexicoCity } from "../src/lib/kyc.ts";

// createRequire honors NODE_PATH (ESM imports do not), so a global install works.
const { chromium } = createRequire(import.meta.url)("playwright");

const BASE = process.env.FRONTEND_URL ?? "http://localhost:3000";
// The backend the frontend was built against (NEXT_PUBLIC_API_URL): for deposits made outside the browser.
const API = process.env.API_URL ?? "http://localhost:8080";
const OUT = process.argv[2] ?? ".";
mkdirSync(OUT, { recursive: true });

const runId = Date.now();
const email = `e2e-${runId}@example.mx`;
const password = "austrian-school-1926";
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

/** Signs in through the API, as another device would: a session of its own. */
async function apiLogin(emailAddress, secret) {
  const response = await fetch(`${API}/api/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ email: emailAddress, password: secret }),
  });
  const body = await response.json();
  if (body.estado !== "OK") throw new Error(`API sign-in failed: ${JSON.stringify(body)}`);
  return body.token;
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

/** Full-page screenshot once finite animations (step reveal, transitions) have finished. */
async function shot(page, name) {
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
  await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: true });
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
      await openTab(page, "Vault");
      await page.getByTestId("vault-total").filter({ hasText: "26,234" }).waitFor();
      await page.getByTestId("portfolio-line").waitFor();
      await shot(page, "vault-mobile");
      await openTab(page, "Reserve");
      await page.getByRole("button", { name: "Verify My Merkle Proof" }).click();
      await page.getByTestId("proof-verdict").waitFor({ timeout: 15_000 });
      await page.waitForTimeout(1500);
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
      await page.getByRole("button", { name: "Sell", exact: true }).click();
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
      // sale, then this pass's two gold buys and the sale.
      await openTab(page, "Account");
      await page.getByText("Market sale", { exact: true }).nth(1).waitFor();
      check(
        (await page.getByText("Market buy", { exact: true }).count()) === 3 &&
          (await page.getByText("Market sale", { exact: true }).count()) === 2,
        "the ledger lists three market buys and two market sales",
      );
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
    await page.waitForURL("**/auth/onboarding");
    await page.getByTestId("onboarding-step-1").waitFor();
    check(true, "sign-up lands on the onboarding wizard (step 1)");

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
    await page.waitForURL("**/auth/onboarding");
    const step1 = page.getByTestId("onboarding-step-1");
    await step1.waitFor();
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
    await shot(page, "onboarding-1-personal");
    await page.getByRole("button", { name: "Save and continue" }).click();

    // ---------------------------------------------------------------------
    // Step 2: both sides of the INE (two different JPEGs) and its details.
    // ---------------------------------------------------------------------
    const step2 = page.getByTestId("onboarding-step-2");
    await step2.waitFor();
    check(true, "step 1 saves without warnings and advances to the INE step");
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
    await page.setInputFiles("#ine-reverso-file", { name: "ine-reverso.jpg", mimeType: "image/jpeg", buffer: back });
    await step2.getByAltText("Captured front of your INE").waitFor();
    await step2.getByAltText("Captured back of your INE").waitFor();
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
    // Step 3: biometric consent, then the liveness check with the fake camera.
    // ---------------------------------------------------------------------
    const step3 = page.getByTestId("onboarding-step-3");
    await step3.waitFor();
    const start = step3.getByRole("button", { name: "Start liveness check" });
    check(await start.isDisabled(), "liveness check cannot start without biometric consent");
    await page.check("#consentimiento");
    check(await start.isEnabled(), "biometric consent enables the liveness check");
    await shot(page, "onboarding-3-consent");
    const verify = page.waitForResponse((r) => r.url().includes("/kyc/verificar") && r.request().method() === "POST", {
      timeout: 45_000,
    });
    // The file the page reloads right after the verification (GET /kyc/expediente).
    let verifySent = false;
    page.on("request", (r) => {
      if (r.url().includes("/kyc/verificar") && r.method() === "POST") verifySent = true;
    });
    const reloaded = page.waitForResponse(
      (r) => verifySent && r.url().includes("/kyc/expediente") && r.request().method() === "GET",
      { timeout: 60_000 },
    );
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

    // ---------------------------------------------------------------------
    // Step 4: status page.
    // ---------------------------------------------------------------------
    const step4 = page.getByTestId("onboarding-step-4");
    await step4.waitFor();
    const statusBadge = step4.getByTestId("kyc-status-badge");
    await statusBadge.and(page.locator("[data-status=VERIFICADO]")).waitFor();
    check(true, "status badge reads VERIFICADO");
    await step4.getByText("Your identity is verified").waitFor();
    check(await step4.getByText("Approved", { exact: true }).isVisible(), "last verification shows Approved");
    check(
      (await step4.getByText(/^Face match/).count()) === 0,
      "status page shows no face-match score or row (the API withholds it)",
    );
    await shot(page, "onboarding-4-status");
    await step4.getByRole("button", { name: "Go to dashboard" }).click();
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

    // Demo SPEI deposit.
    await page.getByRole("button", { name: /Add funds/ }).click();
    await page.fill("#demo-amount", "25,000");
    await page.getByRole("button", { name: "Deposit", exact: true }).click();
    await page.getByTestId("vault-available").filter({ hasText: "25,000.00" }).waitFor();
    check(true, "simulated deposit credits the vault");

    // IFPE -> SOFOM bridge.
    await openTab(page, "Invest");
    check(await page.locator("#bridge-amount").isEnabled(), "investment form is enabled once verified");
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

    // Proof of Reserve.
    await openTab(page, "Reserve");
    await page.getByRole("button", { name: "Verify My Merkle Proof" }).click();
    const proof = page.getByTestId("proof-verdict");
    await proof.waitFor({ timeout: 15_000 });
    check((await proof.innerText()).includes("Verified"), "browser verifies the Merkle proof against the published root");
    await page.getByText("Server check: agrees").waitFor();
    check(true, "server-side verification agrees");

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
    await page.getByRole("button", { name: "Verify My Merkle Proof" }).click();
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
    check(true, "the search finds assets by name, ticker or class, whatever the case");
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
    await page.getByTestId("asset-buy").click();
    const drawer = page.getByTestId("order-checkout");
    for (const key of ["1", "0", "0", "0"]) await drawer.getByRole("button", { name: key, exact: true }).click();
    await drawer.getByTestId("order-confirm").click();
    const firstReceipt = drawer.getByTestId("order-receipt");
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
    check((await search.inputValue()) === "tesla", "back on Discover, the search and its results are as they were left");
    await page.getByTestId("market-search-clear").click();
    await listing(TRENDING);

    // The Vault lists it; opened from there, the page is the Vault's, and Max sells it all.
    await openTab(page, "Vault");
    const teslaRow = page.getByTestId("asset-row").and(page.locator("[data-ticker=STSLA]"));
    await teslaRow.getByTestId("asset-row-link").click();
    await page.waitForURL("**/dashboard/asset/STSLA");
    await page.getByTestId("asset-detail").getByText("0.2264 STSLA", { exact: true }).waitFor();
    check(
      (await assetBack.getAttribute("href")) === "/dashboard" && /back to vault/i.test(await assetBack.innerText()),
      "opened from the Vault's holdings, the asset's Back link returns to the Vault",
    );
    await page.getByRole("button", { name: "Sell", exact: true }).click();
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

    // Sign out.
    await page.getByRole("button", { name: /Sign out/ }).click();
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
