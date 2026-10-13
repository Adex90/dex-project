/**
 * End-to-end smoke test: drives the real dashboard in Chromium against a
 * running backend (MODO_DEMO=true) and Next.js server.
 *
 *   FRONTEND_URL=http://localhost:3000 node e2e/smoke.mjs [screenshot-dir]
 *
 * Requires the `playwright` package (resolved from NODE_PATH or a global
 * install) and a Chromium build (PLAYWRIGHT_BROWSERS_PATH).
 */
import { createRequire } from "node:module";

// createRequire honors NODE_PATH (ESM imports do not), so a global install works.
const { chromium } = createRequire(import.meta.url)("playwright");

const BASE = process.env.FRONTEND_URL ?? "http://localhost:3000";
const OUT = process.argv[2] ?? ".";
const email = `e2e-${Date.now()}@example.mx`;
const password = "austrian-school-1926";
const errors = [];

function check(condition, message) {
  if (!condition) throw new Error(`FAILED: ${message}`);
  console.log(`ok - ${message}`);
}

const browser = await chromium.launch();
try {
  for (const colorScheme of ["light", "dark"]) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme });
    const page = await context.newPage();
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    page.on("pageerror", (e) => errors.push(e.message));

    if (colorScheme === "dark") {
      // Second pass: sign in with the account created in the first pass.
      await page.goto(`${BASE}/auth/login`);
      await page.fill("#email", email);
      await page.fill("#password", password);
      await page.click("button[type=submit]");
      await page.waitForURL("**/dashboard");
      await page.getByTestId("vault-available").filter({ hasText: "15,000.00" }).waitFor();
      await page.getByRole("button", { name: "Verify My Merkle Proof" }).click();
      await page.getByTestId("proof-verdict").waitFor();
      await page.waitForTimeout(1500);
      await page.screenshot({ path: `${OUT}/dashboard-dark.png`, fullPage: true });
      check(true, "dark mode dashboard renders after sign-in");
      await context.close();
      continue;
    }

    // Landing page with live reserve.
    await page.goto(BASE);
    await page.getByText("Reserve ratio").waitFor();
    await page.screenshot({ path: `${OUT}/landing.png`, fullPage: true });
    check(await page.getByText("100%").first().isVisible(), "landing shows the live 100% reserve ratio");

    // Protected route redirects to login.
    await page.goto(`${BASE}/dashboard`);
    await page.waitForURL("**/auth/login");
    check(true, "dashboard redirects unauthenticated visitors to login");

    // Registration.
    await page.goto(`${BASE}/auth/register`);
    await page.fill("#nombre", "Murray");
    await page.fill("#paterno", "Rothbard");
    await page.fill("#email", email);
    await page.fill("#password", password);
    await page.fill("#confirm", password);
    await page.click("button[type=submit]");
    await page.waitForURL("**/dashboard");
    await page.getByTestId("vault-available").filter({ hasText: "0.00" }).waitFor();
    check(await page.getByText("100% Fully Backed in Custody").isVisible(), "vault shows the 100% backed badge");

    // Demo SPEI deposit.
    await page.getByRole("button", { name: /Add funds/ }).click();
    await page.fill("#demo-amount", "25,000");
    await page.getByRole("button", { name: "Deposit", exact: true }).click();
    await page.getByTestId("vault-available").filter({ hasText: "25,000.00" }).waitFor();
    check(true, "simulated deposit credits the vault");

    // IFPE -> SOFOM bridge.
    await page.fill("#bridge-amount", "10000");
    await page.getByText("182d").click();
    await page.getByText("You receive").waitFor();
    check(await page.getByText("366.53").first().isVisible(), "bridge preview shows ACT/360 interest (366.53)");
    await page.getByLabel(/no early withdrawal/).check();
    await page.getByRole("button", { name: /Lock funds/ }).click();
    await page.getByText(/Contract #\d+ created/).waitFor();
    await page.getByTestId("vault-available").filter({ hasText: "15,000.00" }).waitFor();
    await page.getByTestId("sofom-locked").filter({ hasText: "10,000.00" }).waitFor();
    check(true, "bridge moves 10,000 from the vault into a mutuum contract");

    // Proof of Reserve.
    await page.getByRole("button", { name: "Verify My Merkle Proof" }).click();
    const verdict = page.getByTestId("proof-verdict");
    await verdict.waitFor({ timeout: 15_000 });
    check((await verdict.innerText()).includes("Verified"), "browser verifies the Merkle proof against the published root");
    await page.getByText("Server check: agrees").waitFor();
    check(true, "server-side verification agrees");

    // Immutable ledger feed.
    const rows = page.locator("tbody tr");
    check((await rows.count()) === 2, "ledger feed lists the deposit and the transfer to fixed term");
    check(await page.getByText("To fixed term").isVisible(), "ledger labels the bridge movement");
    await rows.first().click();
    await page.getByText("Previous hash (chain link)").waitFor();
    check(true, "ledger row expands to show the hash chain");
    await page.screenshot({ path: `${OUT}/dashboard-light.png`, fullPage: true });

    // A dishonest server: a private root (differs from the public copy of the
    // cut) or a leaf for another account must both fail in the browser.
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
    issues = await tamper("**/api/v1/reserva/prueba", (p) => ({
      ...p,
      hoja: { ...p.hoja, cuenta_id: "00000000-0000-4000-8000-000000000000" },
    }));
    check(issues.includes("different account"), "a proof for another account fails verification");

    // Overdraft attempt through the bridge is blocked client-side.
    await page.getByRole("button", { name: "Make another investment" }).click();
    await page.fill("#bridge-amount", "15000.01");
    check(await page.getByText("More than your available vault balance.").isVisible(), "bridge blocks amounts above the balance");

    // Sign out.
    await page.getByRole("button", { name: /Sign out/ }).click();
    await page.waitForURL("**/auth/login");
    check(true, "sign out returns to login");
    await context.close();
  }
} finally {
  await browser.close();
}

const relevant = errors.filter((e) => !e.includes("favicon"));
if (relevant.length) {
  console.error("Console errors:\n" + relevant.join("\n"));
  process.exit(1);
}
console.log("All end-to-end checks passed.");
