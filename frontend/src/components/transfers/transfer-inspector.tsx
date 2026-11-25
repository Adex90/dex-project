"use client";

import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Check, ExternalLink, FileCheck2, MessageSquareText, Minus, Undo2, X } from "lucide-react";

import { CARD } from "@/components/transfers/transfer-list";
import {
  ClarificationStatusBadge,
  CopyButton,
  DirectionMark,
  TransferStatusBadge,
} from "@/components/transfers/transfer-parts";
import { Money } from "@/components/ui/data";
import { cx } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { newIdempotencyKey } from "@/lib/api/endpoints";
import { formatOrderTime } from "@/lib/order-history";
import {
  KIND_LABELS,
  MAX_DESCRIPTION,
  MIN_OTHER_DESCRIPTION,
  REASON_LABELS,
  bankLabel,
  cepDetailsText,
  cepNotice,
  characterCount,
  clarificationBlock,
  clarificationBlockText,
  clarificationProblem,
  clarificationRequest,
  clarificationSignature,
  clarificationStatusText,
  cleanDescription,
  formatOperationDate,
  isClosed,
  maskedAccount,
  reasonsFor,
  transferTimeline,
  transferTitle,
  type StepState,
} from "@/lib/transfers";
import type { Inspected } from "@/store/transfers";
import type {
  CepData,
  ClarificationReason,
  SpeiClarification,
  SpeiClarificationRequest,
  TrackedTransfer,
  TransferBank,
} from "@/types/transfers";

/*
 * The Transfer Inspector: one SPEI transfer in a drawer (a sheet along the
 * bottom on phones, a panel on the right from `sm` up).
 *
 * - Its amount, status and when it started.
 * - The clave de rastreo, with one-tap copy: banks find a transfer by it.
 * - The settlement timeline: Initiated → Sent to Banxico via STP → Settled,
 *   and Returned (or Not completed) when it came back.
 * - The Banxico CEP inspector: the details Banxico's CEP lookup asks for,
 *   "Copy details" and a link to Banxico's own page (never another site); or
 *   why there is no CEP yet (or ever, for a simulated transfer).
 * - The clarification: the one already open on it, or "Report a delay" /
 *   "Open a clarification" and its form. The words are neutral, and the
 *   backend enforces the same rules the form checks first.
 *
 * A native <dialog> opened as a modal: focus stays inside, the page behind
 * is inert, Escape closes (not while a clarification is being sent), and
 * closing returns focus to the row that opened it.
 */

export interface TransferInspectorProps {
  inspected: Inspected | null;
  /** The client's open and in-review clarifications, against the limit of 10. */
  activeCount: number;
  onClose: () => void;
  onRetry: () => void;
  /** Sends a clarification; resolves with it as the API answered it (validated), or throws. */
  onOpenClarification: (request: SpeiClarificationRequest) => Promise<SpeiClarification>;
  /** Shows the client's clarifications (the page's other tab). */
  onShowClarifications: () => void;
}

export function TransferInspector(props: TransferInspectorProps) {
  if (!props.inspected) return null;
  // A new dialog per transfer: nothing of one transfer's form leaks into another's.
  return <InspectorDialog key={props.inspected.key} {...props} inspected={props.inspected} />;
}

const EYEBROW = "text-[11px] font-semibold tracking-[0.18em] text-muted uppercase";
const PRIMARY = cx(
  "inline-flex min-h-11 items-center justify-center gap-2 bg-synthetic-yellow px-4 text-[11px] font-bold tracking-[0.18em] text-black uppercase transition-colors",
  "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed disabled:cursor-not-allowed disabled:opacity-40",
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
);
const QUIET = cx(
  "inline-flex min-h-11 items-center justify-center gap-2 border border-border px-4 text-[11px] font-semibold tracking-[0.18em] text-ink-2 uppercase transition-colors",
  "hover:border-synthetic-yellow/60 hover:text-synthetic-yellow disabled:cursor-not-allowed disabled:opacity-40",
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
);

function InspectorDialog({
  inspected,
  activeCount,
  onClose,
  onRetry,
  onOpenClarification,
  onShowClarifications,
}: TransferInspectorProps & { inspected: Inspected }) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const titleId = useId();
  const [busy, setBusy] = useState(false);
  const transfer = inspected.transfer;

  // A modal from the first frame; the page behind does not scroll meanwhile.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    dialog.showModal();
    headingRef.current?.focus();
    const root = document.documentElement;
    const overflow = root.style.overflow;
    root.style.overflow = "hidden";
    return () => {
      root.style.overflow = overflow;
      if (dialog.open) dialog.close();
    };
  }, []);

  /** Closing goes through the dialog, which hands focus back; its "close" event tells the page. */
  function requestClose() {
    if (!busy) dialogRef.current?.close();
  }

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onCancel={(event) => {
        // Escape: not while a clarification is on its way.
        if (busy) event.preventDefault();
      }}
      onClose={onClose}
      onClick={(event) => {
        // A click on the backdrop lands on the dialog element itself.
        if (event.target === dialogRef.current) requestClose();
      }}
      className={cx(
        // Phone: a sheet along the bottom edge. From sm: a full-height panel on the right.
        "inset-0 mx-0 mt-auto mb-0 h-fit max-h-[92dvh] w-full max-w-full overflow-y-auto overscroll-contain p-0",
        "sm:my-0 sm:mr-0 sm:ml-auto sm:h-dvh sm:max-h-dvh sm:max-w-lg",
        "border-t border-border bg-surface text-ink sm:border-t-0 sm:border-l",
        "backdrop:bg-black/75 backdrop:backdrop-blur-[2px]",
        "transition duration-200 starting:translate-y-6 starting:opacity-0 sm:starting:translate-x-6 sm:starting:translate-y-0",
        "motion-reduce:transition-none",
      )}
      data-testid="transfer-inspector"
      data-transfer-id={transfer?.transfer_id}
      data-status={inspected.status}
    >
      <header className="sticky top-0 z-10 border-b border-border bg-surface px-5 pt-4 pb-3.5">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
              {transfer ? `${KIND_LABELS[transfer.kind]} · SPEI` : "SPEI transfer"}
            </p>
            <h2
              id={titleId}
              ref={headingRef}
              tabIndex={-1}
              className="mt-1 truncate text-base font-semibold tracking-tight outline-none"
              data-testid="inspector-title"
            >
              {transfer ? transferTitle(transfer) : "Transfer details"}
            </h2>
          </div>
          <button
            type="button"
            onClick={requestClose}
            disabled={busy}
            aria-label="Close"
            data-testid="inspector-close"
            className="grid size-10 shrink-0 place-items-center border border-border text-muted transition-colors hover:border-border-strong hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none disabled:opacity-40"
          >
            <X className="size-4" aria-hidden />
          </button>
        </div>
      </header>

      {inspected.status === "loading" ? (
        <InspectorSkeleton />
      ) : inspected.status === "error" || !transfer ? (
        <div className="px-5 py-6" data-testid="inspector-error">
          <p role="alert" className="text-sm text-danger">
            {inspected.error instanceof ApiError && inspected.error.status === 404
              ? "This is not one of your SPEI transfers, or it no longer exists."
              : `Could not load this transfer. ${describeError(inspected.error)}`}
          </p>
          <button type="button" onClick={onRetry} className={cx(QUIET, "mt-4")}>
            Try again
          </button>
        </div>
      ) : (
        <>
          <Summary transfer={transfer} />
          <TrackingKey transfer={transfer} />
          <Timeline transfer={transfer} />
          <Details transfer={transfer} />
          <CepInspector transfer={transfer} />
          <ClarificationSection
            transfer={transfer}
            activeCount={activeCount}
            onOpenClarification={onOpenClarification}
            onShowClarifications={onShowClarifications}
            onBusy={setBusy}
          />
        </>
      )}
    </dialog>
  );
}

/* ------------------------------------------------------------------------ */

function Section({
  title,
  children,
  testId,
  last = false,
}: {
  title: ReactNode;
  children: ReactNode;
  testId?: string;
  /** The drawer's last section: no rule below, room to scroll clear of the edge. */
  last?: boolean;
}) {
  const id = useId();
  return (
    <section
      aria-labelledby={id}
      className={cx("px-5", last ? "pt-5 pb-8" : "border-b border-border py-5")}
      data-testid={testId}
    >
      <h3 id={id} className={EYEBROW}>
        {title}
      </h3>
      {children}
    </section>
  );
}

function Summary({ transfer }: { transfer: TrackedTransfer }) {
  const incoming = transfer.direction === "incoming";
  const back = transfer.status === "returned";
  return (
    <section className="border-b border-border px-5 py-5" data-testid="inspector-summary">
      <div className="flex items-start gap-3">
        <DirectionMark transfer={transfer} className="size-10" />
        <div className="min-w-0 flex-1">
          <p className="text-2xl font-semibold tracking-tight" data-testid="inspector-amount">
            <Money
              value={transfer.amount}
              sign={back ? undefined : incoming ? "+" : "-"}
              className={back ? "text-muted" : undefined}
            />
          </p>
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <TransferStatusBadge transfer={transfer} />
            <span className="text-xs text-muted tabular-nums">{formatOrderTime(transfer.initiated_at)}</span>
            {transfer.simulated ? (
              <span className="border border-border px-1.5 py-px text-[10px] font-semibold tracking-[0.16em] text-muted uppercase">
                Demo
              </span>
            ) : null}
          </div>
        </div>
      </div>
    </section>
  );
}

function TrackingKey({ transfer }: { transfer: TrackedTransfer }) {
  return (
    <Section title="Clave de rastreo · Tracking key" testId="inspector-key">
      <div className="mt-2 flex items-center gap-3 border border-border bg-bg py-2 pr-2 pl-3">
        {/* Never truncated: banks need every character. */}
        <p
          className="min-w-0 flex-1 font-mono text-base tracking-[0.06em] break-all text-ink select-all"
          data-testid="inspector-tracking-key"
        >
          {transfer.tracking_key}
        </p>
        <CopyButton value={transfer.tracking_key} label="Copy" variant="primary" testId="copy-tracking-key" />
      </div>
      <p className="mt-2 text-xs leading-relaxed text-muted">
        Banks find a SPEI transfer by this key. Quote it to your bank, or to us.
      </p>
    </Section>
  );
}

const STEP_WORDS: Readonly<Record<StepState, string>> = {
  done: "Done",
  current: "In progress",
  upcoming: "Next",
  skipped: "Did not happen",
  returned: "Came back",
};

function StepMarker({ state }: { state: StepState }) {
  const base = "relative z-[1] grid size-7 shrink-0 place-items-center";
  switch (state) {
    case "done":
      return (
        <span aria-hidden className={cx(base, "bg-synthetic-yellow text-black")}>
          <Check className="size-4" strokeWidth={2.75} />
        </span>
      );
    case "current":
      return (
        <span aria-hidden className={cx(base, "border-2 border-synthetic-yellow bg-surface")}>
          <span className="size-2 bg-synthetic-yellow motion-safe:animate-pulse" />
        </span>
      );
    case "upcoming":
      return <span aria-hidden className={cx(base, "border border-border-strong bg-surface")} />;
    case "skipped":
      return (
        <span aria-hidden className={cx(base, "border border-dashed border-border-strong bg-surface text-muted")}>
          <Minus className="size-3.5" />
        </span>
      );
    case "returned":
      return (
        <span aria-hidden className={cx(base, "border border-warning/60 bg-warning-soft text-warning")}>
          <Undo2 className="size-3.5" />
        </span>
      );
  }
}

function Timeline({ transfer }: { transfer: TrackedTransfer }) {
  const steps = transferTimeline(transfer);
  return (
    <Section title="Settlement">
      <ol className="mt-4" data-testid="transfer-timeline">
        {steps.map((step, i) => {
          const next = steps[i + 1];
          const when = step.at
            ? formatOrderTime(step.at)
            : step.date
              ? formatOperationDate(step.date)
              : step.state === "done"
                ? null
                : STEP_WORDS[step.state];
          return (
            <li
              key={step.key}
              className="relative flex gap-3 pb-5 last:pb-0"
              data-testid="timeline-step"
              data-step={step.key}
              data-state={step.state}
            >
              {next ? (
                <span
                  aria-hidden
                  className={cx(
                    "absolute top-7 bottom-0 left-[13px] w-px",
                    next.state === "done" ? "bg-synthetic-yellow/60" : "bg-border",
                  )}
                />
              ) : null}
              <StepMarker state={step.state} />
              <div className="min-w-0 flex-1 pt-0.5">
                <p className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
                  <span
                    className={cx(
                      "text-sm font-semibold",
                      step.state === "upcoming" || step.state === "skipped"
                        ? "text-muted"
                        : step.state === "returned"
                          ? "text-warning"
                          : "text-ink",
                    )}
                  >
                    {step.title}
                    <span className="sr-only">: {STEP_WORDS[step.state]}</span>
                  </span>
                  {when !== null ? (
                    <span className="text-xs text-muted tabular-nums" data-testid="timeline-when">
                      {when}
                    </span>
                  ) : null}
                </p>
                <p className="mt-0.5 text-xs leading-relaxed text-muted">{step.detail}</p>
              </div>
            </li>
          );
        })}
      </ol>
    </Section>
  );
}

function Row({ label, children, mono = false }: { label: string; children: ReactNode; mono?: boolean }) {
  return (
    <>
      <dt className="text-muted">{label}</dt>
      <dd className={cx("min-w-0 text-right break-words text-ink-2", mono && "font-mono text-xs break-all")}>
        {children}
      </dd>
    </>
  );
}

function Details({ transfer }: { transfer: TrackedTransfer }) {
  const { name, bank, account_last4 } = transfer.counterparty;
  const side = transfer.direction === "incoming" ? "From" : "To";
  const pendingDate = transfer.status === "pending" ? "Assigned when it is sent" : "—";
  return (
    <Section title="Details">
      <dl
        className="mt-3 grid grid-cols-[max-content_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm"
        data-testid="inspector-details"
      >
        {name !== null ? <Row label={side}>{name}</Row> : null}
        <Row label="Bank">{bankLabel(bank) ?? "—"}</Row>
        {account_last4 !== null ? (
          <Row label="Account" mono>
            {maskedAccount(account_last4)}
          </Row>
        ) : null}
        <Row label="Operation date">
          {transfer.operation_date ? formatOperationDate(transfer.operation_date) : pendingDate}
        </Row>
        <Row label="Concept">{transfer.concept ?? "—"}</Row>
        <Row label="Numeric reference" mono>
          {transfer.numeric_reference ?? "—"}
        </Row>
        {transfer.transaction_id ? (
          <Row label="Ledger movement" mono>
            {transfer.transaction_id}
          </Row>
        ) : null}
        {transfer.refund_transaction_id ? (
          <Row label="Refund movement" mono>
            {transfer.refund_transaction_id}
          </Row>
        ) : null}
      </dl>
    </Section>
  );
}

function bankWithCode(bank: TransferBank): string {
  return bank.name !== null ? `${bank.name} (${bank.code})` : (bankLabel(bank) ?? "—");
}

function CepFields({ cep, outgoing }: { cep: CepData; outgoing: boolean }) {
  return (
    <dl
      className="mt-3 grid grid-cols-[max-content_minmax(0,1fr)] gap-x-4 gap-y-2 border border-border bg-bg px-3 py-3 text-sm"
      data-testid="cep-fields"
    >
      <Row label="Operation date">
        {cep.operation_date ? formatOperationDate(cep.operation_date) : "Not assigned yet"}
      </Row>
      <Row label="Tracking key" mono>
        {cep.tracking_key}
      </Row>
      <Row label="Sender">{bankWithCode(cep.sender_bank)}</Row>
      <Row label="Receiver">{bankWithCode(cep.receiver_bank)}</Row>
      <Row label="Beneficiary account" mono>
        {cep.beneficiary_account ??
          (cep.beneficiary_account_last4 !== null ? maskedAccount(cep.beneficiary_account_last4) : "—")}
      </Row>
      <Row label="Amount">
        <Money value={cep.amount} />
      </Row>
      {outgoing && cep.beneficiary_account === null ? (
        <dd className="col-span-2 text-xs leading-relaxed text-muted">
          Banxico asks for the full beneficiary account: enter the CLABE you sent it to.
        </dd>
      ) : null}
    </dl>
  );
}

function CepInspector({ transfer }: { transfer: TrackedTransfer }) {
  const { cep } = transfer;
  const notice = cepNotice(transfer);
  return (
    <section
      aria-label="Banxico CEP"
      className="border-b border-border px-5 py-5"
      data-testid="cep-inspector"
      data-available={cep?.available ? "true" : "false"}
    >
      <div className="flex items-start gap-3">
        <span
          aria-hidden
          className="grid size-9 shrink-0 place-items-center bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
        >
          <FileCheck2 className="size-4" />
        </span>
        <div className="min-w-0">
          <h3 className="text-sm font-semibold tracking-tight">Banxico CEP</h3>
          <p className="mt-0.5 text-xs leading-relaxed text-muted">
            Comprobante Electrónico de Pago: Banxico&apos;s own record that the transfer settled.
          </p>
        </div>
      </div>
      {notice !== null ? (
        <p
          className="mt-3 border border-border bg-surface-2 px-3 py-2.5 text-xs leading-relaxed text-ink-2"
          data-testid="cep-notice"
        >
          {notice}
        </p>
      ) : null}
      {cep !== null ? (
        <>
          <CepFields cep={cep} outgoing={transfer.direction === "outgoing"} />
          {cep.available ? (
            <p className="mt-2 text-xs leading-relaxed text-muted">
              On Banxico&apos;s page, search by tracking key (clave de rastreo) and enter these details.
            </p>
          ) : null}
          <div className="mt-3 flex flex-wrap gap-2">
            {cep.available ? (
              <a href={cep.url} target="_blank" rel="noopener noreferrer" className={PRIMARY} data-testid="cep-link">
                Verify on Banxico
                <ExternalLink className="size-3.5" aria-hidden />
                <span className="sr-only">(opens Banxico&apos;s site in a new tab)</span>
              </a>
            ) : null}
            <CopyButton value={cepDetailsText(cep)} label="Copy details" className="min-h-11 px-4" testId="copy-cep" />
          </div>
        </>
      ) : null}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function ClarificationSection({
  transfer,
  activeCount,
  onOpenClarification,
  onShowClarifications,
  onBusy,
}: {
  transfer: TrackedTransfer;
  activeCount: number;
  onOpenClarification: (request: SpeiClarificationRequest) => Promise<SpeiClarification>;
  onShowClarifications: () => void;
  onBusy: (busy: boolean) => void;
}) {
  const [formOpen, setFormOpen] = useState(false);
  const [opened, setOpened] = useState<SpeiClarification | null>(null);
  const startRef = useRef<HTMLButtonElement>(null);
  const openedRef = useRef<HTMLParagraphElement>(null);
  const wasOpen = useRef(false);
  const latest = transfer.clarification;
  const block = clarificationBlock(transfer, activeCount);

  // Cancelling the form hands the focus back to the button that opened it.
  useEffect(() => {
    if (wasOpen.current && !formOpen && opened === null) startRef.current?.focus();
    wasOpen.current = formOpen;
  }, [formOpen, opened]);
  // Sent: the confirmation takes the focus (the form it replaces had it).
  useEffect(() => {
    if (opened !== null) openedRef.current?.focus();
  }, [opened]);

  return (
    <Section title="Clarification" testId="inspector-clarification" last>
      {opened ? (
        <div
          role="status"
          className="mt-3 border border-synthetic-yellow/50 bg-synthetic-yellow/10 px-4 py-3"
          data-testid="clarification-opened"
        >
          <p
            ref={openedRef}
            tabIndex={-1}
            className="flex flex-wrap items-center gap-2 text-sm font-semibold text-ink outline-none"
          >
            <Check className="size-4 text-synthetic-yellow" aria-hidden />
            Clarification #{opened.number} opened
          </p>
          <p className="mt-1 text-xs leading-relaxed text-ink-2">
            {clarificationStatusText(opened.status)} Follow it in Clarifications, quoting case #{opened.number}.
          </p>
          <button type="button" onClick={onShowClarifications} className={cx(QUIET, "mt-3")}>
            View clarifications
          </button>
        </div>
      ) : latest !== null && !isClosed(latest.status) ? (
        <div className="mt-3 border border-border bg-surface-2 px-4 py-3" data-testid="clarification-active">
          <p className="flex flex-wrap items-center gap-2 text-sm font-semibold text-ink">
            <MessageSquareText className="size-4 text-muted" aria-hidden />
            Clarification #{latest.number}
            <ClarificationStatusBadge status={latest.status} />
          </p>
          <p className="mt-1 text-xs leading-relaxed text-ink-2">{clarificationStatusText(latest.status)}</p>
          <button type="button" onClick={onShowClarifications} className={cx(QUIET, "mt-3")}>
            View clarifications
          </button>
        </div>
      ) : (
        <>
          {latest !== null ? (
            <p className="mt-3 flex flex-wrap items-center gap-2 text-xs text-muted" data-testid="clarification-closed">
              Clarification #{latest.number}
              <ClarificationStatusBadge status={latest.status} />
              <button
                type="button"
                onClick={onShowClarifications}
                className="font-semibold text-synthetic-yellow underline-offset-2 hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
              >
                See the note
              </button>
            </p>
          ) : null}
          {block !== null ? (
            <p
              className="mt-3 text-xs leading-relaxed text-muted"
              data-testid="clarification-blocked"
              data-block={block.kind}
            >
              {clarificationBlockText(block)}
            </p>
          ) : formOpen ? (
            <ClarificationForm
              transfer={transfer}
              onCancel={() => setFormOpen(false)}
              onSubmit={onOpenClarification}
              onDone={(clarification) => {
                setFormOpen(false);
                setOpened(clarification);
              }}
              onBusy={onBusy}
            />
          ) : (
            <>
              <p className="mt-2 text-xs leading-relaxed text-muted">
                {transfer.status === "pending"
                  ? "Taking longer than it should? Tell us and our team will look into it with the banks involved."
                  : "Something not right with this transfer? Open a clarification and our team will review it."}
              </p>
              <button
                ref={startRef}
                type="button"
                onClick={() => setFormOpen(true)}
                className={cx(PRIMARY, "mt-3 w-full sm:w-auto")}
                data-testid="clarification-start"
              >
                <MessageSquareText className="size-4" aria-hidden />
                {transfer.status === "pending" ? "Report a delay" : "Open a clarification"}
              </button>
            </>
          )}
        </>
      )}
    </Section>
  );
}

function clarificationError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.code === "KYC_NO_VERIFICADO") return "Open a clarification once your identity is verified.";
    if (error.status === 404) return "This is not one of your SPEI transfers, or it no longer exists.";
  }
  return describeError(error);
}

function ClarificationForm({
  transfer,
  onCancel,
  onSubmit,
  onDone,
  onBusy,
}: {
  transfer: TrackedTransfer;
  onCancel: () => void;
  onSubmit: (request: SpeiClarificationRequest) => Promise<SpeiClarification>;
  onDone: (clarification: SpeiClarification) => void;
  onBusy: (busy: boolean) => void;
}) {
  const reasons = reasonsFor(transfer);
  const [reason, setReason] = useState<ClarificationReason>(reasons[0] ?? "other");
  const [description, setDescription] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [touched, setTouched] = useState(false);
  const attempt = useRef<{ signature: string; key: string } | null>(null);
  const legendRef = useRef<HTMLLegendElement>(null);
  const groupRef = useRef<HTMLDivElement>(null);
  const descriptionId = useId();
  const hintId = useId();
  const problem = clarificationProblem(reason, description);
  const length = characterCount(cleanDescription(description));
  const required = reason === "other";

  // Opened: the chosen reason takes the focus (the button that opened the form is gone), in view.
  useEffect(() => {
    groupRef.current?.querySelector<HTMLInputElement>("input:checked")?.focus({ preventScroll: true });
    legendRef.current?.scrollIntoView({ block: "nearest" });
  }, []);

  function setSending(next: boolean) {
    setBusy(next);
    onBusy(next);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setTouched(true);
    if (busy || problem !== null) return;
    // The same terms retried keep their key (a lost answer cannot open two); other terms get a new one.
    const signature = clarificationSignature(transfer.transfer_id, reason, description);
    if (attempt.current?.signature !== signature) attempt.current = { signature, key: newIdempotencyKey() };
    setSending(true);
    setError(null);
    try {
      const clarification = await onSubmit(clarificationRequest(transfer, reason, description, attempt.current.key));
      setSending(false);
      onDone(clarification);
    } catch (cause) {
      setSending(false);
      setError(cause);
    }
  }

  return (
    <form
      onSubmit={submit}
      noValidate
      className={cx(CARD, "mt-3 bg-surface-2 px-4 py-4")}
      data-testid="clarification-form"
    >
      <fieldset disabled={busy}>
        <legend ref={legendRef} className="text-sm font-semibold text-ink">
          What happened?
        </legend>
        <div ref={groupRef} className="mt-3 grid gap-px border border-border bg-border">
          {reasons.map((option) => {
            const selected = option === reason;
            return (
              <label
                key={option}
                className={cx(
                  "flex min-h-11 cursor-pointer items-center gap-3 px-3 text-sm transition-colors",
                  "has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-synthetic-yellow has-[:focus-visible]:ring-inset",
                  selected ? "bg-synthetic-yellow/10 text-ink" : "bg-surface text-ink-2 hover:bg-surface-hover",
                )}
                data-testid="clarification-reason"
                data-reason={option}
              >
                <input
                  type="radio"
                  name="clarification-reason"
                  value={option}
                  checked={selected}
                  onChange={() => {
                    setReason(option);
                    setError(null);
                  }}
                  className="peer sr-only"
                />
                <span
                  aria-hidden
                  className={cx(
                    "grid size-4 shrink-0 place-items-center border",
                    selected ? "border-synthetic-yellow bg-synthetic-yellow" : "border-border-strong",
                  )}
                >
                  {selected ? <span className="size-1.5 bg-black" /> : null}
                </span>
                {REASON_LABELS[option]}
              </label>
            );
          })}
        </div>

        <label htmlFor={descriptionId} className="mt-4 flex items-baseline justify-between gap-3 text-sm">
          <span className="font-semibold text-ink">
            Tell us more <span className="font-normal text-muted">{required ? "(required)" : "(optional)"}</span>
          </span>
          <span
            className={cx("text-xs tabular-nums", length > MAX_DESCRIPTION ? "text-danger" : "text-muted")}
            aria-hidden
          >
            {length}/{MAX_DESCRIPTION}
          </span>
        </label>
        <textarea
          id={descriptionId}
          value={description}
          onChange={(event) => {
            setDescription(event.target.value);
            setError(null);
          }}
          rows={4}
          aria-describedby={hintId}
          aria-invalid={touched && problem !== null ? true : undefined}
          className="mt-2 block w-full resize-y border border-border bg-bg px-3 py-2 text-sm text-ink placeholder:text-muted focus-visible:border-synthetic-yellow focus-visible:ring-1 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
          placeholder={required ? "What went wrong, in a few words" : "Anything that helps us look into it"}
          data-testid="clarification-description"
        />
        <p id={hintId} className={cx("mt-1.5 text-xs", touched && problem !== null ? "text-danger" : "text-muted")}>
          {touched && problem !== null
            ? problem
            : required
              ? `At least ${MIN_OTHER_DESCRIPTION} characters, up to ${MAX_DESCRIPTION}.`
              : `Up to ${MAX_DESCRIPTION} characters. Please don't include passwords or card numbers.`}
        </p>
      </fieldset>

      {error !== null ? (
        <p
          role="alert"
          className="mt-3 border border-danger/40 bg-danger-soft px-3 py-2 text-xs text-danger"
          data-testid="clarification-error"
        >
          {clarificationError(error)}
        </p>
      ) : null}

      <p className="mt-3 text-xs leading-relaxed text-muted">
        Your clarification is recorded with its case number and can&apos;t be edited. The transfer itself doesn&apos;t
        change while our team reviews it.
      </p>
      <div className="mt-4 flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
        <button type="button" onClick={onCancel} disabled={busy} className={QUIET}>
          Cancel
        </button>
        <button type="submit" disabled={busy} className={PRIMARY} data-testid="clarification-submit">
          {busy ? "Sending…" : "Send clarification"}
        </button>
      </div>
    </form>
  );
}

function InspectorSkeleton() {
  return (
    <div className="space-y-4 px-5 py-5 motion-safe:animate-pulse" role="status" data-testid="inspector-loading">
      <span className="sr-only">Loading the transfer…</span>
      <div aria-hidden className="flex items-start gap-3">
        <div className="size-10 bg-surface-2" />
        <div className="flex-1 space-y-2">
          <div className="h-7 w-40 bg-surface-2" />
          <div className="h-4 w-28 bg-surface-2" />
        </div>
      </div>
      <div aria-hidden className="h-14 w-full bg-surface-2" />
      {Array.from({ length: 3 }, (_, i) => (
        <div key={i} aria-hidden className="flex gap-3">
          <div className="size-7 bg-surface-2" />
          <div className="flex-1 space-y-2">
            <div className="h-4 w-36 bg-surface-2" />
            <div className="h-3 w-full bg-surface-2" />
          </div>
        </div>
      ))}
    </div>
  );
}
