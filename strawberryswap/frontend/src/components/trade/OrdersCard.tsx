"use client";

import { fmt } from "@/lib/format";
import type { IntentView, OwnedNoteView } from "@/zk/engine";
import { useApp, useSymbols } from "@/components/AppState";
import { IconClock, IconLock } from "@/components/ui/icons";
import { Card, CardHeader, Loading, Skeleton, TokenIcon } from "@/components/ui/primitives";

export const when = (unix: bigint): string => new Date(Number(unix) * 1000).toISOString().replace("T", " ").slice(0, 16) + " UTC";

const STATE_PILL: Record<string, string> = { settled: "pill-mint", refunded: "pill-blush", created: "pill-plum" };

/** One intent row: "<value> <sold> → ≥ <minOut> <bought>" plus its state (the relay's view while still open). */
export function IntentRow({ i }: { i: IntentView }) {
  const { swapper } = useApp();
  const sym = useSymbols();
  const t = swapper.tracked.find((x) => x.commitment === i.commitment);
  const label = i.state === "created" ? (t ? `relay: ${t.state}` : "on-chain · not yet sent to a relay") : i.state;
  return (
    <li className="flex items-center justify-between gap-3 rounded-2xl bg-plum-950/50 px-3 py-2.5 shadow-inset">
      <span className="flex min-w-0 items-center gap-2">
        <TokenIcon symbol={sym(i.dir)} size={22} />
        <span className="num min-w-0 truncate text-sm">
          {fmt(i.value)} {sym(i.dir)} → ≥ {fmt(i.minOut)} {sym(i.dir === 0 ? 1 : 0)}
        </span>
      </span>
      <span className="flex shrink-0 flex-col items-end gap-0.5">
        <span className={STATE_PILL[i.state] ?? "pill-plum"}>{label}</span>
        <span className="flex items-center gap-1 text-[11px] text-haze">
          <IconClock className="h-3 w-3" />
          until {when(i.deadline)}
        </span>
      </span>
    </li>
  );
}

/** One shielded note; with `onWithdraw` it carries the unshield action. */
export function NoteRow({ n, onWithdraw, disabled }: { n: OwnedNoteView; onWithdraw?: () => void; disabled?: boolean }) {
  const sym = useSymbols();
  return (
    <li className="flex items-center justify-between gap-3 rounded-2xl bg-plum-950/50 px-3 py-2.5 shadow-inset">
      <span className="flex min-w-0 items-center gap-2">
        <TokenIcon symbol={sym(n.assetId)} size={22} />
        <span className="num min-w-0 truncate text-sm">
          {fmt(n.value)} {sym(n.assetId)} <span className="text-xs text-haze">· leaf {n.index}{n.reserved ? " · in use" : ""}</span>
        </span>
      </span>
      {onWithdraw ? (
        <button type="button" className="btn-secondary !px-3 !py-1.5 text-xs" disabled={disabled} onClick={onWithdraw}>
          Withdraw
        </button>
      ) : null}
    </li>
  );
}

/** Open and past private orders (intents) and spendable notes, with unshield (withdraw) per note. */
export function OrdersCard({ className = "" }: { className?: string }) {
  const { ready, shielded: w, swapper } = useApp();
  const canAct = ready && w.phase === "ready" && w.inSync !== false && !swapper.busy;
  const unlocked = w.phase === "ready" || w.phase === "syncing";
  const unspent = (w.wallet?.notes ?? []).filter((n) => !n.spent);
  const intents = w.wallet?.intents ?? [];

  return (
    <Card className={className} aria-labelledby="orders-title">
      <CardHeader
        icon={<IconClock />}
        title={<span id="orders-title">Your private orders</span>}
        subtitle="Intents settle in the next batch or refund after their deadline"
        right={
          intents.length > 0 ? (
            <button type="button" className="btn-secondary !px-3 !py-1.5 text-xs" disabled={!canAct} onClick={() => void swapper.resubmit()}>
              Resubmit pending
            </button>
          ) : null
        }
      />

      {!unlocked ? (
        <div className="card-inset flex items-center gap-3 text-sm text-lavender">
          <IconLock className="h-5 w-5 shrink-0 text-lavender" />
          Unlock your shielded wallet to see your intents and notes.
        </div>
      ) : !w.wallet ? (
        <Loading label="Loading your orders" className="space-y-2">
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
        </Loading>
      ) : (
        <div className="space-y-5 animate-fade-in">
          <section>
            <h3 className="eyebrow mb-2">Intents</h3>
            {intents.length > 0 ? (
              <ul className="space-y-2">
                {intents.map((i) => (
                  <IntentRow key={i.commitment.toString()} i={i} />
                ))}
              </ul>
            ) : (
              <p className="text-sm text-haze">No intents yet. A private swap creates one.</p>
            )}
          </section>
          <section>
            <h3 className="eyebrow mb-2">Shielded notes</h3>
            {unspent.length > 0 ? (
              <ul className="space-y-2">
                {unspent.map((n) => (
                  <NoteRow key={n.commitment.toString()} n={n} disabled={!canAct || n.reserved} onWithdraw={() => void swapper.withdraw(n.commitment)} />
                ))}
              </ul>
            ) : (
              <p className="text-sm text-haze">No notes yet. Shield tokens to create your first private note.</p>
            )}
          </section>
        </div>
      )}
    </Card>
  );
}
