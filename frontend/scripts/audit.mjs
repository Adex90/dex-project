// The frontend's dependency gate for the whole tree, build tools included (they run code during the build). It fails on
// any high or critical advisory unless audit-exceptions.json excepts it, and an exception holds only while all of this
// is true:
//   * npm still reports the advisory with the exact vulnerable range the exception was approved for;
//   * the package has no release newer than the exception's `newestRelease` (a candidate fix, a backport included);
//   * today (UTC) is no later than its `expires` date;
//   * every vulnerable copy of the package in package-lock.json is a dev dependency.
// So an exception lapses on its own at an upstream fix or on its date, whichever comes first, and it can never cover a
// runtime dependency. Runtime dependencies are gated before this anyway, with no exceptions at all: the Dockerfile's
// audit stage runs `npm audit --omit=dev --audit-level=moderate` first.
//
//   node scripts/audit.mjs              # runs `npm audit --json` in the frontend directory
//   node scripts/audit.mjs report.json  # checks a saved `npm audit --json` report instead
//
// Exit status: 0 passed, 1 an advisory is not excepted, 2 the check could not run (it fails closed).
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const GATED = new Set(["high", "critical"]);
const FIELDS = ["id", "package", "range", "newestRelease", "scope", "statement", "approved", "expires"];
const today = new Date().toISOString().slice(0, 10);

function fatal(message) {
  console.error(`audit: cannot check: ${message}`);
  process.exit(2);
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fatal(`${path}: ${error.message}`);
  }
}

// npm exits non-zero whenever it reports something (findings, a 404); its JSON is still on stdout.
function npmJson(args) {
  let stdout;
  try {
    stdout = execFileSync("npm", [...args, "--json"], {
      cwd: ROOT,
      encoding: "utf8",
      maxBuffer: 64 << 20,
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (error) {
    if (!error.stdout) fatal(`npm ${args.join(" ")}: ${error.stderr || error.message}`);
    stdout = error.stdout;
  }
  try {
    return JSON.parse(stdout);
  } catch {
    fatal(`npm ${args.join(" ")} printed no JSON`);
  }
}

function isDay(value) {
  try {
    return /^\d{4}-\d{2}-\d{2}$/.test(value) && new Date(value).toISOString().slice(0, 10) === value;
  } catch {
    return false;
  }
}

const { exceptions } = readJson(`${ROOT}/audit-exceptions.json`);
if (!Array.isArray(exceptions)) fatal("audit-exceptions.json has no exceptions array");
for (const e of exceptions) {
  const missing = FIELDS.filter((field) => typeof e[field] !== "string" || e[field].trim() === "");
  if (missing.length) fatal(`audit-exceptions.json: ${e.id ?? "an exception"} has no ${missing.join(", ")}`);
  if (!isDay(e.expires)) fatal(`audit-exceptions.json: ${e.id}: expires is not a YYYY-MM-DD date`);
}

const report = process.argv[2] ? readJson(process.argv[2]) : npmJson(["audit"]);
if (report.error) fatal(`npm audit: ${report.error.summary ?? JSON.stringify(report.error)}`);
if (report.auditReportVersion !== 2 || typeof report.vulnerabilities !== "object") {
  fatal("not an npm audit report (version 2)");
}
const { vulnerabilities } = report;
const lock = readJson(`${ROOT}/package-lock.json`);

// Each vulnerable package lists advisories of its own (objects) and the vulnerable packages it inherits from (names).
const advisories = new Map();
for (const [name, vulnerability] of Object.entries(vulnerabilities)) {
  for (const via of vulnerability.via) {
    if (typeof via !== "object") continue;
    const id = String(via.url ?? via.source).split("/").pop();
    const advisory = { id, name, severity: via.severity, range: via.range, title: via.title, nodes: vulnerability.nodes };
    advisories.set(`${name} ${id}`, advisory);
  }
}

// npm's own verdict, inheritance included, must be explained by the advisories read above: a package it reports as high
// or critical with no such advisory behind it means this reading of the report is wrong, so the gate fails closed.
function roots(name, seen = new Set()) {
  if (seen.has(name)) return [];
  seen.add(name);
  return (vulnerabilities[name]?.via ?? []).flatMap((via) => (typeof via === "object" ? [via] : roots(via, seen)));
}
for (const [name, vulnerability] of Object.entries(vulnerabilities)) {
  if (GATED.has(vulnerability.severity) && !roots(name).some((via) => GATED.has(via.severity))) {
    fatal(`npm reports ${name} as ${vulnerability.severity} with no high or critical advisory behind it`);
  }
}

// Releases of a package published after `release`: any of them may be the fix.
function releasesAfter(name, release) {
  const times = npmJson(["view", name, "time"]);
  if (times.error) fatal(`npm view ${name} time: ${times.error.summary ?? times.error.code}`);
  if (!times[release]) fatal(`npm view ${name} time: no release ${release}`);
  return Object.keys(times).filter((version) => !["created", "modified"].includes(version) && times[version] > times[release]);
}

let failed = false;
function fail(where, why) {
  console.error(`FAIL ${where}${why ? ` (${why})` : ""}`);
  failed = true;
}

const gated = [...advisories.values()].filter((a) => GATED.has(a.severity));
for (const a of gated) {
  const where = `${a.severity} ${a.id} ${a.name} ${a.range}: ${a.title}`;
  const exception = exceptions.find((e) => e.id === a.id && e.package === a.name);
  if (!exception) {
    fail(where);
    continue;
  }
  if (exception.range !== a.range) {
    fail(where, `its exception covers ${exception.range}: the advisory changed, so review it`);
    continue;
  }
  if (exception.expires < today) {
    fail(where, `its exception expired at the end of ${exception.expires}`);
    continue;
  }
  const runtime = (a.nodes ?? []).filter((node) => lock.packages?.[node]?.dev !== true);
  if (runtime.length || !a.nodes?.length) {
    fail(where, `its exception is for dev tooling only, and package-lock.json has it outside: ${runtime.join(", ")}`);
    continue;
  }
  const released = releasesAfter(a.name, exception.newestRelease);
  if (released.length) {
    fail(where, `${a.name} ${released.join(", ")} came out after ${exception.newestRelease}: upgrade, or review it`);
    continue;
  }
  console.log(`excepted through ${exception.expires} (${exception.scope}): ${where}`);
}
for (const e of exceptions) {
  if (!advisories.has(`${e.package} ${e.id}`)) console.log(`note: ${e.id} (${e.package}) is no longer reported: remove its exception`);
}
const others = advisories.size - gated.length;
console.log(
  failed
    ? "audit: FAILED"
    : `audit: passed (${gated.length} high or critical advisories, all excepted; ${others} below high, not gated here)`,
);
process.exit(failed ? 1 : 0);
