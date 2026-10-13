"use client";

import { useEffect, useRef, useState } from "react";
import {
  ArrowUp,
  CheckCircle2,
  Download,
  Fingerprint,
  GitBranch,
  Loader2,
  Server,
  ShieldCheck,
  XCircle,
} from "lucide-react";

import { HashText, Money, formatDateTime } from "@/components/ui/data";
import { Alert, Badge, Button, Card, CardHeader, cx } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { PruebaReserva, ResultadoVerificacionPrueba } from "@/lib/api/types";
import { compareAmounts } from "@/lib/decimal";
import { verifyInclusion, type VerificationResult } from "@/lib/merkle";

type Phase = "idle" | "fetching" | "verifying" | "done" | "error";

const STEP_DELAY_MS = 140;

/**
 * Proof of Reserve audit. The browser downloads the account's inclusion
 * proof, then recomputes every hash itself (WebCrypto SHA-256), from the
 * leaf up to the root, and compares the result with the root and total
 * liabilities the bank published. The server is not trusted for the
 * verdict; its own check is shown only as a second opinion.
 */
export function ProofOfReservePanel({ token }: { token: string }) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [proof, setProof] = useState<PruebaReserva | null>(null);
  const [local, setLocal] = useState<VerificationResult | null>(null);
  const [server, setServer] = useState<ResultadoVerificacionPrueba | null>(null);
  const [revealed, setRevealed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => () => {
    if (timer.current) clearInterval(timer.current);
  }, []);

  const run = async () => {
    if (timer.current) clearInterval(timer.current);
    setPhase("fetching");
    setError(null);
    setPending(null);
    setLocal(null);
    setServer(null);
    setRevealed(0);
    try {
      const p = await api.reserveProof(token);
      setProof(p);
      setPhase("verifying");
      const result = await verifyInclusion(p.hoja, p.ruta, p.corte.merkle_root, p.corte.total_pasivo);
      setLocal(result);
      api.verifyProof(p).then(setServer).catch(() => setServer(null));

      // Reveal the path one level at a time (leaf, each step, root).
      const total = result.steps.length + 2;
      const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      if (reduced) {
        setRevealed(total);
        setPhase("done");
        return;
      }
      timer.current = setInterval(() => {
        setRevealed((n) => {
          if (n + 1 >= total) {
            if (timer.current) clearInterval(timer.current);
            setPhase("done");
          }
          return n + 1;
        });
      }, STEP_DELAY_MS);
    } catch (e) {
      // A brand-new account is not in a snapshot yet: informational, not a failure.
      if (e instanceof ApiError && e.code === "PRUEBA_PENDIENTE") setPending(describeError(e));
      else setError(describeError(e));
      setPhase("error");
    }
  };

  const download = () => {
    if (!proof) return;
    const blob = new Blob([JSON.stringify(proof, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `proof-of-reserve-${proof.corte.id}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const busy = phase === "fetching" || phase === "verifying";
  const reserveCovers = proof ? compareAmounts(proof.corte.total_reserva, proof.corte.total_pasivo) >= 0 : false;
  const verdict = local && phase === "done" ? local.valid && reserveCovers && local.leafHashMatches !== false : null;

  return (
    <Card aria-labelledby="por-title">
      <CardHeader
        icon={<Fingerprint className="size-4.5" aria-hidden />}
        eyebrow="Cryptographic audit"
        title={<span id="por-title">Proof of Reserve</span>}
        action={
          proof ? (
            <Button variant="ghost" size="sm" onClick={download} aria-label="Download proof as JSON">
              <Download className="size-3.5" aria-hidden /> JSON
            </Button>
          ) : null
        }
      />
      <div className="space-y-5 p-5">
        <p className="text-sm leading-relaxed text-ink-2">
          Every account balance is a leaf in a <strong>Merkle sum tree</strong>. Your browser rebuilds the path from your
          leaf to the root the bank published, hashing each level with SHA-256. If any balance were hidden or altered,
          the root would not match.
        </p>

        <Button onClick={run} loading={busy} size="lg" className="w-full sm:w-auto">
          <ShieldCheck className="size-4" aria-hidden />
          {phase === "done" || phase === "error" ? "Verify again" : "Verify My Merkle Proof"}
        </Button>

        {error ? <Alert tone="error">{error}</Alert> : null}
        {pending ? (
          <Alert tone="info" title="Not in a snapshot yet">
            {pending}
          </Alert>
        ) : null}

        {phase === "fetching" ? (
          <p className="flex items-center gap-2 text-sm text-muted">
            <Loader2 className="size-4 animate-spin" aria-hidden /> Fetching your inclusion proof…
          </p>
        ) : null}

        {proof && local ? (
          <div className="space-y-5" aria-live="polite">
            {verdict !== null ? <Verdict ok={verdict} local={local} proof={proof} /> : null}

            <dl className="grid grid-cols-2 gap-x-4 gap-y-3 rounded-lg bg-surface-2 p-3 text-xs sm:grid-cols-3">
              <Meta label="Snapshot" value={formatDateTime(proof.corte.creado_en)} />
              <Meta label="Accounts in tree" value={proof.corte.num_hojas.toLocaleString("en-US")} />
              <Meta label="Your leaf index" value={`#${proof.indice}`} />
              <Meta label="Total liabilities" value={<Money value={proof.corte.total_pasivo} />} />
              <Meta label="Cash in custody" value={<Money value={proof.corte.total_reserva} />} />
              <Meta
                label="Anchored to ledger"
                value={
                  <span className="inline-flex items-center gap-1">
                    #{proof.corte.secuencia_ledger}
                    <HashText value={proof.corte.hash_ledger} head={6} tail={4} label="ledger anchor hash" />
                  </span>
                }
              />
            </dl>

            <ol className="relative space-y-2" aria-label="Merkle path from your leaf to the root">
              <PathNode
                visible={revealed >= 1}
                kind="leaf"
                title="Your leaf"
                hash={local.leafHash}
                sum={proof.hoja.saldo}
                detail={
                  <>
                    account <span className="font-mono">{proof.hoja.cuenta_id.slice(0, 8)}…</span> · salt{" "}
                    <HashText value={proof.hoja.sal} head={6} tail={4} label="salt" className="text-[11px]" />
                    {local.leafHashMatches === false ? (
                      <span className="text-danger"> · does not match the hash sent by the bank</span>
                    ) : null}
                  </>
                }
              />
              {local.steps.map((step, i) => (
                <PathNode
                  key={step.level}
                  visible={revealed >= i + 2}
                  kind="node"
                  title={`Level ${step.level}`}
                  hash={step.nodeHash}
                  sum={step.nodeSum}
                  detail={
                    <>
                      combined with sibling on the {step.side === "IZQUIERDA" ? "left" : "right"}{" "}
                      <HashText value={step.siblingHash} head={8} tail={4} label="sibling hash" className="text-[11px]" />{" "}
                      (sum <Money value={step.siblingSum} />)
                    </>
                  }
                />
              ))}
              <PathNode
                visible={revealed >= local.steps.length + 2}
                kind={local.rootMatches ? "root-ok" : "root-bad"}
                title="Computed root"
                hash={local.rootHash}
                sum={local.rootSum}
                detail={
                  <>
                    published root{" "}
                    <HashText value={proof.corte.merkle_root} head={8} tail={4} label="published root" className="text-[11px]" />{" "}
                    {local.rootMatches ? "· identical" : "· DIFFERENT"}
                  </>
                }
              />
            </ol>

            <div className="flex flex-wrap gap-2 text-[11px]">
              <Badge tone="neutral" icon={<GitBranch className="size-3" aria-hidden />}>
                {proof.corte.algoritmo}
              </Badge>
              <Badge tone="neutral">Hashed in your browser · {local.engine === "webcrypto" ? "WebCrypto" : "JS SHA-256"}</Badge>
              {server ? (
                <Badge tone={server.valida ? "verified" : "danger"} icon={<Server className="size-3" aria-hidden />}>
                  Server check: {server.valida ? "agrees" : "disagrees"}
                </Badge>
              ) : null}
            </div>
          </div>
        ) : null}
      </div>
    </Card>
  );
}

function Verdict({ ok, local, proof }: { ok: boolean; local: VerificationResult; proof: PruebaReserva }) {
  const changed = compareAmounts(proof.saldo_actual, proof.hoja.saldo) !== 0;
  return (
    <div
      className={cx(
        "flex gap-3 rounded-lg border p-4 animate-reveal",
        ok ? "border-verified/30 bg-verified-soft" : "border-danger/30 bg-danger-soft",
      )}
      data-testid="proof-verdict"
    >
      {ok ? (
        <CheckCircle2 className="size-6 shrink-0 text-verified" aria-hidden />
      ) : (
        <XCircle className="size-6 shrink-0 text-danger" aria-hidden />
      )}
      <div className="space-y-1 text-sm">
        <p className={cx("font-semibold", ok ? "text-verified" : "text-danger")}>
          {ok ? "Verified: your balance is included and fully backed" : "Verification failed"}
        </p>
        <p className="text-ink-2">
          {ok ? (
            <>
              Your balance of <Money value={proof.hoja.saldo} /> is part of the published root, and the sum of all
              balances (<Money value={local.rootSum} />) equals the cash held in custody.
            </>
          ) : (
            <>
              {!local.rootMatches ? "The computed root does not match the published root. " : null}
              {!local.totalMatches ? "The tree's total does not match the published liabilities. " : null}
              Download the proof and contact the bank.
            </>
          )}
        </p>
        {changed ? (
          <p className="text-xs text-muted">
            Your balance has changed since this snapshot (now <Money value={proof.saldo_actual} />). The next snapshot
            will include it.
          </p>
        ) : null}
      </div>
    </div>
  );
}

function Meta({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-muted">{label}</dt>
      <dd className="mt-0.5 truncate font-medium text-ink">{value}</dd>
    </div>
  );
}

function PathNode({
  visible,
  kind,
  title,
  hash,
  sum,
  detail,
}: {
  visible: boolean;
  kind: "leaf" | "node" | "root-ok" | "root-bad";
  title: string;
  hash: string;
  sum: string;
  detail: React.ReactNode;
}) {
  if (!visible) return <li className="h-[58px] rounded-lg border border-dashed border-border" aria-hidden />;
  const tone =
    kind === "root-ok"
      ? "border-verified/40 bg-verified-soft"
      : kind === "root-bad"
        ? "border-danger/40 bg-danger-soft"
        : kind === "leaf"
          ? "border-accent/40 bg-accent-soft"
          : "border-border bg-surface";
  return (
    <li className={cx("relative rounded-lg border px-3 py-2 animate-reveal", tone)}>
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-0.5">
        <span className="flex items-center gap-1.5 text-xs font-semibold">
          {kind === "node" ? <ArrowUp className="size-3 text-muted" aria-hidden /> : null}
          {kind === "root-ok" ? <CheckCircle2 className="size-3.5 text-verified" aria-hidden /> : null}
          {kind === "root-bad" ? <XCircle className="size-3.5 text-danger" aria-hidden /> : null}
          {title}
        </span>
        <span className="text-xs tabular-nums">
          Σ <Money value={sum} />
        </span>
      </div>
      <HashText value={hash} head={20} tail={12} label={`${title} hash`} className="mt-0.5 text-[11px] text-ink-2" />
      <p className="mt-0.5 text-[11px] text-muted">{detail}</p>
    </li>
  );
}
