import { createContext, useContext } from "react";
import type { AppState } from "@/components/AppState";

/**
 * The app-state context on its own, without the chain hooks: shared chrome (the footer) reads it here, so the landing
 * and terms pages do not bundle viem and the wallet code just to learn that there is no app state.
 */
export const AppStateContext = createContext<AppState | null>(null);

/** The app state inside <AppStateProvider>, else null: the landing and terms pages make no chain request. */
export function useOptionalApp(): AppState | null {
  return useContext(AppStateContext);
}
