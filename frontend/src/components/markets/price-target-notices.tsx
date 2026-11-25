"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { BellRing, X } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { assetPath } from "@/lib/market-data";
import { formatOrderTime } from "@/lib/order-history";
import { reachedSentence, toAnnounce } from "@/lib/price-alerts";
import { usePriceAlertsStore } from "@/store/price-alerts";

/*
 * Price target reached: the dashboard's notice, at the top of every page,
 * when one of the client's price targets is reached (the market worker
 * checks every few seconds; the vault's WebSocket brings the change and
 * FollowingSync reloads the list).
 *
 * - It lists the targets reached since the client last dismissed it (this
 *   device remembers when, per client; the first time, the last 24 hours),
 *   newest first, each with its asset's page. Dismiss marks them seen.
 * - Never the singular "alert" (an AML word no client screen may show): the
 *   feature is "Price alerts" and this notice says "Price target reached".
 */

/** The last reached target dismissed on this device, per client (epoch ms): a convenience, nothing breaks without it. */
const seenKey = (clientId: string) => `synthetic-capital.price-targets-seen.${clientId}`;
/** With nothing remembered, what was reached in the last day is news. */
const FIRST_LOOK_MS = 24 * 60 * 60 * 1000;
/** Lines shown; the rest are counted. */
const SHOWN = 3;

function readSeen(clientId: string): number | null {
  try {
    const value = window.localStorage.getItem(seenKey(clientId));
    const at = value === null ? Number.NaN : Number(value);
    return Number.isFinite(at) ? at : null;
  } catch {
    return null;
  }
}

function writeSeen(clientId: string, at: number): void {
  try {
    window.localStorage.setItem(seenKey(clientId), String(at));
  } catch {
    // Private mode or storage off: dismissed for this visit only.
  }
}

export function PriceTargetNotices({ clientId, className }: { clientId: string; className?: string }) {
  const alerts = usePriceAlertsStore((s) => s.alerts);
  // Rendered only in the browser (the dashboard waits for the session), so storage can be read here.
  const [since, setSince] = useState(() => readSeen(clientId) ?? Date.now() - FIRST_LOOK_MS);
  const reached = useMemo(() => toAnnounce(alerts, since), [alerts, since]);

  if (reached.length === 0) return null;

  const dismiss = () => {
    // The newest one shown, by the server's clock: one reached later is still news.
    const latest = Math.max(...reached.map((a) => Date.parse(a.triggered_at ?? "")));
    writeSeen(clientId, latest);
    setSince(latest);
  };
  const more = reached.length - SHOWN;

  return (
    <section
      aria-labelledby="price-targets-title"
      className={cx("relative border border-synthetic-yellow/40 bg-surface pl-[3px]", className)}
      data-testid="price-target-notices"
    >
      <span aria-hidden className="absolute inset-y-0 left-0 w-[3px] bg-synthetic-yellow" />
      <div className="flex items-start gap-3 px-4 py-3 sm:px-5">
        <BellRing className="mt-0.5 size-4 shrink-0 text-synthetic-yellow" aria-hidden />
        <div className="min-w-0 flex-1" role="status">
          <h2
            id="price-targets-title"
            className="text-[11px] font-semibold tracking-[0.2em] text-synthetic-yellow uppercase"
          >
            {reached.length === 1 ? "Price target reached" : "Price targets reached"}
          </h2>
          <ul className="mt-1.5 space-y-1.5">
            {reached.slice(0, SHOWN).map((alert) => (
              <li
                key={alert.alert_id}
                className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 text-sm"
                data-testid="price-target-notice"
                data-alert-id={alert.alert_id}
                data-ticker={alert.ticker}
              >
                <span className="text-ink">{reachedSentence(alert)}</span>
                <span className="text-xs text-muted tabular-nums">{formatOrderTime(alert.triggered_at ?? "")}</span>
                <Link
                  href={assetPath(alert.ticker)}
                  className="text-xs font-semibold tracking-[0.14em] text-synthetic-yellow uppercase underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
                >
                  View {alert.ticker}
                </Link>
              </li>
            ))}
          </ul>
          {more > 0 ? (
            <p className="mt-1.5 text-xs text-muted">
              {more} more on {more === 1 ? "its asset's page" : "their assets' pages"}.
            </p>
          ) : null}
        </div>
        <button
          type="button"
          onClick={dismiss}
          aria-label="Dismiss"
          title="Dismiss"
          className="grid size-8 shrink-0 place-items-center text-muted transition-colors hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
          data-testid="price-target-dismiss"
        >
          <X className="size-4" aria-hidden />
        </button>
      </div>
    </section>
  );
}
