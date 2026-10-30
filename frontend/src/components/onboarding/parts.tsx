"use client";

import type { InputHTMLAttributes, ReactNode, Ref } from "react";
import { Check, XCircle } from "lucide-react";

import { Input, cx } from "@/components/ui/primitives";

import { STEPS, type Step } from "./logic";

// -----------------------------------------------------------------------------
// Stepper
// -----------------------------------------------------------------------------

/**
 * Progress through the four steps. The current step carries
 * aria-current="step"; earlier steps the customer may revisit are buttons.
 */
export function Stepper({
  current,
  canVisit,
  isDone = (step) => step < current,
  onVisit,
}: {
  current: Step;
  canVisit: (step: Step) => boolean;
  /** Whether a step is shown as completed (default: every step before the current one). */
  isDone?: (step: Step) => boolean;
  onVisit: (step: Step) => void;
}) {
  return (
    <nav aria-label="Verification steps">
      <ol className="grid grid-cols-4 gap-1">
        {STEPS.map(({ n, label }) => {
          const isCurrent = n === current;
          const done = !isCurrent && isDone(n);
          const visitable = !isCurrent && canVisit(n);
          const content = (
            <>
              <span
                className={cx(
                  "flex size-7 items-center justify-center rounded-full border text-xs font-semibold tabular-nums transition",
                  isCurrent && "border-accent bg-accent text-accent-ink shadow-sm",
                  done && "border-verified/40 bg-verified-soft text-verified",
                  !isCurrent && !done && "border-border-strong bg-surface text-muted",
                )}
                aria-hidden
              >
                {done ? <Check className="size-3.5" /> : n}
              </span>
              <span
                className={cx(
                  "text-[11px] leading-tight",
                  isCurrent ? "font-semibold text-ink" : done ? "text-ink-2" : "text-muted",
                )}
              >
                <span className="sr-only">Step {n}: </span>
                {label}
                <span className="sr-only">{isCurrent ? " (current step)" : done ? " (completed)" : ""}</span>
              </span>
            </>
          );
          return (
            <li key={n} aria-current={isCurrent ? "step" : undefined} className="relative">
              {n > 1 ? (
                <span
                  className={cx(
                    "absolute top-3.5 right-1/2 mr-4 h-px w-[calc(100%-2rem)]",
                    n <= current ? "bg-verified/40" : "bg-border",
                  )}
                  aria-hidden
                />
              ) : null}
              {visitable ? (
                <button
                  type="button"
                  onClick={() => onVisit(n)}
                  className="relative flex w-full flex-col items-center gap-1 rounded-md py-0.5 text-center transition hover:bg-surface-2 focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
                >
                  {content}
                </button>
              ) : (
                <div className="relative flex flex-col items-center gap-1 py-0.5 text-center">{content}</div>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

// -----------------------------------------------------------------------------
// Step frame
// -----------------------------------------------------------------------------

/**
 * Container of one step: a labelled region whose heading receives focus when
 * the step changes (so screen readers announce the new step).
 */
export function StepFrame({
  step,
  title,
  description,
  headingRef,
  children,
}: {
  step: Step;
  title: string;
  description?: ReactNode;
  headingRef?: Ref<HTMLHeadingElement>;
  children: ReactNode;
}) {
  const headingId = `onboarding-step-${step}-title`;
  return (
    <section data-testid={`onboarding-step-${step}`} aria-labelledby={headingId} className="animate-reveal space-y-5">
      <div className="space-y-1">
        <p className="text-[11px] font-semibold tracking-[0.12em] text-muted uppercase">Step {step} of 4</p>
        <h2
          id={headingId}
          ref={headingRef}
          tabIndex={-1}
          className="rounded-sm text-lg font-semibold tracking-tight text-ink focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
        >
          {title}
        </h2>
        {description ? <div className="text-sm text-muted">{description}</div> : null}
      </div>
      {children}
    </section>
  );
}

// -----------------------------------------------------------------------------
// Form field
// -----------------------------------------------------------------------------

/**
 * Label + input + messages, all tied to the input: the error (`<id>-error`),
 * a confirmation or notice (`<id>-note`) and the hint (`<id>-hint`) are
 * referenced by aria-describedby.
 */
export function FormField({
  id,
  label,
  optional = false,
  hint,
  note,
  error,
  className,
  inputClassName,
  ...input
}: {
  id: string;
  label: string;
  optional?: boolean;
  hint?: ReactNode;
  note?: ReactNode;
  error?: string | null;
  className?: string;
  inputClassName?: string;
} & Omit<InputHTMLAttributes<HTMLInputElement>, "id" | "className">) {
  const errorId = error ? `${id}-error` : null;
  const noteId = note ? `${id}-note` : null;
  const hintId = hint ? `${id}-hint` : null;
  const describedBy = [errorId, noteId, hintId].filter(Boolean).join(" ") || undefined;
  return (
    <div className={cx("min-w-0 space-y-1.5", className)}>
      <label htmlFor={id} className="block text-xs font-medium text-ink-2">
        {label}
        {optional ? <span className="font-normal text-muted"> (optional)</span> : null}
      </label>
      <Input
        id={id}
        invalid={Boolean(error)}
        aria-describedby={describedBy}
        aria-required={optional ? undefined : true}
        className={inputClassName}
        {...input}
      />
      {error ? (
        <p id={errorId ?? undefined} className="flex items-start gap-1 text-xs text-danger">
          <XCircle className="mt-px size-3.5 shrink-0" aria-hidden />
          <span>{error}</span>
        </p>
      ) : null}
      {note ? (
        <div id={noteId ?? undefined} className="text-xs">
          {note}
        </div>
      ) : null}
      {hint ? (
        <p id={hintId ?? undefined} className="text-xs text-muted">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

/** A confirmation under a field ("Check digit OK"). */
export function OkNote({ children }: { children: ReactNode }) {
  return (
    <p className="flex items-start gap-1 text-verified">
      <Check className="mt-px size-3.5 shrink-0" aria-hidden />
      <span>{children}</span>
    </p>
  );
}

/** One pass/fail line of the status page. */
export function CheckRow({ ok, label, detail }: { ok: boolean | null; label: string; detail?: ReactNode }) {
  return (
    <li className="flex items-start justify-between gap-3 py-2 text-sm">
      <span className="text-ink-2">{label}</span>
      <span
        className={cx(
          "inline-flex shrink-0 items-center gap-1 text-xs font-semibold",
          ok === null ? "text-muted" : ok ? "text-verified" : "text-danger",
        )}
      >
        {ok === null ? "Not checked" : ok ? <Check className="size-3.5" aria-hidden /> : <XCircle className="size-3.5" aria-hidden />}
        {ok === null ? null : ok ? "Passed" : "Failed"}
        {detail ? <span className="font-normal text-muted">{detail}</span> : null}
      </span>
    </li>
  );
}
