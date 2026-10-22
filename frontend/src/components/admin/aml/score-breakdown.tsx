"use client";

import { Gauge } from "lucide-react";

import { LevelBadge, MxTime, Section } from "@/components/admin/aml/parts";
import { Money } from "@/components/ui/data";
import { Alert, Badge, cx } from "@/components/ui/primitives";
import type { AlertaAml, CalculoAml, EvaluacionAml, FactorAml } from "@/lib/api/types";
import {
  BLOCK_REASON_LABEL,
  CHANNEL_LABEL,
  DIRECTION_LABEL,
  EVALUATION_ORIGIN_LABEL,
  RESULT_LABEL,
  factorRows,
  formatScore,
  isScoringError,
  strongestEvaluation,
  type Thresholds,
} from "@/lib/aml";

function Evidence({ evidencia }: { evidencia: Record<string, unknown> | null }) {
  if (!evidencia || Object.keys(evidencia).length === 0) return null;
  return (
    <details className="mt-1 text-[11px] text-muted">
      <summary className="cursor-pointer select-none hover:text-ink-2">Evidence</summary>
      <dl className="mt-1 grid grid-cols-[max-content_1fr] gap-x-3 gap-y-0.5 rounded-md bg-surface-2 p-2 font-mono">
        {Object.entries(evidencia).map(([key, value]) => (
          <div key={key} className="contents">
            <dt>{key}</dt>
            <dd className="min-w-0 break-all text-ink-2">{typeof value === "string" ? value : JSON.stringify(value)}</dd>
          </div>
        ))}
      </dl>
    </details>
  );
}

function FactorBar({
  row,
  factor,
}: {
  row: ReturnType<typeof factorRows>[number];
  factor: FactorAml | undefined;
}) {
  const primary = row.tipo === "PRIMARIA";
  return (
    <li className="grid grid-cols-[minmax(0,11rem)_1fr_3.5rem] items-start gap-3">
      <div className="min-w-0 pt-0.5">
        <span className="block truncate font-mono text-[11px] text-ink" title={row.codigo}>
          {row.codigo}
        </span>
        {factor?.permite_congelar ? (
          <span className="text-[10px] font-medium text-danger">May restrict</span>
        ) : null}
        <Evidence evidencia={factor?.evidencia ?? null} />
      </div>
      <div className="relative mt-1 h-2.5 rounded-full bg-surface-2">
        <div
          className={cx("absolute inset-y-0 left-0 rounded-full", primary ? "bg-accent" : "bg-info/70")}
          style={{ width: `${row.width * 100}%` }}
        />
      </div>
      <span className="text-right font-mono text-xs font-semibold text-ink tabular-nums">+{formatScore(row.puntos)}</span>
    </li>
  );
}

function Breakdown({ calculo, thresholds }: { calculo: CalculoAml; thresholds: Thresholds }) {
  const rows = factorRows(calculo);
  const byCode = new Map(calculo.factores.map((f) => [`${f.tipo}:${f.codigo}`, f]));
  const primaries = rows.filter((r) => r.tipo === "PRIMARIA");
  const modifiers = rows.filter((r) => r.tipo === "MODIFICADOR");
  const scale = Math.max(100, Number(calculo.puntaje_bruto) || 0);
  const criticalAt = Math.min(100, (Number(thresholds.critico) / scale) * 100);
  const totalWidth = Math.min(100, ((Number(calculo.puntaje_bruto) || 0) / scale) * 100);
  const v = calculo.ventana;

  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <p className="text-xs font-medium text-ink-2">Rules that fired</p>
        {primaries.length ? (
          <ul className="space-y-2">
            {primaries.map((r) => (
              <FactorBar key={`p:${r.codigo}`} row={r} factor={byCode.get(`PRIMARIA:${r.codigo}`)} />
            ))}
          </ul>
        ) : (
          <p className="text-xs text-muted">No rule fired on this evaluation.</p>
        )}
      </div>
      {modifiers.length ? (
        <div className="space-y-2">
          <p className="text-xs font-medium text-ink-2">
            Profile modifiers <span className="font-normal text-muted">(count only when a rule fires)</span>
          </p>
          <ul className="space-y-2">
            {modifiers.map((r) => (
              <FactorBar key={`m:${r.codigo}`} row={r} factor={byCode.get(`MODIFICADOR:${r.codigo}`)} />
            ))}
          </ul>
        </div>
      ) : null}

      <div className="space-y-1.5 rounded-lg border border-border p-3">
        <div className="relative h-3 rounded-full bg-surface-2" aria-hidden>
          <div className="absolute inset-y-0 left-0 rounded-full bg-danger/80" style={{ width: `${totalWidth}%` }} />
          <div className="absolute -inset-y-1 w-0 border-l-2 border-dashed border-ink" style={{ left: `${criticalAt}%` }} />
        </div>
        <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
          <span className="text-muted">
            Critical line at <span className="font-semibold text-ink">{formatScore(thresholds.critico)}</span>
          </span>
          <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <span>
              Raw total <span className="font-mono font-semibold text-ink">{formatScore(calculo.puntaje_bruto)}</span>
            </span>
            <span>
              Score <span className="font-mono font-semibold text-ink">{formatScore(calculo.puntaje)}</span>
              {Number(calculo.puntaje_bruto) > Number(calculo.puntaje) ? <span className="text-muted"> (capped at 100)</span> : null}
            </span>
            <LevelBadge level={calculo.nivel} />
          </span>
        </div>
        <p className="text-xs text-muted">
          {calculo.congelar
            ? "Automatic restriction criteria met: critical score and a rule that may restrict."
            : "Automatic restriction criteria not met."}
          {calculo.dominante ? ` Dominant rule: ${calculo.dominante}.` : ""}
        </p>
      </div>

      <dl className="grid grid-cols-2 gap-3 rounded-lg bg-surface-2 p-3 text-xs sm:grid-cols-4">
        <div>
          <dt className="text-muted">Window</dt>
          <dd className="font-semibold text-ink">{v.horas} h</dd>
        </div>
        <div>
          <dt className="text-muted">Incoming</dt>
          <dd className="font-semibold text-ink">
            {v.entradas_n} · <Money value={v.entradas_monto} decimals={0} />
          </dd>
        </div>
        <div>
          <dt className="text-muted">Outgoing</dt>
          <dd className="font-semibold text-ink">
            {v.salidas_n} · <Money value={v.salidas_monto} decimals={0} />
          </dd>
        </div>
        <div>
          <dt className="text-muted">Pattern funds</dt>
          <dd className="font-semibold text-ink">
            <Money value={calculo.monto_patron} decimals={0} />
          </dd>
        </div>
      </dl>
    </div>
  );
}

/**
 * Why the alert scored what it did: the evaluation with the highest score,
 * one bar per factor (primaries and modifiers apart), the raw total, the
 * capped score and the critical line. Scores are computed by the database
 * from ledger data; this panel only displays them.
 */
export function ScoreBreakdown({
  alerta,
  evaluaciones,
  thresholds,
}: {
  alerta: AlertaAml;
  evaluaciones: EvaluacionAml[];
  thresholds: Thresholds;
}) {
  const best = strongestEvaluation(evaluaciones);
  const failed = evaluaciones.filter((e) => isScoringError(e.factores));

  return (
    <Section title="Score breakdown" icon={<Gauge className="size-3.5" aria-hidden />}>
      {failed.length > 0 ? <Alert tone="warning">Scoring failed; re-evaluated by the monitor.</Alert> : null}
      {best ? (
        <>
          <p className="text-xs text-muted">
            Evaluation of <MxTime at={best.creado_en} /> · {CHANNEL_LABEL[best.canal]} · {DIRECTION_LABEL[best.sentido]} ·{" "}
            <Money value={best.monto} /> · {RESULT_LABEL[best.resultado]}
            {evaluaciones.length > 1 ? ` · highest of ${evaluaciones.length} evaluations` : ""}
          </p>
          <Breakdown calculo={best.factores} thresholds={thresholds} />
        </>
      ) : (
        <p className="text-sm text-ink-2">
          No automatic score breakdown for this alert. Recorded score:{" "}
          <span className="font-mono font-semibold">{formatScore(alerta.puntaje)}</span>.
        </p>
      )}
      {evaluaciones.length > 0 ? (
        <details className="text-xs">
          <summary className="cursor-pointer text-muted select-none hover:text-ink-2">
            All evaluations ({evaluaciones.length})
          </summary>
          <div className="mt-2 overflow-x-auto">
            <table className="w-full min-w-[560px] text-left">
              <thead>
                <tr className="border-b border-border text-[11px] text-muted">
                  <th className="py-1 pr-3 font-medium">Time</th>
                  <th className="py-1 pr-3 font-medium">Channel</th>
                  <th className="py-1 pr-3 text-right font-medium">Amount</th>
                  <th className="py-1 pr-3 font-medium">Score</th>
                  <th className="py-1 pr-3 font-medium">Result</th>
                  <th className="py-1 font-medium">Origin</th>
                </tr>
              </thead>
              <tbody>
                {evaluaciones.map((e) => (
                  <tr key={e.id} className="border-b border-border/60">
                    <td className="py-1 pr-3 text-ink-2">
                      <MxTime at={e.creado_en} />
                    </td>
                    <td className="py-1 pr-3">{CHANNEL_LABEL[e.canal]}</td>
                    <td className="py-1 pr-3 text-right">
                      <Money value={e.monto} />
                    </td>
                    <td className="py-1 pr-3 font-mono">{e.puntaje ? formatScore(e.puntaje) : "—"}</td>
                    <td className="py-1 pr-3">
                      {RESULT_LABEL[e.resultado]}
                      {e.motivo_bloqueo ? <span className="text-muted"> · {BLOCK_REASON_LABEL[e.motivo_bloqueo]}</span> : null}
                      {isScoringError(e.factores) ? (
                        <Badge tone="warning" className="ml-1">
                          Scoring failed
                        </Badge>
                      ) : null}
                    </td>
                    <td className="py-1">{EVALUATION_ORIGIN_LABEL[e.origen]}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </details>
      ) : null}
    </Section>
  );
}
