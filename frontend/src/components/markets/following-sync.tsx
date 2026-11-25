"use client";

import { useEffect } from "react";

import { useSignOutOn401 } from "@/hooks/use-sign-out-on-401";
import { api } from "@/lib/api/endpoints";
import { usePriceAlertsStore } from "@/store/price-alerts";
import { useVaultStore } from "@/store/vault";
import { useWatchlistStore } from "@/store/watchlist";

/*
 * Keeps the watchlist and the price alerts current for the whole dashboard,
 * whatever the page: the stars (Discover, an asset's page), the Vault's
 * Watchlist, an asset's Price alerts card and the notice when a target is
 * reached all read the same stores.
 *
 * Loaded when the dashboard opens and again whenever the vault moves:
 * following an asset, setting or cancelling a target and a target being
 * reached each bump the vault's version, so a change made on another device,
 * and the market worker's, arrive live. Moves this close together (one
 * action's commits) reload once. Renders nothing.
 */

/** Vault moves this close together reload once (as the Vault's lists do). */
const RELOAD_MS = 150;

export function FollowingSync({ token }: { token: string }) {
  const vaultMoved = useVaultStore((s) => s.updatedAt);
  const signOutOn401 = useSignOutOn401();

  useEffect(() => {
    const timer = setTimeout(() => {
      void useWatchlistStore.getState().load((signal) => api.watchlist(token, signal).catch(signOutOn401));
      void usePriceAlertsStore.getState().load((signal) => api.priceAlerts(token, signal).catch(signOutOn401));
    }, RELOAD_MS);
    return () => clearTimeout(timer);
  }, [token, vaultMoved, signOutOn401]);

  return null;
}
