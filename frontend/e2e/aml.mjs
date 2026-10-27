/**
 * Real-stack end-to-end test of the AML/PLD flow: a client structures deposits
 * just under the reporting threshold -> alert -> CONGELADO_PREVENTIVO with the
 * funds held, while the client sees nothing (no tipping off) -> officer 1
 * proposes a false positive -> officer 2 approves -> the client operates again.
 * Also: manual freeze, escalate, confirm, UIF folio, four-eyes requests, the
 * AUDITOR read-only and CLIENTE / OPERADOR locked out of the console.
 *
 *   e2e/aml-stack.sh start          # fresh database, backend :8080 (MODO_DEMO + officers), next start :3000
 *   NODE_PATH=/opt/node22/lib/node_modules PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers \
 *     node e2e/aml.mjs [out-dir]
 *   e2e/aml-stack.sh stop
 *
 * Writes to out-dir (default e2e/out, git-ignored):
 *   network.json  every request the browser made to the API (method, url, bodies, status)
 *   results.json  every check with pass/fail, console errors, failed requests
 *   *.png         screenshots of each step
 * Checks are soft: a failure is recorded and the run continues when it can.
 * Exit code 1 if any check failed. The only expected console errors are the
 * browser's own lines for the deliberate 4xx calls (refused operations, the
 * AUDITOR POST, CLIENTE / OPERADOR calls).
 */
import { createRequire } from "node:module";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// Node >= 22.18 strips TypeScript types: the app's own KYC library builds a
// valid identity for each run, as in smoke.mjs.
import { sampleIdentity, SAMPLE_IDENTITY_SPACE, todayInMexicoCity } from "../src/lib/kyc.ts";

const { chromium } = createRequire(import.meta.url)("playwright");

const HERE = dirname(fileURLToPath(import.meta.url));
const BASE = process.env.FRONTEND_URL ?? "http://localhost:3000";
const API = process.env.API_URL ?? "http://localhost:8080";
const OUT = process.argv[2] ?? join(HERE, "out");
mkdirSync(OUT, { recursive: true });

const OFFICER1 = { email: "oficial1@cumplimiento.mx", password: "cumplimiento-e2e-2026" };
const OFFICER2 = { email: "oficial2@cumplimiento.mx", password: "cumplimiento-e2e-2026" };
const AUDITOR = { email: "auditor@synthetic.mx", password: "auditor-e2e-2026" };
const OPERATOR = { email: "operador@synthetic.mx", password: "operador-e2e-2026" };
const GENERIC = "This operation is not available right now. If you need help, please contact support.";
const DEPOSIT = process.env.DEPOSIT_AMOUNT ?? "140000"; // in the R05 band (>= 0.80 x 150,000) and under the 150,000 threshold

const runId = Date.now();
const client = { email: `e2e-client-${runId}@example.mx`, password: "austrian-school-1926" };
const identity = sampleIdentity(runId % SAMPLE_IDENTITY_SPACE, todayInMexicoCity());

const checks = [];
const traffic = [];
const consoleErrors = [];
const failedRequests = [];
let step = "setup";

function check(ok, message, extra) {
  checks.push({ step, ok: Boolean(ok), message, ...(extra === undefined ? {} : { extra }) });
  console.log(`${ok ? "ok  " : "FAIL"} [${step}] ${message}${!ok && extra !== undefined ? ` :: ${JSON.stringify(extra).slice(0, 400)}` : ""}`);
  return Boolean(ok);
}
function phase(name) {
  step = name;
  console.log(`\n=== ${name}`);
}
async function shot(page, name) {
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: true }).catch(() => undefined);
}

// ---------------------------------------------------------------------------
// Direct API calls from Node (oracle / setup only: not part of the console traffic).
// ---------------------------------------------------------------------------
async function apiCall(method, path, { token, body } = {}) {
  const res = await fetch(`${API}${path}`, {
    method,
    headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    json = text;
  }
  return { status: res.status, body: json };
}
async function apiLogin({ email, password }) {
  const r = await apiCall("POST", "/api/v1/auth/login", { body: { email, password } });
  if (r.status !== 200) throw new Error(`login ${email}: ${r.status} ${JSON.stringify(r.body)}`);
  return r.body;
}
function clabeWithCheck(d17) {
  const w = [3, 7, 1];
  const s = [...d17].reduce((acc, c, i) => acc + ((Number(c) * w[i % 3]) % 10), 0);
  return d17 + String((10 - (s % 10)) % 10);
}

// ---------------------------------------------------------------------------
// Browser contexts with network capture.
// ---------------------------------------------------------------------------
const browser = await chromium.launch({ args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream"] });

async function newContext(label, options = {}) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, permissions: ["camera"], ...options });
  ctx.on("response", async (res) => {
    const req = res.request();
    const url = req.url();
    if (!url.startsWith(API) || req.method() === "OPTIONS") return;
    const entry = { label, step, t: Date.now(), method: req.method(), url, status: res.status() };
    const u = new URL(url);
    entry.path = u.pathname;
    entry.query = Object.fromEntries(u.searchParams);
    try {
      entry.reqBody = req.postDataJSON();
    } catch {
      entry.reqBody = req.postData();
    }
    traffic.push(entry);
    try {
      const text = await res.text();
      entry.resBody = text ? JSON.parse(text) : null;
    } catch (e) {
      entry.resBody = `<<unreadable: ${e.message}>>`;
    }
  });
  ctx.on("requestfailed", (req) => {
    if (!req.url().startsWith(API)) return;
    failedRequests.push({ label, step, method: req.method(), url: req.url(), error: req.failure()?.errorText });
  });
  ctx.on("page", (page) => {
    page.on("console", (m) => {
      if (m.type() === "error") consoleErrors.push({ label, step, text: m.text(), url: m.location().url || null });
    });
    page.on("pageerror", (e) => consoleErrors.push({ label, step, text: `pageerror: ${e.message}` }));
  });
  return ctx;
}
async function uiLogin(page, { email, password }) {
  await page.goto(`${BASE}/auth/login`);
  await page.fill("#email", email);
  await page.fill("#password", password);
  await page.click("button[type=submit]");
}
/** fetch() from inside the page with the page's own session (what a curious client could do from devtools). */
async function pageFetch(page, method, path, body) {
  return page.evaluate(
    async ({ api, method, path, body }) => {
      const token = JSON.parse(sessionStorage.getItem("synthetic-capital-session") ?? "{}")?.state?.token;
      const r = await fetch(`${api}${path}`, {
        method,
        headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      const t = await r.text();
      let j = null;
      try {
        j = t ? JSON.parse(t) : null;
      } catch {
        j = t;
      }
      return { status: r.status, body: j };
    },
    { api: API, method, path, body },
  );
}
/** A feed row by folio (exact match on the folio cell; row text runs cells together, e.g. "#41 min ago"). */
const alertRow = (page, folio) =>
  page.getByTestId("aml-alert-row").filter({ has: page.locator("td:first-child", { hasText: new RegExp(`^#${folio}$`) }) });
const lastResponse = (pred) => [...traffic].reverse().find(pred);
/** Runs `action` while waiting for the matching response; never leaves a dangling rejection. */
async function withResponse(page, pred, action, timeout = 20_000) {
  const [r] = await Promise.all([page.waitForResponse(pred, { timeout }), action()]);
  return r;
}
async function demoDeposit(page, amount) {
  await page.getByRole("button", { name: /Add funds/ }).click();
  await page.fill("#demo-amount", amount);
  const r = await withResponse(page, (x) => x.url().includes("/ifpe/depositos/simulados") && x.request().method() === "POST", () =>
    page.getByRole("button", { name: "Deposit", exact: true }).click(),
  );
  await page.locator("#demo-amount").waitFor({ state: "detached", timeout: 10_000 }).catch(() => undefined);
  return { status: r.status(), body: await r.json().catch(() => null) };
}

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
      return canvas.toDataURL("image/jpeg", 0.9);
    },
    { label, hue },
  );
  return Buffer.from(dataUrl.slice(dataUrl.indexOf(",") + 1), "base64");
}

async function renderDataUrl(page, label, hue) {
  return `data:image/jpeg;base64,${(await renderJpeg(page, label, hue)).toString("base64")}`;
}
/** Registers and verifies a client through the API (simulated provider), for the scenarios that need a second client. */
async function apiOnboard(page, seed, email) {
  const id = sampleIdentity(seed % SAMPLE_IDENTITY_SPACE, todayInMexicoCity());
  const reg = await apiCall("POST", "/api/v1/auth/registro", {
    body: { tipo_persona: "FISICA", nombre: id.profile.nombres, apellido_paterno: id.profile.apellido_paterno, email, password: client.password },
  });
  if (reg.status !== 201) throw new Error(`registro ${email}: ${reg.status} ${JSON.stringify(reg.body)}`);
  const token = reg.body.sesion.token;
  const perfil = await apiCall("POST", "/api/v1/kyc/perfil", { token, body: id.profile });
  if (perfil.status >= 300) throw new Error(`perfil ${email}: ${perfil.status} ${JSON.stringify(perfil.body)}`);
  const imgs = [];
  for (const [label, hue] of [["INE F", 200], ["INE R", 20], ["S1", 90], ["S2", 130], ["S3", 170]]) imgs.push(await renderDataUrl(page, `${label} ${seed}`, hue));
  const ver = await apiCall("POST", "/api/v1/kyc/verificar", {
    token,
    body: { ine: { ...id.credential, frente: imgs[0], reverso: imgs[1] }, selfie: { frames: imgs.slice(2) }, consentimiento_biometrico: true },
  });
  if (ver.body?.resultado?.estado_kyc !== "VERIFICADO") throw new Error(`verificar ${email}: ${ver.status} ${JSON.stringify(ver.body).slice(0, 300)}`);
  return { email, token, id: reg.body.sesion.cliente_id, clabe: reg.body.cuenta_ifpe.clabe, nombre: `${id.profile.nombres} ${id.profile.apellido_paterno}` };
}

/** Words that would tip a client off if they appeared on the client's own screens. */
const TIP_OFF = /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/gi;

let exitCode = 0;
let officer1Token;
let clientId;
let freeze = null;
const state = {};

try {
  officer1Token = (await apiLogin(OFFICER1)).token;

  // =========================================================================
  phase("1-client-signup-onboarding");
  // =========================================================================
  const clientCtx = await newContext("client");
  const cp = await clientCtx.newPage();
  await cp.goto(`${BASE}/auth/register`);
  await cp.fill("#nombre", "Ludwig");
  await cp.fill("#paterno", "Mises");
  await cp.fill("#email", client.email);
  await cp.fill("#password", client.password);
  await cp.fill("#confirm", client.password);
  await cp.click("button[type=submit]");
  await cp.waitForURL("**/auth/onboarding");
  await cp.getByTestId("onboarding-step-1").waitFor();
  check(true, "client signs up and lands on onboarding step 1");
  clientId = await cp.evaluate(() => JSON.parse(sessionStorage.getItem("synthetic-capital-session")).state.clientId);

  const p = identity.profile;
  const d = p.domicilio;
  await cp.fill("#nombres", p.nombres);
  await cp.fill("#apellido-paterno", p.apellido_paterno);
  await cp.fill("#apellido-materno", p.apellido_materno ?? "");
  await cp.fill("#curp", p.curp);
  await cp.fill("#rfc", p.rfc);
  await cp.fill("#calle", d.calle);
  await cp.fill("#numero-exterior", d.numero_exterior);
  await cp.fill("#colonia", d.colonia);
  await cp.fill("#municipio", d.municipio);
  await cp.fill("#estado", d.estado);
  await cp.fill("#codigo-postal", d.codigo_postal);
  await cp.getByTestId("onboarding-step-1").getByText("Check digit OK").waitFor();
  await cp.getByRole("button", { name: "Save and continue" }).click();
  const step2 = cp.getByTestId("onboarding-step-2");
  await step2.waitFor();
  const front = await renderJpeg(cp, "INE FRENTE", 200);
  const back = await renderJpeg(cp, "INE REVERSO", 20);
  await cp.setInputFiles("#ine-frente-file", { name: "f.jpg", mimeType: "image/jpeg", buffer: front });
  await cp.setInputFiles("#ine-reverso-file", { name: "r.jpg", mimeType: "image/jpeg", buffer: back });
  await step2.getByAltText("Captured front of your INE").waitFor();
  await step2.getByAltText("Captured back of your INE").waitFor();
  const c = identity.credential;
  await cp.fill("#clave-elector", c.clave_elector);
  await cp.fill("#cic", c.cic);
  await cp.fill("#identificador", c.identificador_ciudadano);
  await cp.fill("#emision", String(c.emision));
  await cp.fill("#vigencia", String(c.vigencia));
  await step2.getByText("Birth date and sex match your CURP").waitFor();
  await step2.getByRole("button", { name: "Continue" }).click();
  const step3 = cp.getByTestId("onboarding-step-3");
  await step3.waitFor();
  await cp.check("#consentimiento");
  const verify = cp.waitForResponse((r) => r.url().includes("/kyc/verificar") && r.request().method() === "POST", { timeout: 60_000 });
  await step3.getByRole("button", { name: "Start liveness check" }).click();
  const vr = await verify;
  const verdict = await vr.json();
  check(vr.status() === 200 && verdict.resultado?.resultado === "APROBADO", `KYC verification ${vr.status()} ${verdict.resultado?.resultado ?? JSON.stringify(verdict)}`);
  const step4 = cp.getByTestId("onboarding-step-4");
  await step4.waitFor();
  await step4.getByTestId("kyc-status-badge").and(cp.locator("[data-status=VERIFICADO]")).waitFor({ timeout: 30_000 });
  await step4.getByRole("button", { name: "Go to dashboard" }).click();
  await cp.waitForURL("**/dashboard");
  await cp.getByTestId("kyc-header-badge").and(cp.locator("[data-status=VERIFICADO]")).waitFor();
  check(true, "client completes onboarding (VERIFICADO) and reaches the dashboard");
  // Client B: verified through the API; the internal-transfer recipient here and the subject of scenario B.
  state.b = await apiOnboard(cp, runId + 1, `e2e-b-${runId}@example.mx`);
  check(true, `client B ${state.b.nombre} verified through the API (CLABE ${state.b.clabe})`);

  // =========================================================================
  phase("2-structured-deposits");
  // =========================================================================
  state.deposits = [];
  // A first, ordinary deposit outside the band: it stays available, so the invest form can be submitted later.
  const first = await demoDeposit(cp, "20000");
  check(first.status === 201, `opening deposit of 20,000 answered ${first.status}`, first.status === 201 ? undefined : first.body);
  for (let i = 1; i <= 6 && !freeze; i++) {
    const r = await demoDeposit(cp, DEPOSIT);
    state.deposits.push({ i, ...r });
    check(r.status === 201, `deposit #${i} of ${DEPOSIT} answered ${r.status}`, r.status === 201 ? undefined : r.body);
    const f = await apiCall("GET", `/api/v1/aml/congelamientos?cliente_id=${clientId}`, { token: officer1Token });
    freeze = f.body?.congelamientos?.[0] ?? null;
    const al = await apiCall("GET", `/api/v1/aml/alertas?cliente_id=${clientId}&estado=TODOS`, { token: officer1Token });
    console.log(
      `   after #${i}: alerts=${JSON.stringify((al.body?.alertas ?? []).map((a) => [a.folio, a.regla.codigo, a.nivel_riesgo, a.puntaje, a.estado]))} freeze=${freeze ? `${freeze.folio} held ${freeze.retenido_vigente}` : "none"}`,
    );
  }
  check(freeze?.estado === "ACTIVO", "an AML alert fired and the account was frozen preventively", freeze ?? state.deposits);
  // A deposit while frozen: credited, then held.
  if (freeze) {
    const r = await demoDeposit(cp, "5000");
    state.frozenDeposit = r;
    check(r.status === 201, `a deposit while frozen is accepted (${r.status}) like any other`, r.body);
    const depJson = JSON.stringify(state.frozenDeposit.body ?? {});
    TIP_OFF.lastIndex = 0;
    check(!/RETEN|CONGEL|AML|ALERT/i.test(depJson), "the deposit response itself does not reveal the hold or the freeze", state.frozenDeposit.body);
  }

  // =========================================================================
  phase("3-client-sees-nothing");
  // =========================================================================
  await cp.reload();
  await cp.getByTestId("vault-available").waitFor();
  await cp.waitForTimeout(1500);
  const tablero = lastResponse((e) => e.label === "client" && e.path === "/api/v1/tablero" && e.status === 200);
  check(tablero?.resBody?.boveda?.estado === "ACTIVA", `GET /tablero shows boveda.estado ${tablero?.resBody?.boveda?.estado} (must be ACTIVA)`);
  const tabJson = JSON.stringify(tablero?.resBody ?? {});
  check(!/CONGELADO|nivel_riesgo|es_pep|retenido_aml|alerta/i.test(tabJson), "GET /tablero carries no freeze, risk or alert field", tablero?.resBody);
  const yo = await pageFetch(cp, "GET", "/api/v1/yo");
  check(!/nivel_riesgo|es_pep|congel|alerta/i.test(JSON.stringify(yo.body)), "GET /yo carries no risk, PEP or freeze field", yo.body);
  const cuenta = await pageFetch(cp, "GET", "/api/v1/ifpe/cuenta");
  check(cuenta.body?.estado === "ACTIVA", `GET /ifpe/cuenta estado ${cuenta.body?.estado}`, cuenta.body);
  const movs = await pageFetch(cp, "GET", "/api/v1/ifpe/movimientos");
  state.clientMovements = movs.body;
  const movJson = JSON.stringify(movs.body ?? []);
  check(!/aml|pld|congel|alerta|cumplimiento/i.test(movJson), "client movements carry no AML wording (descriptions, keys)", movs.body);
  const vaultText = await cp.locator("main").innerText();
  check((await cp.getByText(/Account (congelad|frozen|restrict)/i).count()) === 0, "vault shows no 'Account ...' restriction banner");
  const holdLine = await cp.getByText(/MXN on hold \(still fully backed\)/).count();
  check(holdLine > 0 || /0\.00/.test(tablero?.resBody?.boveda?.saldo_bloqueado ?? ""), `held funds use the neutral hold copy (${holdLine} line(s))`);
  const hits = [...vaultText.matchAll(TIP_OFF)].map((m) => m[0]);
  check(hits.length === 0, `dashboard text has no tipping-off words (${hits.join(", ") || "none"})`, hits);
  await shot(cp, "client-dashboard-frozen");

  // =========================================================================
  phase("4-client-operations-refused");
  // =========================================================================
  // Invest through the UI.
  await cp.fill("#bridge-amount", "1000");
  await cp.getByText("182d").click();
  await cp.getByLabel(/no early withdrawal/).check();
  const lockEnabled = await cp.getByRole("button", { name: /Lock funds/ }).isEnabled();
  check(lockEnabled, "invest form can be submitted (available balance outside the held pattern)");
  const invR = await withResponse(cp, (r) => r.url().endsWith("/api/v1/sofom/mutuos") && r.request().method() === "POST", () =>
    cp.getByRole("button", { name: /Lock funds/ }).click({ timeout: 5000 }),
  );
  const invBody = await invR.json().catch(() => null);
  check(invBody?.error?.codigo === "OPERACION_NO_DISPONIBLE", `invest answered ${invR.status()} ${invBody?.error?.codigo}`, invBody);
  await cp.getByText(GENERIC).first().waitFor({ timeout: 5000 }).catch(() => undefined);
  check((await cp.getByText(GENERIC).count()) > 0, "the invest form shows only the constant OPERACION_NO_DISPONIBLE message");
  await shot(cp, "client-invest-refused");
  // Transfer, SPEI send and withdrawal through the API with the client's own session.
  const internalClabe = state.b.clabe;
  const externalClabe = clabeWithCheck("01218000123456789");
  const attempts = {
    transferencia: await pageFetch(cp, "POST", "/api/v1/ifpe/transferencias", { clabe_destino: internalClabe, monto: "100.00", clave_idempotencia: crypto.randomUUID() }),
    spei: await pageFetch(cp, "POST", "/api/v1/spei/transfer", {
      clabe_destino: externalClabe,
      beneficiario_nombre: "Proveedor Externo SA",
      monto: "100.00",
      clave_idempotencia: crypto.randomUUID(),
    }),
    retiro: await pageFetch(cp, "POST", "/api/v1/ifpe/retiros", { clabe_destino: externalClabe, monto: "100.00", clave_idempotencia: crypto.randomUUID() }),
    inversion: { status: invR.status(), body: invBody },
  };
  state.refusals = attempts;
  for (const [k, v] of Object.entries(attempts)) {
    check(v.body?.error?.codigo === "OPERACION_NO_DISPONIBLE", `${k} attempt answered ${v.status} ${v.body?.error?.codigo}: ${v.body?.error?.mensaje}`, v.body);
  }
  const shapes = new Set(Object.values(attempts).map((v) => JSON.stringify([v.status, v.body])));
  check(shapes.size === 1, "every refused operation gets the identical status and body", [...shapes]);

  // =========================================================================
  phase("5-officer1-console");
  // =========================================================================
  const o1Ctx = await newContext("officer1");
  const o1 = await o1Ctx.newPage();
  await uiLogin(o1, OFFICER1);
  await o1.waitForURL("**/admin/aml", { timeout: 15_000 }).catch(() => undefined);
  check(new URL(o1.url()).pathname === "/admin/aml", `officer 1 lands on ${new URL(o1.url()).pathname}`);
  const rows = o1.getByTestId("aml-alert-row");
  await rows.first().waitFor({ timeout: 20_000 }).catch(() => undefined);
  const nRows = await rows.count();
  check(nRows > 0, `the feed lists ${nRows} alert row(s)`);
  await shot(o1, "officer1-feed");
  const feedPage = lastResponse((e) => e.label === "officer1" && e.path === "/api/v1/aml/alertas" && !e.query.actualizadas_desde && e.status === 200);
  const clientAlerts = (feedPage?.resBody?.alertas ?? []).filter((a) => a.cliente.id === clientId);
  check(clientAlerts.length > 0, `the client's alerts are in the feed page (${clientAlerts.map((a) => `#${a.folio} ${a.regla.codigo} ${a.nivel_riesgo} ${a.puntaje}`).join("; ")})`);
  const critical = clientAlerts.find((a) => a.congelamiento) ?? clientAlerts[0];
  state.criticalAlert = critical;
  if (critical) {
    const row = alertRow(o1, critical.folio).first();
    const rowText = (await row.innerText().catch(() => "")).replace(/\s+/g, " ");
    check(rowText.includes(critical.regla.codigo), `feed row #${critical.folio} shows rule ${critical.regla.codigo}`, rowText);
    check(rowText.includes(String(Math.round(Number(critical.puntaje)))) || rowText.includes(critical.puntaje), `feed row shows the score ${critical.puntaje}`, rowText);
    check((await row.getAttribute("data-level")) === critical.nivel_riesgo, `feed row level ${critical.nivel_riesgo}`);
    // Summary bar.
    const summary = lastResponse((e) => e.label === "officer1" && e.path === "/api/v1/aml/resumen" && e.status === 200);
    check(summary?.resBody?.congelamientos_activos >= 1, `summary counts ${summary?.resBody?.congelamientos_activos} active freeze(s)`, summary?.resBody);

    // Case file.
    await row.click();
    const drawer = o1.getByRole("dialog");
    await drawer.waitFor();
    await drawer.getByText("Score breakdown").waitFor({ timeout: 10_000 }).catch(() => undefined);
    check(o1.url().includes(`alerta=${critical.id}`), "opening a row deep-links ?alerta=<id>");
    const detail = lastResponse((e) => e.label === "officer1" && e.path === `/api/v1/aml/alertas/${critical.id}`);
    check(detail?.status === 200, `GET /aml/alertas/{id} answered ${detail?.status}`, detail?.resBody);
    state.detail = detail?.resBody;
    const dText = (await drawer.innerText()).replace(/\s+/g, " ");
    for (const s of ["Score breakdown", "Transaction context", "Client", "Resolution", "History"]) {
      check(dText.toLowerCase().includes(s.toLowerCase()), `case file has the "${s}" section`);
    }
    check((detail?.resBody?.movimientos ?? []).length > 0, `case file lists ${(detail?.resBody?.movimientos ?? []).length} pattern movement(s)`);
    check(/Client restricted|Preventive restriction|restriction/i.test(dText), "case file shows the client is restricted");
    await shot(o1, "officer1-casefile");

    // Take.
    await drawer.getByRole("button", { name: "Take for review" }).click();
    await drawer.getByText("is assigned to you").waitFor({ timeout: 10_000 }).catch(() => undefined);
    const take = lastResponse((e) => e.label === "officer1" && e.path.endsWith("/resolver") && e.reqBody?.accion === "TOMAR");
    check(take?.status === 201 && take?.resBody?.alerta?.estado === "EN_REVISION", `TOMAR answered ${take?.status} ${take?.resBody?.alerta?.estado ?? JSON.stringify(take?.resBody)}`);

    // False positive with an unfreeze request.
    await drawer.getByText("Clear as false positive").first().waitFor({ timeout: 10_000 }).catch(() => undefined);
    await drawer.getByRole("radio", { name: /Clear as false positive/ }).check();
    await drawer.getByLabel("Dictamen").fill("Depósitos de la venta documentada de un inmueble; escritura pública y estado de cuenta verificados.");
    const releaseChoice = drawer.getByRole("radio", { name: /Release held funds/ });
    check((await releaseChoice.count()) > 0, "restricted client: the form offers lifting with release of held funds");
    if (await releaseChoice.count()) await releaseChoice.check();
    await drawer.getByRole("button", { name: "Clear as false positive" }).click();
    const confirm = o1.getByRole("dialog", { name: "Clear as false positive?" });
    await confirm.waitFor({ timeout: 5000 }).catch(() => undefined);
    await shot(o1, "officer1-confirm-fp");
    await confirm.getByRole("button", { name: "Clear alert" }).click();
    await o1.getByText(/False positive proposed on alert|cleared as a false positive/).first().waitFor({ timeout: 10_000 }).catch(() => undefined);
    const fp = lastResponse((e) => e.label === "officer1" && e.path.endsWith("/resolver") && e.reqBody?.accion === "FALSO_POSITIVO");
    state.fp = fp;
    check(fp?.status === 201 && fp?.resBody?.solicitud?.estado === "PENDIENTE", `FALSO_POSITIVO answered ${fp?.status}, request ${fp?.resBody?.solicitud?.folio} ${fp?.resBody?.solicitud?.estado ?? JSON.stringify(fp?.resBody)}`);
    const shownFp = await o1.getByText(/False positive proposed on alert/).count();
    check(shownFp > 0, "the case file confirms the false-positive proposal and the pending request");
    await shot(o1, "officer1-after-fp");

    // Other open high-risk alerts of the client block the lift: propose them too.
    const others = (await apiCall("GET", `/api/v1/aml/alertas?cliente_id=${clientId}`, { token: officer1Token })).body?.alertas ?? [];
    const blocking = others.filter((a) => a.id !== critical.id && ["ALTO", "CRITICO"].includes(a.nivel_riesgo) && !a.resolucion_propuesta);
    state.blocking = blocking.map((a) => `#${a.folio} ${a.regla.codigo} ${a.nivel_riesgo}`);
    for (const a of blocking) {
      await o1.goto(`${BASE}/admin/aml?alerta=${a.id}`);
      const dr = o1.getByRole("dialog");
      await dr.getByRole("button", { name: "Take for review" }).click();
      await dr.getByText("is assigned to you").waitFor({ timeout: 10_000 }).catch(() => undefined);
      await dr.getByRole("radio", { name: /Clear as false positive/ }).check();
      await dr.getByLabel("Dictamen").fill("Mismo origen documentado que la alerta principal: venta de inmueble con escritura pública.");
      await dr.getByRole("button", { name: "Clear as false positive" }).click();
      await o1.getByRole("dialog", { name: "Clear as false positive?" }).getByRole("button", { name: "Clear alert" }).click();
      await o1.getByText(/False positive proposed on alert|cleared as a false positive/).first().waitFor({ timeout: 10_000 }).catch(() => undefined);
      const r = lastResponse((e) => e.label === "officer1" && e.path === `/api/v1/aml/alertas/${a.id}/resolver` && e.reqBody?.accion === "FALSO_POSITIVO");
      check(r?.status === 201, `second high alert #${a.folio} (${a.regla.codigo}) proposed as false positive: ${r?.status} ${r?.resBody?.alerta?.resolucion_propuesta ?? JSON.stringify(r?.resBody)}`);
    }
    await o1.keyboard.press("Escape");

    // Officer 1 must not be able to approve their own request.
    await o1.goto(`${BASE}/admin/aml?tab=aprobaciones`);
    await o1.getByTestId("aml-request").first().waitFor({ timeout: 15_000 }).catch(() => undefined);
    const reqRow = o1.getByTestId("aml-request").filter({ hasText: new RegExp(`#${state.fp?.resBody?.solicitud?.folio}(?!\\d)`) }).first();
    const approveDisabled = await reqRow.getByRole("button", { name: "Approve" }).isDisabled().catch(() => null);
    check(approveDisabled === true, `officer 1 sees the request with Approve disabled (${approveDisabled})`);
    await shot(o1, "officer1-approvals");
    // Let a poll cycle run with the console open.
    await o1.goto(`${BASE}/admin/aml`);
    await o1.waitForTimeout(23_000);
    const polls = traffic.filter((e) => e.label === "officer1" && e.path === "/api/v1/aml/alertas" && e.query.actualizadas_desde);
    check(polls.length >= 1 && polls.every((e) => e.status === 200), `poll requests: ${polls.length}, statuses ${[...new Set(polls.map((e) => e.status))].join(",")}`, polls.filter((e) => e.status !== 200).map((e) => e.resBody));
  }

  // =========================================================================
  phase("6-auditor-read-only");
  // =========================================================================
  const auCtx = await newContext("auditor");
  const au = await auCtx.newPage();
  await uiLogin(au, AUDITOR);
  await au.waitForURL("**/admin/aml", { timeout: 15_000 }).catch(() => undefined);
  check(new URL(au.url()).pathname === "/admin/aml", `auditor lands on ${new URL(au.url()).pathname}`);
  await au.getByText("Auditor · read-only").waitFor({ timeout: 10_000 }).catch(() => undefined);
  check((await au.getByText("Auditor · read-only").count()) > 0, "header badge says Auditor · read-only");
  await au.getByTestId("aml-alert-row").first().waitFor({ timeout: 15_000 }).catch(() => undefined);
  await shot(au, "auditor-feed");
  if (state.criticalAlert) {
    await au.goto(`${BASE}/admin/aml?alerta=${state.criticalAlert.id}`);
    const dr = au.getByRole("dialog");
    await dr.getByText("Score breakdown").waitFor({ timeout: 10_000 }).catch(() => undefined);
    const t = await dr.innerText();
    check(!/Take for review|Clear as false positive|Freeze client|Request lift|Release held/i.test(t), "auditor case file has no action controls");
    check((await dr.getByRole("button", { name: /Take|Clear|Confirm|Escalate|Freeze|Approve|Reject|Request/ }).count()) === 0, "auditor case file has no action buttons");
    await shot(au, "auditor-casefile");
    await au.keyboard.press("Escape");
  }
  await au.goto(`${BASE}/admin/aml?tab=aprobaciones`);
  await au.getByTestId("aml-request").first().waitFor({ timeout: 15_000 }).catch(() => undefined);
  check((await au.getByTestId("aml-request").count()) > 0, "auditor sees the pending four-eyes request");
  // e2e#4: bootstrapped officers have distinct names, so an auditor can tell the requester apart.
  const auReqText = (await au.getByTestId("aml-request").first().innerText().catch(() => "")).replace(/\s+/g, " ");
  check(/Cumplimiento oficial1/.test(auReqText), `auditor sees who requested it by a distinct name ("${(auReqText.match(/Requested by [^·]+/) ?? [auReqText.slice(0, 80)])[0]}")`);
  check((await au.getByRole("button", { name: /^(Approve|Reject)$/ }).count()) === 0, "auditor has no Approve / Reject buttons");
  await shot(au, "auditor-approvals");
  await au.goto(`${BASE}/admin/aml?tab=congelamientos`);
  await au.getByTestId("aml-freeze-row").first().waitFor({ timeout: 15_000 }).catch(() => undefined);
  if (await au.getByTestId("aml-freeze-row").count()) {
    await au.getByTestId("aml-freeze-row").first().click();
    const dr = au.getByRole("dialog");
    await dr.waitFor();
    check((await dr.getByRole("button", { name: /Request|Lift|Release|Approve|Reject/ }).count()) === 0, "auditor freeze drawer has no request controls");
    await shot(au, "auditor-freeze");
    await au.keyboard.press("Escape");
  }
  const auPost = await pageFetch(au, "POST", `/api/v1/aml/alertas/${state.criticalAlert?.id}/resolver`, { accion: "LIBERAR", clave_idempotencia: crypto.randomUUID() });
  check(auPost.status === 403, `auditor POST /resolver answered ${auPost.status} ${auPost.body?.error?.codigo}`);

  // =========================================================================
  phase("7-cliente-and-operador-locked-out");
  // =========================================================================
  const before = traffic.length;
  await cp.goto(`${BASE}/admin/aml`);
  await cp.getByText("This console is for compliance officers and auditors.").waitFor({ timeout: 10_000 }).catch(() => undefined);
  check((await cp.getByText("This console is for compliance officers and auditors.").count()) > 0, "CLIENTE opening /admin/aml gets the staff-only notice");
  check(!traffic.slice(before).some((e) => e.label === "client" && e.path.startsWith("/api/v1/aml")), "CLIENTE page makes no /aml request");
  await shot(cp, "cliente-admin-blocked");
  const cliAml = await pageFetch(cp, "GET", "/api/v1/aml/alertas");
  check(cliAml.status === 403, `CLIENTE GET /aml/alertas answered ${cliAml.status}`);
  await cp.goto(`${BASE}/dashboard`);
  await cp.getByTestId("vault-available").waitFor();

  const opCtx = await newContext("operator");
  const op = await opCtx.newPage();
  await uiLogin(op, OPERATOR);
  await op.waitForTimeout(2500);
  check(new URL(op.url()).pathname !== "/admin/aml", `OPERADOR sign-in does not reach the console (at ${new URL(op.url()).pathname})`);
  const opSession = await apiLogin(OPERATOR);
  await op.evaluate((s) => {
    sessionStorage.setItem(
      "synthetic-capital-session",
      JSON.stringify({ state: { token: s.token, expiresAt: s.expira_en, clientId: s.cliente_id, role: s.rol, email: "operador@synthetic.mx" }, version: 0 }),
    );
  }, opSession);
  const beforeOp = traffic.length;
  await op.goto(`${BASE}/admin/aml`);
  await op.getByText("This console is for compliance officers and auditors.").waitFor({ timeout: 10_000 }).catch(() => undefined);
  check((await op.getByText("This console is for compliance officers and auditors.").count()) > 0, "OPERADOR session opening /admin/aml gets the staff-only notice");
  check(!traffic.slice(beforeOp).some((e) => e.label === "operator" && e.path.startsWith("/api/v1/aml")), "OPERADOR page makes no /aml request");
  await shot(op, "operador-admin-blocked");
  const opAml = await pageFetch(op, "GET", "/api/v1/aml/resumen");
  check(opAml.status === 403, `OPERADOR GET /aml/resumen answered ${opAml.status}`);

  // =========================================================================
  phase("8-officer2-approves");
  // =========================================================================
  const o2Ctx = await newContext("officer2");
  const o2 = await o2Ctx.newPage();
  await uiLogin(o2, OFFICER2);
  await o2.waitForURL("**/admin/aml", { timeout: 15_000 }).catch(() => undefined);
  await o2.getByRole("tab", { name: /Approvals/ }).click();
  const req2 = o2.getByTestId("aml-request").filter({ hasText: new RegExp(`#${state.fp?.resBody?.solicitud?.folio}(?!\\d)`) }).first();
  await req2.waitFor({ timeout: 15_000 }).catch(() => undefined);
  await shot(o2, "officer2-approvals");
  const approve = req2.getByRole("button", { name: "Approve" });
  check((await approve.count()) > 0 && (await approve.isEnabled()), "officer 2 can approve the request");
  await approve.click();
  const dlg = o2.getByRole("dialog");
  await dlg.getByLabel("Comment").fill("Expediente revisado: el origen de los fondos está documentado y es consistente.");
  await dlg.getByRole("button", { name: "Approve" }).click();
  await o2.getByText(/Request #\d+ approved/).waitFor({ timeout: 10_000 }).catch(() => undefined);
  const dec = lastResponse((e) => e.label === "officer2" && e.path.endsWith("/decision"));
  state.decision = dec;
  check(dec?.status === 201 && dec?.resBody?.congelamiento?.estado === "LEVANTADO", `decision answered ${dec?.status}: freeze ${dec?.resBody?.congelamiento?.estado ?? JSON.stringify(dec?.resBody)}`);
  check((await o2.getByText(/Request #\d+ approved/).count()) > 0, `approval notice: ${(await o2.getByText(/Request #\d+ approved/).first().innerText().catch(() => "none"))}`);
  const reqBy = dec?.resBody?.solicitud?.solicitada_por?.nombre_completo;
  const decBy = dec?.resBody?.solicitud?.decidida_por?.nombre_completo;
  check(Boolean(reqBy && decBy && reqBy !== decBy), `requester and approver have distinct names ("${reqBy}" / "${decBy}")`);
  await shot(o2, "officer2-approved");
  await o2.getByRole("tab", { name: /Frozen clients/ }).click();
  await o2.waitForTimeout(2000);
  const clientName = state.detail?.cliente?.nombre_completo ?? "~none~";
  check((await o2.getByTestId("aml-freeze-row").filter({ hasText: clientName }).count()) === 0, `Frozen clients tab (ACTIVO) no longer lists ${clientName}`);
  const after = (await apiCall("GET", `/api/v1/aml/alertas?cliente_id=${clientId}&estado=TODOS`, { token: officer1Token })).body?.alertas ?? [];
  check(after.every((a) => a.estado === "FALSO_POSITIVO"), `client alerts after approval: ${after.map((a) => `#${a.folio} ${a.estado}`).join(", ")}`);

  // =========================================================================
  phase("9-client-operates-again");
  // =========================================================================
  await cp.reload();
  await cp.getByTestId("vault-available").waitFor();
  await cp.waitForTimeout(1500);
  const tab2 = lastResponse((e) => e.label === "client" && e.path === "/api/v1/tablero" && e.status === 200);
  check(/^0(\.0+)?$/.test(tab2?.resBody?.boveda?.saldo_bloqueado ?? "x"), `held funds released (saldo_bloqueado ${tab2?.resBody?.boveda?.saldo_bloqueado})`);
  check((await cp.getByText(/on hold \(still fully backed\)/).count()) === 0, "no hold line on the vault after release");
  await cp.fill("#bridge-amount", "1000");
  await cp.getByText("182d").click();
  await cp.getByLabel(/no early withdrawal/).check();
  const inv2R = await withResponse(cp, (r) => r.url().endsWith("/api/v1/sofom/mutuos") && r.request().method() === "POST", () =>
    cp.getByRole("button", { name: /Lock funds/ }).click({ timeout: 5000 }),
  );
  check(inv2R.status() === 201, `invest after the approval answered ${inv2R.status()}`, await inv2R.json().catch(() => null));
  await cp.getByText(/Contract #\d+ created/).waitFor({ timeout: 10_000 }).catch(() => undefined);
  const tr2 = await pageFetch(cp, "POST", "/api/v1/ifpe/transferencias", { clabe_destino: internalClabe, monto: "100.00", clave_idempotencia: crypto.randomUUID() });
  check(tr2.body?.error?.codigo !== "OPERACION_NO_DISPONIBLE", `transfer after the approval answered ${tr2.status} ${tr2.body?.error?.codigo ?? "ok"}`, tr2.body);
  const ledgerText = await cp.locator("main").innerText();
  const hits2 = [...ledgerText.matchAll(TIP_OFF)].map((m) => m[0]);
  check(hits2.length === 0, `dashboard after release has no tipping-off words (${hits2.join(", ") || "none"})`);
  await shot(cp, "client-dashboard-released");

  // =========================================================================
  phase("10-new-alert-reaches-open-console");
  // =========================================================================
  // Officer 2 keeps the Alerts tab open while client B structures two deposits (R02, no freeze).
  const B = state.b;
  await o2.goto(`${BASE}/admin/aml`);
  await o2.getByTestId("aml-alert-row").first().waitFor({ timeout: 15_000 }).catch(() => undefined);
  await o2.waitForTimeout(1500);
  for (const monto of ["100000.00", "60000.00"]) {
    const r = await apiCall("POST", "/api/v1/ifpe/depositos/simulados", { token: B.token, body: { monto, clave_idempotencia: crypto.randomUUID() } });
    check(r.status === 201, `client B deposit ${monto} answered ${r.status}`, r.status === 201 ? undefined : r.body);
  }
  const bAlerts = (await apiCall("GET", `/api/v1/aml/alertas?cliente_id=${B.id}`, { token: officer1Token })).body?.alertas ?? [];
  state.bAlerts = bAlerts.map((a) => `#${a.folio} ${a.regla.codigo} ${a.nivel_riesgo} ${a.puntaje}`);
  check(bAlerts.length > 0, `client B has an open alert (${state.bAlerts.join("; ")})`);
  const bAlert = bAlerts[0];
  const pill = o2.getByRole("button", { name: /\d+ new alerts?/ });
  await pill.waitFor({ timeout: 30_000 }).catch(() => undefined);
  check((await pill.count()) > 0, `the open console shows the "${await pill.innerText().catch(() => "N new alerts")}" pill after a poll`);
  if (await pill.count()) {
    await pill.click();
    await o2.waitForTimeout(500);
    check((await alertRow(o2, bAlert?.folio).count()) > 0, `merging the pill shows alert #${bAlert?.folio}`);
  }
  await shot(o2, "officer2-new-alert");

  if (bAlert) {
    // =======================================================================
    phase("11-officer1-full-case-B");
    // =======================================================================
    await o1.goto(`${BASE}/admin/aml?alerta=${bAlert.id}`);
    const dr = o1.getByRole("dialog").first();
    await dr.getByRole("button", { name: "Take for review" }).waitFor({ timeout: 15_000 }).catch(() => undefined);
    // Client review.
    await dr.getByRole("button", { name: "Open client review" }).click();
    const review = o1.getByRole("dialog", { name: /Client review/ });
    await review.getByText(/Activity/i).first().waitFor({ timeout: 10_000 }).catch(() => undefined);
    const rv = lastResponse((e) => e.label === "officer1" && e.path === `/api/v1/aml/clientes/${B.id}`);
    check(rv?.status === 200, `GET /aml/clientes/{id} answered ${rv?.status}`, rv?.resBody);
    await shot(o1, "officer1-client-review");
    await o1.keyboard.press("Escape");
    await o1.waitForTimeout(300);
    check((await o1.getByRole("dialog").count()) === 1, "Esc closes only the client review, the case file stays open");
    const resolveRes = (accion) => lastResponse((e) => e.label === "officer1" && e.path === `/api/v1/aml/alertas/${bAlert.id}/resolver` && e.reqBody?.accion === accion);
    // Take, release, take again.
    await dr.getByRole("button", { name: "Take for review" }).click();
    await dr.getByText("is assigned to you").waitFor({ timeout: 10_000 }).catch(() => undefined);
    await dr.getByRole("button", { name: "Release", exact: true }).click();
    await dr.getByText("is back in the pending queue").waitFor({ timeout: 10_000 }).catch(() => undefined);
    check(resolveRes("LIBERAR")?.status === 201 && resolveRes("LIBERAR")?.resBody?.alerta?.estado === "PENDIENTE", `LIBERAR answered ${resolveRes("LIBERAR")?.status} ${resolveRes("LIBERAR")?.resBody?.alerta?.estado ?? JSON.stringify(resolveRes("LIBERAR")?.resBody)}`);
    await dr.getByRole("button", { name: "Take for review" }).click();
    await dr.getByText("is assigned to you").waitFor({ timeout: 10_000 }).catch(() => undefined);
    // Escalate (no restriction).
    await dr.getByRole("radio", { name: /Escalate/ }).check();
    await dr.getByLabel("Dictamen").fill("Escalamiento: patrón de fraccionamiento que requiere revisión del oficial titular.");
    await dr.getByRole("button", { name: "Escalate", exact: true }).click();
    await dr.getByText(/escalated to critical/).waitFor({ timeout: 10_000 }).catch(() => undefined);
    const esc = resolveRes("ESCALAR");
    check(esc?.status === 201 && esc?.resBody?.alerta?.escalada === true, `ESCALAR answered ${esc?.status}, nivel ${esc?.resBody?.alerta?.nivel_riesgo}, escalada ${esc?.resBody?.alerta?.escalada}`, esc?.resBody?.error);
    // Manual freeze linked to the open alert.
    await dr.getByRole("button", { name: "Freeze client" }).click();
    const fz = o1.getByRole("dialog", { name: "Freeze client" });
    await fz.getByLabel("Reason").fill("Congelamiento preventivo manual: fraccionamiento escalado sin documentación del origen.");
    await fz.getByRole("button", { name: "Restrict client" }).click();
    await o1.getByText(/Restriction #\d+ applied|already restricted/).first().waitFor({ timeout: 10_000 }).catch(() => undefined);
    const man = lastResponse((e) => e.label === "officer1" && e.path === `/api/v1/aml/clientes/${B.id}/congelar`);
    check(man?.status === 201 && man?.resBody?.congelamiento?.estado === "ACTIVO", `manual freeze answered ${man?.status} ${man?.resBody?.congelamiento?.estado ?? JSON.stringify(man?.resBody)}`);
    state.bFreeze = man?.resBody?.congelamiento;
    // Client B, now frozen: a deposit is held, an SPEI send is refused with the constant error.
    const dep = await apiCall("POST", "/api/v1/ifpe/depositos/simulados", { token: B.token, body: { monto: "5000.00", clave_idempotencia: crypto.randomUUID() } });
    check(dep.status === 201, `frozen client B deposit answered ${dep.status}`);
    const sp = await apiCall("POST", "/api/v1/spei/transfer", {
      token: B.token,
      body: { clabe_destino: clabeWithCheck("01218000123456789"), beneficiario_nombre: "Proveedor Externo SA", monto: "10.00", clave_idempotencia: crypto.randomUUID() },
    });
    check(sp.body?.error?.codigo === "OPERACION_NO_DISPONIBLE", `frozen client B SPEI answered ${sp.status} ${sp.body?.error?.codigo}`);
    const bTab = await apiCall("GET", "/api/v1/tablero", { token: B.token });
    check(bTab.body?.boveda?.estado === "ACTIVA", `frozen client B /tablero estado ${bTab.body?.boveda?.estado}, saldo_bloqueado ${bTab.body?.boveda?.saldo_bloqueado}`);
    // Confirm suspicious activity (urgent), then record the UIF folio.
    await o1.goto(`${BASE}/admin/aml?alerta=${bAlert.id}`);
    const dr2 = o1.getByRole("dialog").first();
    await dr2.getByRole("radio", { name: /Confirm suspicious activity/ }).waitFor({ timeout: 15_000 }).catch(() => undefined);
    await dr2.getByRole("radio", { name: /Confirm suspicious activity/ }).check();
    await dr2.getByLabel(/Urgent: 24-hour report/).check();
    await dr2.getByLabel("Dictamen").fill("Se confirma operación inusual: fraccionamiento sin justificación documental del origen de los recursos.");
    await dr2.getByRole("button", { name: "Confirm suspicious activity" }).click();
    await o1.getByRole("dialog", { name: "Confirm suspicious activity?" }).getByRole("button", { name: "Confirm", exact: true }).click();
    await o1.getByText(/Suspicious activity confirmed on alert/).waitFor({ timeout: 10_000 }).catch(() => undefined);
    const conf = resolveRes("CONFIRMAR");
    check(conf?.status === 201 && conf?.resBody?.alerta?.estado === "CONFIRMADO", `CONFIRMAR answered ${conf?.status} ${conf?.resBody?.alerta?.estado ?? JSON.stringify(conf?.resBody)}`);
    await shot(o1, "officer1-B-confirmed");
    const folioInput = dr2.getByLabel("UIF folio");
    await folioInput.waitFor({ timeout: 10_000 }).catch(() => undefined);
    check((await folioInput.count()) > 0, "after confirming, the UIF folio form is offered");
    if (await folioInput.count()) {
      await folioInput.fill(`SITI-${runId % 1_000_000}`);
      await dr2.getByRole("button", { name: "Record UIF folio" }).click();
      await o1.getByText(/UIF folio .* recorded/).waitFor({ timeout: 10_000 }).catch(() => undefined);
      const uif = lastResponse((e) => e.label === "officer1" && e.path === `/api/v1/aml/alertas/${bAlert.id}/reporte-uif`);
      check(uif?.status === 201 && uif?.resBody?.alerta?.folio_uif, `reporte-uif answered ${uif?.status} folio ${uif?.resBody?.alerta?.folio_uif ?? JSON.stringify(uif?.resBody)}`);
    }
    // Request unfreeze keeping the held funds.
    const requestUnfreeze = async (page, scope, keep) => {
      await scope.getByRole("button", { name: /Request unfreeze|Release kept funds/ }).first().click();
      const dlg = page.getByRole("dialog", { name: /Request unfreeze|Release kept funds/ });
      await dlg.waitFor({ timeout: 5000 });
      if (keep) await dlg.getByRole("radio", { name: /Lift but keep funds held/ }).check();
      await dlg.getByLabel("Justification").fill("Reporte a la UIF presentado; la restricción preventiva ya no es necesaria para la investigación.");
      const r = await withResponse(page, (x) => /\/aml\/congelamientos\/[^/]+\/solicitudes$/.test(new URL(x.url()).pathname), () => dlg.getByRole("button", { name: "Send for approval" }).click());
      await page.getByText(/Request #\d+ sent/).first().waitFor({ timeout: 10_000 }).catch(() => undefined);
      return { status: r.status(), body: await r.json().catch(() => null) };
    };
    const decide = async (folio, aprobar) => {
      await o2.goto(`${BASE}/admin/aml?tab=aprobaciones`);
      const row = o2.getByTestId("aml-request").filter({ hasText: new RegExp(`#${folio}(?!\\d)`) }).first();
      await row.waitFor({ timeout: 15_000 });
      await row.getByRole("button", { name: aprobar ? "Approve" : "Reject" }).click();
      const dlg = o2.getByRole("dialog");
      await dlg.getByLabel("Comment").fill(aprobar ? "Revisado: procede conforme a la política vigente." : "No procede todavía: falta el acuse del reporte.");
      const r = await withResponse(o2, (x) => new URL(x.url()).pathname.endsWith("/decision"), () => dlg.getByRole("button", { name: aprobar ? "Approve" : "Reject" }).click());
      await o2.getByText(new RegExp(`Request #${folio} (approved|rejected)`)).first().waitFor({ timeout: 10_000 }).catch(() => undefined);
      return { status: r.status(), body: await r.json().catch(() => null), notice: await o2.getByText(new RegExp(`Request #${folio} (approved|rejected)`)).first().innerText().catch(() => null) };
    };
    const req1 = await requestUnfreeze(o1, dr2, true);
    check(req1.status === 201 && req1.body?.solicitud?.tipo === "LEVANTAR_CONSERVANDO_RETENCIONES", `unfreeze request answered ${req1.status} ${req1.body?.solicitud?.tipo ?? JSON.stringify(req1.body)}`);

    // =======================================================================
    phase("12-four-eyes-reject-approve-release");
    // =======================================================================
    if (req1.body?.solicitud) {
      const rej = await decide(req1.body.solicitud.folio, false);
      check(rej.status === 201 && rej.body?.solicitud?.estado === "RECHAZADA", `reject answered ${rej.status} ${rej.body?.solicitud?.estado ?? JSON.stringify(rej.body)} (${rej.notice})`);
      await o1.goto(`${BASE}/admin/aml?alerta=${bAlert.id}`);
      const dr3 = o1.getByRole("dialog").first();
      await dr3.getByRole("button", { name: "Request unfreeze" }).waitFor({ timeout: 15_000 }).catch(() => undefined);
      const req2 = await requestUnfreeze(o1, dr3, true);
      check(req2.status === 201, `second unfreeze request answered ${req2.status}`, req2.body);
      if (req2.body?.solicitud) {
        const ap = await decide(req2.body.solicitud.folio, true);
        check(ap.status === 201 && ap.body?.congelamiento?.estado === "LEVANTADO", `approve (keep holds) answered ${ap.status}: freeze ${ap.body?.congelamiento?.estado}, held ${ap.body?.congelamiento?.retenido_vigente} (${ap.notice})`, ap.body?.error);
        const bTab2 = await apiCall("GET", "/api/v1/tablero", { token: B.token });
        state.bAfterLift = bTab2.body?.boveda;
        check(Number(bTab2.body?.boveda?.saldo_bloqueado) > 0, `client B keeps ${bTab2.body?.boveda?.saldo_bloqueado} held after a lift that keeps holds`);
        // Release the kept funds from the Frozen clients tab (Lifted).
        await o1.goto(`${BASE}/admin/aml?tab=congelamientos`);
        await o1.getByRole("group", { name: "Restriction state" }).getByRole("button", { name: "Lifted" }).click();
        const frow = o1.getByTestId("aml-freeze-row").filter({ hasText: B.nombre.split(" ")[0] }).first();
        await frow.waitFor({ timeout: 15_000 }).catch(() => undefined);
        check((await frow.count()) > 0, "the Lifted filter lists client B's restriction");
        if (await frow.count()) {
          await frow.click();
          const fd = o1.getByRole("dialog").first();
          await fd.getByRole("button", { name: "Release kept funds" }).waitFor({ timeout: 10_000 }).catch(() => undefined);
          await shot(o1, "officer1-B-lifted-freeze");
          check((await fd.getByRole("button", { name: "Release kept funds" }).count()) > 0, "a lifted restriction with holds offers Release kept funds");
          if (await fd.getByRole("button", { name: "Release kept funds" }).count()) {
            const req3 = await requestUnfreeze(o1, fd, false);
            check(req3.status === 201 && req3.body?.solicitud?.tipo === "LIBERAR_RETENCIONES", `release request answered ${req3.status} ${req3.body?.solicitud?.tipo ?? JSON.stringify(req3.body)}`);
            if (req3.body?.solicitud) {
              const ap3 = await decide(req3.body.solicitud.folio, true);
              check(ap3.status === 201 && ap3.body?.liberaciones?.length > 0, `approve release answered ${ap3.status}: ${ap3.body?.liberaciones?.length} release(s) (${ap3.notice})`, ap3.body?.error);
              const bTab3 = await apiCall("GET", "/api/v1/tablero", { token: B.token });
              check(Number(bTab3.body?.boveda?.saldo_bloqueado) === 0, `client B held balance after release: ${bTab3.body?.boveda?.saldo_bloqueado}`);
            }
          }
        }
      }
    }
  }

  // =========================================================================
  phase("13-contract-probes");
  // =========================================================================
  // Endpoints typed in endpoints.ts without a console screen yet: called from an officer page so they land in network.json.
  await o1.goto(`${BASE}/admin/aml`);
  await o1.waitForTimeout(1000);
  const probes = {};
  probes.notificaciones = await pageFetch(o1, "GET", "/api/v1/aml/notificaciones?limite=200");
  probes.reglasTodas = await pageFetch(o1, "GET", "/api/v1/aml/reglas?incluir_inactivas=true");
  probes.relevantes = await pageFetch(o1, "GET", "/api/v1/aml/relevantes");
  probes.bitacora = await pageFetch(o1, "GET", "/api/v1/aml/bitacora/verificacion");
  probes.congelamientosTodos = await pageFetch(o1, "GET", "/api/v1/aml/congelamientos?estado=TODOS");
  probes.solicitudesTodas = await pageFetch(o1, "GET", "/api/v1/aml/solicitudes?estado=TODOS");
  probes.propuesta = await pageFetch(o1, "POST", "/api/v1/aml/propuestas", {
    tipo: "PARAMETROS",
    cambios: { dias_aviso_plazo: 8 },
    justificacion: "Prueba e2e: ampliar el aviso de plazo UIF a ocho días naturales.",
    clave_idempotencia: crypto.randomUUID(),
  });
  probes.propuestas = await pageFetch(o1, "GET", "/api/v1/aml/propuestas");
  if (probes.propuesta.body?.propuesta?.id) {
    probes.decisionPropuesta = await pageFetch(o2, "POST", `/api/v1/aml/propuestas/${probes.propuesta.body.propuesta.id}/decision`, {
      aprobar: false,
      comentario: "Rechazada: solo es una prueba e2e del contrato.",
      clave_idempotencia: crypto.randomUUID(),
    });
  }
  for (const [k, v] of Object.entries(probes)) check(v.status < 300, `probe ${k} answered ${v.status}`, v.status < 300 ? undefined : v.body);
  check(probes.bitacora.body?.valido === true, `audit log verification: ${JSON.stringify(probes.bitacora.body)}`);
} catch (e) {
  check(false, `aborted: ${e.stack ?? e}`);
} finally {
  await new Promise((r) => setTimeout(r, 1000));
  await browser.close();
  const amlTraffic = traffic.filter((e) => e.path?.startsWith("/api/v1/aml"));
  writeFileSync(`${OUT}/network.json`, JSON.stringify(traffic, null, 1));
  writeFileSync(
    `${OUT}/results.json`,
    JSON.stringify({ runId, client: client.email, clientId, checks, consoleErrors, failedRequests, state, amlRequests: amlTraffic.length }, null, 1),
  );
  const failed = checks.filter((c) => !c.ok);
  console.log(`\n${checks.length - failed.length}/${checks.length} checks passed; ${consoleErrors.length} console error(s); ${failedRequests.length} failed request(s)`);
  for (const e of consoleErrors) console.log(`  console [${e.label}/${e.step}] ${e.text}${e.url ? ` (${e.url})` : ""}`);
  for (const f of failedRequests) console.log(`  failed  [${f.label}/${f.step}] ${f.method} ${f.url} ${f.error}`);
  exitCode = failed.length ? 1 : 0;
}
process.exit(exitCode);
