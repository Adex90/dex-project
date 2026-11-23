"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Divide, Plus, Users } from "lucide-react";

import { AllocationBar } from "@/components/beneficiaries/allocation-bar";
import { BeneficiaryCard, type DraftField } from "@/components/beneficiaries/beneficiary-card";
import { DesignationRecord } from "@/components/beneficiaries/designation-record";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { Alert, Button, Skeleton } from "@/components/ui/primitives";
import { ONBOARDING_PATH } from "@/hooks/use-client-home";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import {
  FULL,
  MAX_BENEFICIARIES,
  allocation,
  centsToPercent,
  designationProblem,
  designationRequest,
  draftErrors,
  draftsFrom,
  emptyDraft,
  hasErrors,
  isDirty,
  parseBeneficiaries,
  splitEvenly,
} from "@/lib/beneficiaries";
import { todayInMexicoCity } from "@/lib/kyc";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import type { BeneficiariesView, BeneficiaryDraft } from "@/types/beneficiaries";

const UNREADABLE = "Your beneficiaries could not be read. Please try again.";
const SAVED_UNREADABLE = "Your beneficiaries were saved, but the answer could not be read. Showing what is on record.";

/*
 * Legal beneficiaries (roadmap item 25): who receives the account, in what
 * shares, should the holder die.
 *
 * - Up to five people, each with their legal name, relationship, CURP or date
 *   of birth, and a share. The shares must add up to exactly 100%: the gold
 *   bar shows the sum live, counted in cents of a percent, and Save stays off
 *   until it is exact. An empty list withdraws the designation.
 * - Saving is atomic: the whole designation becomes a new, immutable,
 *   hash-chained version on the server (the database refuses any other sum),
 *   on top of the version this page loaded. If another session saved first,
 *   the server answers 409: the newer version shows on the right and the
 *   edits stay, to review and save again.
 * - Only verified clients can designate; the page says how to get verified.
 *
 * Layout: one column on phones (editor, record, history); from xl, the
 * editor in two thirds and the record beside it.
 */
export default function BeneficiariesPage() {
  const router = useRouter();
  const { token, client } = useDashboard();
  const [view, setView] = useState<BeneficiariesView | null>(null);
  const [drafts, setDrafts] = useState<BeneficiaryDraft[]>([]);
  const [touched, setTouched] = useState<Record<string, ReadonlySet<DraftField>>>({});
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savedVersion, setSavedVersion] = useState<number | null>(null);
  const focusKey = useRef<string | null>(null);
  const nameInputs = useRef(new Map<string, HTMLInputElement>());
  const today = todayInMexicoCity();
  const holderCurp = client?.curp ?? null;

  const endSession = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  /** Loads what is on record; `keepDrafts` leaves the editor as it is (after a 409). */
  const load = useCallback(
    async ({ signal, keepDrafts = false }: { signal?: AbortSignal; keepDrafts?: boolean } = {}) => {
      try {
        const parsed = parseBeneficiaries(await api.beneficiaries(token, signal));
        if (!parsed) throw new Error(UNREADABLE);
        setView(parsed);
        if (!keepDrafts) {
          setDrafts(draftsFrom(parsed.current));
          setTouched({});
        }
        setLoadError(null);
      } catch (error) {
        if (error instanceof DOMException && error.name === "AbortError") return;
        if (error instanceof ApiError && error.isUnauthorized) {
          endSession();
          return;
        }
        setLoadError(describeError(error));
      }
    },
    [token, endSession],
  );

  useEffect(() => {
    const controller = new AbortController();
    void load({ signal: controller.signal });
    return () => controller.abort();
  }, [load]);

  // A beneficiary just added gets the focus on its name.
  useEffect(() => {
    if (!focusKey.current) return;
    nameInputs.current.get(focusKey.current)?.focus();
    focusKey.current = null;
  }, [drafts]);

  const current = view?.current ?? null;
  const live = useMemo(() => allocation(drafts), [drafts]);
  const dirty = isDirty(drafts, current);
  const canDesignate = view?.canDesignate ?? false;
  const problem = useMemo(() => {
    const invalid = drafts.findIndex((d) => hasErrors(draftErrors(d, today, holderCurp)));
    if (invalid >= 0) {
      const first = Object.values(draftErrors(drafts[invalid]!, today, holderCurp))[0];
      return `Beneficiary ${invalid + 1}: ${first}`;
    }
    return designationProblem(drafts, today, holderCurp);
  }, [drafts, today, holderCurp]);
  const canSave = canDesignate && dirty && problem === null && !saving;

  function edit(next: BeneficiaryDraft) {
    setDrafts((list) => list.map((d) => (d.key === next.key ? next : d)));
    setSavedVersion(null);
    setSaveError(null);
  }

  function touch(key: string, field: DraftField) {
    setTouched((all) => {
      const fields = all[key] ?? new Set<DraftField>();
      if (fields.has(field)) return all;
      return { ...all, [key]: new Set([...fields, field]) };
    });
  }

  function add() {
    if (drafts.length >= MAX_BENEFICIARIES) return;
    // The first takes 100%; the next ones, whatever is left.
    const left = FULL - live.allocated;
    const draft = emptyDraft(drafts.length === 0 ? "100" : left > 0 ? centsToPercent(left) : "");
    focusKey.current = draft.key;
    setDrafts((list) => [...list, draft]);
    setSavedVersion(null);
  }

  function remove(key: string) {
    setDrafts((list) => list.filter((d) => d.key !== key));
    nameInputs.current.delete(key);
    setSavedVersion(null);
  }

  function splitShares() {
    const shares = splitEvenly(drafts.length);
    setDrafts((list) => list.map((d, i) => ({ ...d, percent: shares[i] ?? d.percent })));
    setSavedVersion(null);
  }

  function discard() {
    setDrafts(draftsFrom(current));
    setTouched({});
    setSaveError(null);
  }

  async function save() {
    if (!view || !canSave) return;
    setSaving(true);
    setSaveError(null);
    setSavedVersion(null);
    let body: unknown;
    try {
      body = await api.saveBeneficiaries(token, designationRequest(drafts, view.version));
    } catch (error) {
      setSaving(false);
      if (error instanceof ApiError && error.isUnauthorized) {
        endSession();
        return;
      }
      // Saved elsewhere first: show the newer version, keep the edits to save on top of it.
      if (error instanceof ApiError && error.status === 409) await load({ keepDrafts: true });
      setSaveError(describeError(error));
      return;
    }
    const parsed = parseBeneficiaries(body);
    if (parsed) {
      setView(parsed);
      setDrafts(draftsFrom(parsed.current));
      setTouched({});
      setSavedVersion(parsed.version);
    } else {
      // A 2xx means it is saved: never claim otherwise. Show what the server holds.
      setSaveError(SAVED_UNREADABLE);
      await load();
    }
    setSaving(false);
  }

  return (
    <div className="space-y-6" data-testid="beneficiaries-page">
      <div className="space-y-1">
        <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
          Legal beneficiaries
        </p>
        <h2 className="text-2xl font-semibold tracking-tight text-ink">Who receives your account</h2>
        <p className="max-w-2xl text-sm text-muted">
          Name up to {MAX_BENEFICIARIES} people to receive your funds and investments, in the shares you choose. The
          shares must add up to exactly 100%. Every save is kept as a signed version, chained to the one before.
        </p>
      </div>

      {loadError ? (
        <Alert tone="error" title="Could not load your beneficiaries">
          {loadError}{" "}
          <Button variant="ghost" size="sm" onClick={() => void load()}>
            Retry
          </Button>
        </Alert>
      ) : null}

      {view && !canDesignate ? (
        <Alert tone="info" title="Verify your identity to name beneficiaries">
          Beneficiaries can be designated once your identity is verified.{" "}
          <Link href={ONBOARDING_PATH} className="font-semibold underline underline-offset-4">
            Verify your identity
          </Link>
        </Alert>
      ) : null}

      {view === null ? (
        loadError ? null : (
          <div className="grid gap-6 xl:grid-cols-3" aria-busy="true">
            <Skeleton className="h-96 xl:col-span-2" />
            <Skeleton className="h-64" />
          </div>
        )
      ) : (
        <div className="grid grid-cols-1 items-start gap-6 xl:grid-cols-3">
          <section
            className="min-w-0 border border-border bg-surface xl:col-span-2"
            aria-labelledby="designation-title"
          >
            <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3 sm:px-5">
              <div className="flex items-center gap-2">
                <Users className="size-4 text-synthetic-yellow" aria-hidden />
                <h3 id="designation-title" className="text-sm font-semibold text-ink">
                  Designation
                </h3>
              </div>
              <span className="text-xs text-muted tabular-nums">
                {drafts.length} of {MAX_BENEFICIARIES}
              </span>
            </header>

            <div className="space-y-5 px-4 py-5 sm:px-5">
              <AllocationBar value={live} />

              {drafts.length === 0 ? (
                <p className="border border-dashed border-border-strong px-4 py-8 text-center text-sm text-muted">
                  {current && current.beneficiaries.length > 0
                    ? "Saving now withdraws every beneficiary."
                    : "No beneficiaries yet. Add the first one: they receive 100% until you add more."}
                </p>
              ) : (
                <div className="space-y-4">
                  {drafts.map((draft, index) => (
                    <BeneficiaryCard
                      key={draft.key}
                      index={index}
                      draft={draft}
                      errors={draftErrors(draft, today, holderCurp)}
                      touched={touched[draft.key] ?? new Set()}
                      today={today}
                      disabled={!canDesignate || saving}
                      nameRef={(el) => {
                        if (el) nameInputs.current.set(draft.key, el);
                      }}
                      onChange={edit}
                      onBlur={(field) => touch(draft.key, field)}
                      onRemove={() => remove(draft.key)}
                    />
                  ))}
                </div>
              )}

              <div className="flex flex-wrap gap-2">
                <Button
                  variant="secondary"
                  onClick={add}
                  disabled={!canDesignate || saving || drafts.length >= MAX_BENEFICIARIES}
                >
                  <Plus className="size-4" aria-hidden /> Add beneficiary
                </Button>
                {drafts.length >= 2 ? (
                  <Button variant="ghost" onClick={splitShares} disabled={!canDesignate || saving}>
                    <Divide className="size-4" aria-hidden /> Split evenly
                  </Button>
                ) : null}
              </div>
            </div>

            <footer className="space-y-3 border-t border-border px-4 py-4 sm:px-5">
              {savedVersion !== null ? (
                <Alert tone="success" title={`Saved as version ${savedVersion}`}>
                  Your beneficiaries are on record, signed and chained to the previous version.
                </Alert>
              ) : null}
              {saveError ? (
                <Alert tone="error" title="Not saved">
                  {saveError}
                </Alert>
              ) : null}
              <div className="flex flex-col-reverse gap-3 sm:flex-row sm:items-center sm:justify-between">
                <p id="designation-status" className="min-h-5 text-xs text-muted" data-testid="designation-status">
                  {!dirty ? "No unsaved changes." : (problem ?? "Ready to save.")}
                </p>
                <div className="flex flex-col-reverse gap-2 sm:flex-row">
                  {dirty ? (
                    <Button variant="ghost" size="lg" onClick={discard} disabled={saving}>
                      Discard changes
                    </Button>
                  ) : null}
                  <Button
                    size="lg"
                    onClick={() => void save()}
                    disabled={!canSave}
                    loading={saving}
                    aria-describedby="designation-status"
                    className="min-w-56 font-semibold"
                    data-testid="save-beneficiaries"
                  >
                    {drafts.length === 0 && current && current.beneficiaries.length > 0
                      ? "Save with no beneficiaries"
                      : "Save beneficiaries"}
                  </Button>
                </div>
              </div>
            </footer>
          </section>

          <div className="min-w-0">
            <DesignationRecord view={view} />
          </div>
        </div>
      )}
    </div>
  );
}
