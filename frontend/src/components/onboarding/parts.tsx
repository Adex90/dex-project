"use client";

import type { InputHTMLAttributes, ReactNode, Ref } from "react";
import { Check, XCircle } from "lucide-react";

import { Input, cx } from "@/components/ui/primitives";

import { STEPS, type Step, type View } from "./logic";

// -----------------------------------------------------------------------------
// Progress
// -----------------------------------------------------------------------------

/**
 * Progress through the four steps: a gold bar in four segments, gold up to
 * the current step, with each step's number and label under it (labels from
 * sm up; on a phone the step's own heading says where you are). The current
 * step carries aria-current="step"; steps the customer may revisit are
 * buttons.
 */
export function Progress({
  current,
  canVisit,
  isDone,
  onVisit,
}: {
  current: View;
  canVisit: (step: Step) => boolean;
  /** Whether a step is shown as completed. */
  isDone: (step: Step) => boolean;
  onVisit: (step: Step) => void;
}) {
  return (
    <nav aria-label="Verification steps" data-testid="verify-progress">
      <ol className="grid grid-cols-4 gap-1.5">
        {STEPS.map(({ n, label }) => {
          const isCurrent = n === current;
          const done = !isCurrent && isDone(n);
          const visitable = !isCurrent && canVisit(n);
          const content = (
            <>
              <span
                aria-hidden
                className={cx(
                  "block h-1.5 w-full transition-colors",
                  isCurrent || done ? "bg-synthetic-yellow" : "bg-surface-2",
                )}
              />
              <span className="mt-2 flex min-w-0 items-baseline gap-1.5">
                <span
                  aria-hidden
                  className={cx(
                    "font-mono text-[11px] font-semibold tabular-nums",
                    isCurrent || done ? "text-synthetic-yellow" : "text-muted",
                  )}
                >
                  {String(n).padStart(2, "0")}
                </span>
                <span
                  className={cx(
                    "hidden truncate text-[10px] font-semibold tracking-[0.16em] uppercase sm:inline",
                    isCurrent ? "text-ink" : done ? "text-ink-2" : "text-muted",
                  )}
                >
                  {label}
                </span>
                <span className="sr-only">
                  Step {n}: {label}
                  {isCurrent ? " (current step)" : done ? " (completed)" : ""}
                </span>
              </span>
            </>
          );
          return (
            <li key={n} aria-current={isCurrent ? "step" : undefined} className="min-w-0">
              {visitable ? (
                <button
                  type="button"
                  onClick={() => onVisit(n)}
                  className="block w-full text-left focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-4 focus-visible:ring-offset-bg focus-visible:outline-none"
                >
                  {content}
                </button>
              ) : (
                <div>{content}</div>
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
 * Container of one view: a labelled, square panel whose heading receives focus
 * when the view changes (so screen readers announce it). Steps say "Step N of
 * 4"; the status page does not.
 */
export function StepFrame({
  view,
  title,
  description,
  headingRef,
  children,
}: {
  view: View;
  title: string;
  description?: ReactNode;
  headingRef?: Ref<HTMLHeadingElement>;
  children: ReactNode;
}) {
  const key = view === "status" ? "status" : `step-${view}`;
  const headingId = `onboarding-${key}-title`;
  return (
    <section
      data-testid={`onboarding-${key}`}
      aria-labelledby={headingId}
      className="animate-reveal space-y-6 border border-border bg-surface p-5 sm:p-7"
    >
      <div className="space-y-1.5">
        <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
          {view === "status" ? "Verification status" : `Step ${view} of 4`}
        </p>
        <h2
          id={headingId}
          ref={headingRef}
          tabIndex={-1}
          className="text-xl font-semibold tracking-tight text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
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
