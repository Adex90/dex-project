"use client";

import { useCallback, useState } from "react";
import { useRouter } from "next/navigation";

import { ApiError, describeError } from "@/lib/api/client";
import { newIdempotencyKey } from "@/lib/api/endpoints";
import { isStaleStateError } from "@/lib/aml";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";

/**
 * State of one console form: its idempotency key (created when the form
 * opens and reused on every retry, so a lost response can never apply an
 * action twice), the busy flag and the English error. A new key is drawn only
 * after a success or when the API says the key belongs to another request.
 *
 * When the API refuses because the record changed under the officer (409:
 * taken by another officer, already closed or decided...; see
 * `isStaleStateError`), `onStale` gets the English message so the caller can
 * reload the record instead of offering the same doomed action again.
 */
export function useSubmit({ onStale }: { onStale?: (message: string) => void } = {}) {
  const router = useRouter();
  const [key, setKey] = useState(newIdempotencyKey);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(
    async <T>(action: (idempotencyKey: string) => Promise<T>): Promise<T | null> => {
      setBusy(true);
      setError(null);
      try {
        const result = await action(key);
        setKey(newIdempotencyKey());
        return result;
      } catch (e) {
        if (e instanceof ApiError && e.isUnauthorized) {
          useAuthStore.getState().expire();
          router.replace(EXPIRED_LOGIN_PATH);
          return null;
        }
        if (e instanceof ApiError && e.code === "CLAVE_IDEMPOTENCIA_REUTILIZADA") setKey(newIdempotencyKey());
        setError(describeError(e));
        if (onStale && e instanceof ApiError && isStaleStateError(e.status, e.code)) onStale(describeError(e));
        return null;
      } finally {
        setBusy(false);
      }
    },
    [key, router, onStale],
  );

  return { busy, error, setError, run };
}
