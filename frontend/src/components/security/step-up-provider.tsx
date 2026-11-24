"use client";

import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";

import { StepUpModal } from "@/components/security/step-up-modal";
import { StepUpCancelled, isStepUpRequired } from "@/lib/step-up";
import type { StepUpRequest } from "@/types/step-up";

/*
 * The step-up gate for the dashboard (roadmap item 22). A page wraps a
 * sensitive request in `useStepUp()`:
 *
 *   const guard = useStepUp();
 *   await guard(() => api.withdraw(token, request), { action, detail, cancelled });
 *
 * The request goes first as it is: with a verified window open it simply
 * succeeds. If the server answers 403 STEP_UP_REQUERIDO (nothing was done),
 * the check opens (components/security/step-up-modal.tsx); once confirmed,
 * the same request goes again, with the same idempotency key, and its answer
 * is the guard's. Closing the check rejects with `StepUpCancelled`, whose
 * message the page shows: nothing was sent or saved.
 *
 * One check at a time: a second request while one is open replaces it (the
 * first is cancelled). Signing out closes it.
 */

export type StepUpGuard = <T>(run: () => Promise<T>, request: StepUpRequest) => Promise<T>;

const StepUpContext = createContext<StepUpGuard | null>(null);

export function useStepUp(): StepUpGuard {
  const guard = useContext(StepUpContext);
  if (!guard) throw new Error("useStepUp() must be used inside <StepUpProvider>.");
  return guard;
}

interface Pending {
  id: number;
  request: StepUpRequest;
  resolve: () => void;
  reject: (reason: unknown) => void;
}

export function StepUpProvider({
  token,
  onSessionEnded,
  children,
}: {
  token: string;
  /** The check ended the session (a lockout), or found it already over. */
  onSessionEnded: (reason: "locked" | "expired") => void;
  children: ReactNode;
}) {
  const [pending, setPending] = useState<Pending | null>(null);
  const current = useRef<Pending | null>(null);
  const nextId = useRef(0);

  const settle = useCallback((outcome: "verified" | "cancelled") => {
    const check = current.current;
    current.current = null;
    setPending(null);
    if (!check) return;
    if (outcome === "verified") check.resolve();
    else check.reject(new StepUpCancelled(check.request.cancelled));
  }, []);

  const ask = useCallback(
    (request: StepUpRequest) =>
      new Promise<void>((resolve, reject) => {
        const earlier = current.current;
        if (earlier) earlier.reject(new StepUpCancelled(earlier.request.cancelled));
        nextId.current += 1;
        const check: Pending = { id: nextId.current, request, resolve, reject };
        current.current = check;
        setPending(check);
      }),
    [],
  );

  const guard = useCallback<StepUpGuard>(
    async <T,>(run: () => Promise<T>, request: StepUpRequest): Promise<T> => {
      try {
        return await run();
      } catch (error) {
        if (!isStepUpRequired(error)) throw error;
      }
      await ask(request);
      return run();
    },
    [ask],
  );

  // Signing out unmounts the dashboard: a check still open goes with it.
  useEffect(
    () => () => {
      const check = current.current;
      current.current = null;
      if (check) check.reject(new StepUpCancelled(check.request.cancelled));
    },
    [],
  );

  return (
    <StepUpContext value={guard}>
      {children}
      {pending ? (
        <StepUpModal
          key={pending.id}
          token={token}
          request={pending.request}
          onVerified={() => settle("verified")}
          onCancel={() => settle("cancelled")}
          onSessionEnded={(reason) => {
            settle("cancelled");
            onSessionEnded(reason);
          }}
        />
      ) : null}
    </StepUpContext>
  );
}
