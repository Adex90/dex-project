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
 * Light pass: landing -> sign-up -> KYC-locked dashboard -> full onboarding
 * (personal data, INE photos and details, biometric consent + liveness) ->
 * VERIFICADO -> deposit, fixed-term investment, Proof of Reserve, ledger.
 * Dark pass: signing in again lands directly on the dashboard.
 */
import { createRequire } from "node:module";
import { mkdirSync } from "node:fs";

// Node >= 22.18 strips TypeScript types, so the app's own (framework-free)
// KYC library builds an identity that passes every frontend and backend check.
import { sampleIdentity, SAMPLE_IDENTITY_SPACE, todayInMexicoCity } from "../src/lib/kyc.ts";

// createRequire honors NODE_PATH (ESM imports do not), so a global install works.
const { chromium } = createRequire(import.meta.url)("playwright");

const BASE = process.env.FRONTEND_URL ?? "http://localhost:3000";
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
      // Second pass: a verified client signs in and lands directly on the dashboard.
      await page.goto(`${BASE}/auth/login`);
      await page.fill("#email", email);
      await page.fill("#password", password);
      await page.click("button[type=submit]");
      await page.waitForURL("**/dashboard");
      check(true, "a verified client signs in straight to the dashboard (no onboarding detour)");
      await page.getByTestId("vault-available").filter({ hasText: "15,000.00" }).waitFor();
      await page.getByTestId("kyc-header-badge").and(page.locator("[data-status=VERIFICADO]")).waitFor();
      check((await page.getByTestId("kyc-banner").count()) === 0, "dark mode: no KYC banner for the verified client");
      await page.getByRole("button", { name: "Verify My Merkle Proof" }).click();
      await page.getByTestId("proof-verdict").waitFor({ timeout: 15_000 });
      await page.waitForTimeout(1500);
      await shot(page, "dashboard-dark");
      check(true, "dark mode dashboard renders after sign-in");
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
    await page.getByTestId("vault-available").filter({ hasText: "0.00" }).waitFor();
    check((await banner.getAttribute("data-status")) === "PENDIENTE", "dashboard shows the KYC banner (pending)");
    const headerBadge = page.getByTestId("kyc-header-badge");
    check(
      (await headerBadge.getAttribute("data-status")) === "PENDIENTE" && (await headerBadge.innerText()).includes("Pending"),
      "header badge reads Pending",
    );
    check(await page.getByText("100% Fully Backed in Custody").isVisible(), "vault shows the 100% backed badge");
    check(
      (await page.getByTestId("deposit-locked").getAttribute("aria-disabled")) === "true",
      "deposit control is disabled until verified",
    );
    // Playwright treats aria-disabled as disabled, so force the click to prove it does nothing.
    await page.getByTestId("deposit-locked").click({ force: true });
    check((await page.locator("#demo-amount").count()) === 0, "clicking the locked deposit control opens no deposit form");
    check(await page.locator("#bridge-amount").isDisabled(), "investment amount is disabled until verified");
    check(
      await page.getByRole("button", { name: /Lock funds/ }).isDisabled(),
      "investment submit is disabled until verified",
    );
    await shot(page, "dashboard-kyc-pending");

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
    check((await page.getByTestId("deposit-locked").count()) === 0, "deposit control is unlocked");

    // Demo SPEI deposit.
    await page.getByRole("button", { name: /Add funds/ }).click();
    await page.fill("#demo-amount", "25,000");
    await page.getByRole("button", { name: "Deposit", exact: true }).click();
    await page.getByTestId("vault-available").filter({ hasText: "25,000.00" }).waitFor();
    check(true, "simulated deposit credits the vault");

    // IFPE -> SOFOM bridge.
    check(await page.locator("#bridge-amount").isEnabled(), "investment form is enabled once verified");
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
    const proof = page.getByTestId("proof-verdict");
    await proof.waitFor({ timeout: 15_000 });
    check((await proof.innerText()).includes("Verified"), "browser verifies the Merkle proof against the published root");
    await page.getByText("Server check: agrees").waitFor();
    check(true, "server-side verification agrees");

    // Immutable ledger feed.
    const rows = page.locator("tbody tr");
    await rows.nth(1).waitFor();
    check((await rows.count()) === 2, "ledger feed lists the deposit and the transfer to fixed term");
    check(await page.getByText("To fixed term").isVisible(), "ledger labels the bridge movement");
    await rows.first().click();
    await page.getByText("Previous hash (chain link)").waitFor();
    check(true, "ledger row expands to show the hash chain");
    await shot(page, "dashboard-light");

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
    issues = await tamper("**/api/v1/reserva/prueba", (pr) => ({
      ...pr,
      hoja: { ...pr.hoja, cuenta_id: "00000000-0000-4000-8000-000000000000" },
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

// No console error is tolerated except the browser's own request for a
// favicon the app does not ship (a 404 on /favicon.ico, not app code).
const benign = (e) => /Failed to load resource: .*404/.test(e) && /\/favicon\.ico\)$/.test(e);
const relevant = errors.filter((e) => !benign(e));
if (errors.length > relevant.length) console.log(`(ignored ${errors.length - relevant.length} favicon 404 message(s))`);
if (relevant.length) {
  console.error("Console errors:\n" + relevant.join("\n"));
  process.exit(1);
}
console.log("All end-to-end checks passed.");
