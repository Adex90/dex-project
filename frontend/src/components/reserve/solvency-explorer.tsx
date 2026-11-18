"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";

import { LeafAuditor, type AuditOutcome, type ProofLoad } from "@/components/reserve/leaf-auditor";
import { ProofTrace, type Trace } from "@/components/reserve/proof-trace";
import { SolvencyHeader } from "@/components/reserve/solvency-header";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { CorteReserva, PruebaReserva, ResultadoVerificacionPrueba } from "@/lib/api/types";
import { defaultEngine, verifyInclusion, verifyPositionInclusion } from "@/lib/merkle-verifier";
import { proofVerdict } from "@/lib/proof-verdict";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useVaultStore } from "@/store/vault";

/** Time between two revealed nodes of the proof trace. */
const STEP_DELAY_MS = 160;

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/**
 * The Reserve tab: the published snapshot for everyone (header), then the
 * customer's own leaf and the browser-side audit of it.
 *
 * "Verify My Inclusion" always fetches a fresh proof, recomputes every hash
 * locally (cash tree and one tree per asset position), and judges it against
 * the snapshot's public copy fetched without credentials. The server's own
 * check is shown as a second opinion only.
 */
export function SolvencyExplorer({ token, accountId }: { token: string; accountId: string | null }) {
  const router = useRouter();
  const holdings = useVaultStore((s) => s.holdings);
  const [cut, setCut] = useState<CorteReserva | null>(null);
  const [cutMissing, setCutMissing] = useState(false);
  const [cutError, setCutError] = useState<string | null>(null);
  const [load, setLoad] = useState<ProofLoad>({ status: "loading" });
  const [audit, setAudit] = useState<AuditOutcome | null>(null);
  const [traces, setTraces] = useState<Trace[]>([]);
  const [selected, setSelected] = useState("cash");
  const [revealed, setRevealed] = useState(Number.POSITIVE_INFINITY);
  const [verifying, setVerifying] = useState(false);
  const [verifyError, setVerifyError] = useState<string | null>(null);
  const [server, setServer] = useState<ResultadoVerificacionPrueba | null>(null);
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);
  const run = useRef(0);

  const expire = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  const loadCut = useCallback(async (signal?: AbortSignal) => {
    setCutError(null);
    setCutMissing(false);
    try {
      setCut(await api.latestCut(signal));
    } catch (error) {
      if (isAbort(error)) return;
      if (error instanceof ApiError && error.status === 404) setCutMissing(true);
      else setCutError(describeError(error));
    }
  }, []);

  /** GET /reserva/prueba; null when there is no proof to show. */
  const loadProof = useCallback(async (): Promise<PruebaReserva | null> => {
    try {
      const proof = await api.reserveProof(token);
      setLoad({ status: "ready", proof });
      return proof;
    } catch (error) {
      if (error instanceof ApiError && error.isUnauthorized) {
        expire();
        return null;
      }
      // A brand-new account is not in a snapshot yet: informational, not a failure.
      if (error instanceof ApiError && error.code === "PRUEBA_PENDIENTE") {
        setLoad({ status: "pending", message: describeError(error) });
      } else {
        setLoad({ status: "error", message: describeError(error) });
      }
      return null;
    }
  }, [token, expire]);

  useEffect(() => {
    const ctrl = new AbortController();
    void loadCut(ctrl.signal);
    void loadProof();
    return () => ctrl.abort();
  }, [loadCut, loadProof]);

  useEffect(
    () => () => {
      if (timer.current) clearInterval(timer.current);
    },
    [],
  );

  const reveal = (total: number) => {
    if (timer.current) clearInterval(timer.current);
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setRevealed(total);
      return;
    }
    setRevealed(1);
    timer.current = setInterval(() => {
      setRevealed((n) => {
        if (n + 1 >= total && timer.current) clearInterval(timer.current);
        return n + 1;
      });
    }, STEP_DELAY_MS);
  };

  const verify = async () => {
    const id = ++run.current;
    if (timer.current) clearInterval(timer.current);
    setVerifying(true);
    setVerifyError(null);
    setAudit(null);
    setTraces([]);
    setServer(null);
    try {
      const proof = await loadProof();
      if (!proof || id !== run.current) return;
      const engine = defaultEngine();
      const [cash, positions, publicCut] = await Promise.all([
        verifyInclusion(proof.hoja, proof.ruta, proof.corte.merkle_root, proof.corte.total_pasivo, {
          engine,
          shape: { index: proof.indice, leafCount: proof.corte.num_hojas },
        }),
        Promise.all(
          (proof.posiciones ?? []).map(async (p) => {
            const asset = (proof.corte.activos ?? []).find((a) => a.ticker === p.hoja.ticker) ?? null;
            const result = await verifyPositionInclusion(
              p.hoja,
              p.ruta,
              asset?.merkle_root ?? "",
              asset?.total_cantidad ?? "0",
              { engine, shape: asset ? { index: p.indice, leafCount: asset.num_hojas } : undefined },
            );
            return { ticker: p.hoja.ticker, cuenta_id: p.hoja.cuenta_id, result };
          }),
        ),
        // The public copy, fetched without credentials. A failure leaves the
        // verdict negative instead of trusting the proof's own copy.
        api.publicCut(proof.corte.id).catch(() => null),
      ]);
      if (id !== run.current) return;
      const verdict = proofVerdict({
        local: cash,
        proof,
        positions: positions.map(({ ticker, cuenta_id, result }) => ({ ticker, cuenta_id, local: result })),
        accountId,
        publicCut,
      });
      const publicIssue =
        verdict.issues.includes("PUBLIC_CUT_MISMATCH") || verdict.issues.includes("PUBLIC_CUT_UNAVAILABLE");
      setAudit({
        proof,
        cash,
        verdict,
        engine,
        publicCopy:
          publicCut === null ? "unavailable" : verdict.issues.includes("PUBLIC_CUT_MISMATCH") ? "differs" : "matches",
      });
      setTraces([
        {
          key: "cash",
          label: "Cash",
          kind: "cash",
          result: cash,
          publishedRoot: publicCut?.merkle_root ?? proof.corte.merkle_root,
          match:
            cash.valid &&
            !publicIssue &&
            !verdict.issues.includes("WRONG_ACCOUNT") &&
            !verdict.issues.includes("LEAF_HASH_MISMATCH"),
        },
        ...positions.map((p, i) => ({
          key: p.ticker,
          label: p.ticker,
          kind: "position" as const,
          result: p.result,
          publishedRoot:
            ((publicCut ?? proof.corte).activos ?? []).find((a) => a.ticker === p.ticker)?.merkle_root ?? null,
          match: (verdict.positions[i]?.ok ?? false) && !publicIssue,
        })),
      ]);
      setSelected("cash");
      reveal(cash.steps.length + 2);
      api.verifyProof(proof).then(
        (s) => {
          if (id === run.current) setServer(s);
        },
        () => undefined,
      );
    } catch (error) {
      if (id === run.current) {
        setVerifyError(
          `This proof cannot be verified: ${error instanceof Error ? error.message : "it is malformed"}. Download it and contact the bank.`,
        );
      }
    } finally {
      if (id === run.current) setVerifying(false);
    }
  };

  const select = (key: string) => {
    if (timer.current) clearInterval(timer.current);
    setRevealed(Number.POSITIVE_INFINITY);
    setSelected(key);
  };

  return (
    <div className="space-y-6" data-testid="solvency-explorer">
      <SolvencyHeader cut={cut} missing={cutMissing} error={cutError} onRetry={() => void loadCut()} />
      <div className="grid grid-cols-1 items-start gap-6 xl:grid-cols-[minmax(0,5fr)_minmax(0,6fr)]">
        <LeafAuditor
          load={load}
          holdings={holdings}
          accountId={accountId}
          audit={audit}
          server={server}
          verifying={verifying}
          verifyError={verifyError}
          onVerify={() => void verify()}
          onRetry={() => void loadProof()}
        />
        <ProofTrace traces={traces} selected={selected} onSelect={select} revealed={revealed} />
      </div>
    </div>
  );
}
