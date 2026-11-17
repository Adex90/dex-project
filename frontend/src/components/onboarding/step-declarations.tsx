"use client";

import { Landmark } from "lucide-react";

import { cx } from "@/components/ui/primitives";

import { SOURCES_OF_FUNDS, type DeclarationField, type DeclarationsDraft } from "./logic";
import { FormField } from "./parts";

/**
 * Step 3, second half: what Mexican financial regulation asks every new
 * client to declare, sent with the verification (migration 0017): their
 * occupation, where the money they will deposit comes from, and whether they
 * are a politically exposed person (PEP). None has a default: each must be
 * answered. A "yes" to PEP is no obstacle: a person on our team approves the
 * file instead of the automatic check.
 */
export function StepDeclarations({
  draft,
  errors,
  onChange,
}: {
  draft: DeclarationsDraft;
  /** Problems to show (the page shows them once the customer tries to continue). */
  errors: Partial<Record<DeclarationField, string>>;
  onChange: (update: (draft: DeclarationsDraft) => DeclarationsDraft) => void;
}) {
  return (
    <section aria-labelledby="declarations-title" className="space-y-5" data-testid="declarations">
      <div className="space-y-1">
        <h3 id="declarations-title" className="flex items-center gap-2 text-sm font-semibold text-ink">
          <Landmark className="size-4 text-synthetic-yellow" aria-hidden />
          About you and your funds
        </h3>
        <p className="text-xs text-muted">
          Mexican financial regulation asks every new client for these three answers. They stay in your file.
        </p>
      </div>

      <FormField
        id="ocupacion"
        label="Occupation"
        value={draft.ocupacion}
        onChange={(e) => onChange((d) => ({ ...d, ocupacion: e.target.value }))}
        error={errors.ocupacion}
        autoComplete="organization-title"
        maxLength={120}
        placeholder="For example: Civil engineer"
      />

      <div className="min-w-0 space-y-1.5">
        <label htmlFor="origen-recursos" className="block text-xs font-medium text-ink-2">
          Source of funds
        </label>
        <select
          id="origen-recursos"
          value={draft.origen_recursos}
          onChange={(e) =>
            onChange((d) => ({ ...d, origen_recursos: e.target.value as DeclarationsDraft["origen_recursos"] }))
          }
          aria-invalid={errors.origen_recursos ? true : undefined}
          aria-describedby={errors.origen_recursos ? "origen-recursos-error" : "origen-recursos-hint"}
          aria-required
          className={cx(
            "h-11 w-full border bg-surface px-3 text-sm text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none",
            errors.origen_recursos ? "border-danger" : "border-border-strong",
          )}
        >
          <option value="" disabled>
            Choose one
          </option>
          {SOURCES_OF_FUNDS.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>
        {errors.origen_recursos ? (
          <p id="origen-recursos-error" className="text-xs text-danger">
            {errors.origen_recursos}
          </p>
        ) : (
          <p id="origen-recursos-hint" className="text-xs text-muted">
            Where the money you will deposit mainly comes from.
          </p>
        )}
      </div>

      <fieldset
        className="space-y-3"
        aria-describedby={errors.declara_pep ? "declara-pep-error" : "declara-pep-hint"}
        aria-invalid={errors.declara_pep ? true : undefined}
      >
        <legend className="text-xs font-medium text-ink-2">Politically exposed person (PEP)</legend>
        <p id="declara-pep-hint" className="text-xs leading-relaxed text-muted">
          Are you, or have you been in the last year, a person with a prominent public function (for example a minister,
          legislator, judge, governor, mayor, senior official of a state-owned company or a political party leader), or
          are you the spouse, a relative or a close associate of one?
        </p>
        <div className="grid grid-cols-2 gap-2">
          {(
            [
              [false, "No", "declara-pep-no"],
              [true, "Yes", "declara-pep-si"],
            ] as const
          ).map(([value, label, id]) => (
            <label
              key={id}
              htmlFor={id}
              className={cx(
                "flex cursor-pointer items-center gap-2.5 border-2 px-4 py-3 text-sm font-semibold transition-colors",
                "has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-synthetic-yellow",
                draft.declara_pep === value
                  ? "border-synthetic-yellow bg-synthetic-yellow/[0.06] text-ink"
                  : errors.declara_pep
                    ? "border-danger/60 text-ink-2"
                    : "border-border text-ink-2 hover:border-border-strong",
              )}
            >
              <input
                id={id}
                type="radio"
                name="declara-pep"
                checked={draft.declara_pep === value}
                onChange={() => onChange((d) => ({ ...d, declara_pep: value }))}
                className="size-4 accent-synthetic-yellow"
              />
              {label}
            </label>
          ))}
        </div>
        {errors.declara_pep ? (
          <p id="declara-pep-error" className="text-xs text-danger">
            {errors.declara_pep}
          </p>
        ) : null}
        {draft.declara_pep === true ? (
          <p
            className="border-l-2 border-synthetic-yellow pl-3 text-xs leading-relaxed text-ink-2"
            data-testid="pep-note"
          >
            Thank you for telling us. A person on our team reviews and approves your file, instead of the automatic
            check: it takes a little longer.
          </p>
        ) : null}
      </fieldset>
    </section>
  );
}
