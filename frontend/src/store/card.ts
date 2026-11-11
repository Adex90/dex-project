"use client";

/**
 * Card store: the state of the Synthetic Capital debit card on the Account
 * tab (components/wealth/card-management.tsx), locked or not, and what it
 * may be used for.
 *
 * "Lock", never "freeze": the client's screens must not use the words of an
 * AML hold (a preventive freeze is never disclosed: e2e/aml.mjs checks every
 * client page for them).
 *
 * A preview: no card issuer is connected, so nothing here is sent anywhere.
 * The choices outlive the page (back from another tab they are as they were
 * left) and the dashboard shell calls `reset()` on sign-out, so the next
 * person in this tab starts from the defaults.
 */

import { create } from "zustand";

/** What the card may be used for. */
export type CardUse = "online" | "contactless" | "atm" | "abroad";

export const CARD_USES: readonly { key: CardUse; label: string; detail: string }[] = [
  { key: "online", label: "Online payments", detail: "Shops and subscriptions on the web" },
  { key: "contactless", label: "Contactless", detail: "Tap to pay at the till" },
  { key: "atm", label: "ATM withdrawals", detail: "Cash from any ATM in Mexico" },
  { key: "abroad", label: "Payments abroad", detail: "Outside Mexico, in any currency" },
];

export interface CardFields {
  locked: boolean;
  uses: Readonly<Record<CardUse, boolean>>;
}

export interface CardActions {
  toggleLocked: () => void;
  setUse: (use: CardUse, on: boolean) => void;
  reset: () => void;
}

const INITIAL: CardFields = { locked: false, uses: { online: true, contactless: true, atm: true, abroad: false } };

export const useCardStore = create<CardFields & CardActions>()((set) => ({
  ...INITIAL,
  toggleLocked: () => set((state) => ({ locked: !state.locked })),
  setUse: (use, on) => set((state) => ({ uses: { ...state.uses, [use]: on } })),
  reset: () => set({ ...INITIAL }),
}));
