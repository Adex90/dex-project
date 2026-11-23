"use client";

import type { ChangeEvent, Ref } from "react";
import { Trash2 } from "lucide-react";

import { segmentColor } from "@/components/beneficiaries/allocation-bar";
import { Field, Input, cx, fieldDescribedBy } from "@/components/ui/primitives";
import { RELATIONSHIPS, RELATIONSHIP_LABELS, type DraftErrors } from "@/lib/beneficiaries";
import { parseCurp } from "@/lib/kyc";
import type { BeneficiaryDraft, Relationship } from "@/types/beneficiaries";

/*
 * One beneficiary being edited: legal name, relationship, CURP or date of
 * birth, and their share. Errors show once a field has been left (`touched`),
 * so a card is not red while it is being filled in.
 *
 * A CURP carries the date of birth: typing a valid one fills an empty date.
 */

export type DraftField = "fullName" | "relationship" | "curp" | "birthDate" | "percent";

export function BeneficiaryCard({
  index,
  draft,
  errors,
  touched,
  today,
  disabled,
  nameRef,
  onChange,
  onBlur,
  onRemove,
}: {
  index: number;
  draft: BeneficiaryDraft;
  errors: DraftErrors;
  /** Fields the client has left: only their errors show. */
  touched: ReadonlySet<DraftField>;
  /** "YYYY-MM-DD" in Mexico City: the latest date of birth. */
  today: string;
  disabled: boolean;
  nameRef?: Ref<HTMLInputElement>;
  onChange: (next: BeneficiaryDraft) => void;
  onBlur: (field: DraftField) => void;
  onRemove: () => void;
}) {
  const id = (field: string) => `${draft.key}-${field}`;
  const shown = (field: DraftField, message: string | undefined) => (touched.has(field) ? message : undefined);
  const nameError = shown("fullName", errors.fullName);
  const relationshipError = shown("relationship", errors.relationship);
  const curpError =
    shown("curp", errors.curp) ?? (touched.has("curp") && touched.has("birthDate") ? errors.identity : undefined);
  const birthDateError = shown("birthDate", errors.birthDate);
  const percentError = shown("percent", errors.percent);
  const number = index + 1;

  function onCurp(event: ChangeEvent<HTMLInputElement>) {
    const curp = event.target.value
      .toUpperCase()
      .replace(/[^A-Z0-9]/g, "")
      .slice(0, 18);
    const parsed = curp.length === 18 ? parseCurp(curp) : null;
    onChange({
      ...draft,
      curp,
      birthDate: !draft.birthDate && parsed?.ok ? parsed.value.fechaNacimiento : draft.birthDate,
    });
  }

  function onPercent(event: ChangeEvent<HTMLInputElement>) {
    const raw = event.target.value.replace(",", ".");
    if (/^\d{0,3}(\.\d{0,2})?$/.test(raw)) onChange({ ...draft, percent: raw });
  }

  return (
    <fieldset
      className="min-w-0 border border-border bg-bg p-4"
      disabled={disabled}
      data-testid="beneficiary-card"
      aria-labelledby={id("legend")}
    >
      <div className="mb-4 flex items-center justify-between gap-3">
        <p id={id("legend")} className="flex items-center gap-2 text-sm font-semibold text-ink">
          <span aria-hidden className="size-3 shrink-0" style={{ backgroundColor: segmentColor(index) }} />
          Beneficiary {number}
        </p>
        <button
          type="button"
          onClick={onRemove}
          className="inline-flex h-8 items-center gap-1.5 px-2 text-xs font-medium text-muted transition hover:bg-surface-2 hover:text-danger focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none disabled:opacity-50"
          aria-label={`Remove beneficiary ${number}`}
        >
          <Trash2 className="size-3.5" aria-hidden /> Remove
        </button>
      </div>

      <div className="grid gap-4 sm:grid-cols-6">
        <div className="sm:col-span-4">
          <Field label="Full legal name" htmlFor={id("name")} error={nameError} hint="As it appears on their ID.">
            <Input
              ref={nameRef}
              id={id("name")}
              value={draft.fullName}
              onChange={(e) => onChange({ ...draft, fullName: e.target.value })}
              onBlur={() => onBlur("fullName")}
              invalid={Boolean(nameError)}
              aria-describedby={fieldDescribedBy(id("name"), nameError, "hint")}
              autoComplete="off"
              spellCheck={false}
              maxLength={120}
              placeholder="Given names and surnames"
              className="h-11"
            />
          </Field>
        </div>
        <div className="sm:col-span-2">
          <div className="space-y-1.5">
            <label htmlFor={id("relationship")} className="block text-xs font-medium text-ink-2">
              Relationship
            </label>
            <select
              id={id("relationship")}
              value={draft.relationship}
              onChange={(e) => onChange({ ...draft, relationship: e.target.value as Relationship | "" })}
              onBlur={() => onBlur("relationship")}
              aria-invalid={relationshipError ? true : undefined}
              aria-describedby={relationshipError ? `${id("relationship")}-error` : undefined}
              className={cx(
                "h-11 w-full border bg-surface px-3 text-sm text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none",
                relationshipError ? "border-danger" : "border-border-strong",
              )}
            >
              <option value="" disabled>
                Choose one
              </option>
              {RELATIONSHIPS.map((r) => (
                <option key={r} value={r}>
                  {RELATIONSHIP_LABELS[r]}
                </option>
              ))}
            </select>
            {relationshipError ? (
              <p id={`${id("relationship")}-error`} className="text-xs text-danger">
                {relationshipError}
              </p>
            ) : null}
          </div>
        </div>

        <div className="sm:col-span-3">
          <Field label="CURP" htmlFor={id("curp")} error={curpError} hint="Optional with a date of birth.">
            <Input
              id={id("curp")}
              value={draft.curp}
              onChange={onCurp}
              onBlur={() => onBlur("curp")}
              invalid={Boolean(curpError)}
              aria-describedby={fieldDescribedBy(id("curp"), curpError, "hint")}
              autoComplete="off"
              autoCapitalize="characters"
              spellCheck={false}
              maxLength={18}
              placeholder="18 characters"
              className="h-11 font-mono uppercase"
            />
          </Field>
        </div>
        <div className="sm:col-span-2">
          <Field label="Date of birth" htmlFor={id("birth")} error={birthDateError}>
            <Input
              id={id("birth")}
              type="date"
              value={draft.birthDate}
              min="1900-01-01"
              max={today}
              onChange={(e) => onChange({ ...draft, birthDate: e.target.value })}
              onBlur={() => onBlur("birthDate")}
              invalid={Boolean(birthDateError)}
              aria-describedby={fieldDescribedBy(id("birth"), birthDateError)}
              className="h-11 [color-scheme:dark]"
            />
          </Field>
        </div>
        <div className="sm:col-span-1">
          <Field label="Share" htmlFor={id("percent")} error={percentError}>
            <div className="relative">
              <Input
                id={id("percent")}
                value={draft.percent}
                onChange={onPercent}
                onBlur={() => onBlur("percent")}
                invalid={Boolean(percentError)}
                aria-describedby={fieldDescribedBy(id("percent"), percentError)}
                inputMode="decimal"
                autoComplete="off"
                placeholder="0"
                className="h-11 pr-7 text-right font-semibold tabular-nums"
                data-testid="beneficiary-percent"
              />
              <span
                aria-hidden
                className="pointer-events-none absolute inset-y-0 right-2.5 flex items-center text-sm text-muted"
              >
                %
              </span>
            </div>
          </Field>
        </div>
      </div>
    </fieldset>
  );
}
