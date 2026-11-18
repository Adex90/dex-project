"use client";

import { create } from "zustand";

import { DEFAULT_INACTIVITY_MINUTES } from "@/lib/security";
import type { InactivityMinutes } from "@/types/security";

/**
 * The client's inactivity timeout, for this tab's idle sign-out. The
 * dashboard reads it from the server once per session; the Security page
 * updates it when the client picks another. Not persisted: the server's
 * preference is the source of truth.
 */
interface SecurityState {
  inactivityMinutes: InactivityMinutes;
  setInactivityMinutes: (minutes: InactivityMinutes) => void;
  reset: () => void;
}

export const useSecurityStore = create<SecurityState>()((set) => ({
  inactivityMinutes: DEFAULT_INACTIVITY_MINUTES,
  setInactivityMinutes: (inactivityMinutes) => set({ inactivityMinutes }),
  reset: () => set({ inactivityMinutes: DEFAULT_INACTIVITY_MINUTES }),
}));
