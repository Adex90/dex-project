"use client";

import { useState, type FormEvent } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { AlertTriangle, UserPlus } from "lucide-react";

import { formatDate } from "@/components/ui/data";
import { Alert, Button, Field, Input, cx, fieldDescribedBy } from "@/components/ui/primitives";
import { ONBOARDING_PATH, useClientHomeRedirect } from "@/hooks/use-client-home";
import { describeError } from "@/lib/api/client";
import type { RegistroInput, TipoPersona } from "@/lib/api/types";
import { COMPLIANCE_EMAIL } from "@/lib/config";
import {
  KYC_MESSAGES,
  SEXO_LABELS,
  ageOn,
  cleanIdInput,
  curpNameWarnings,
  entidadName,
  parseCurp,
  rfcCheckDigit,
  rfcCurpCheck,
  todayInMexicoCity,
  validateRfc,
  type ParsedCurp,
} from "@/lib/kyc";
import { useAuthStore } from "@/store/auth";

const MIN_PASSWORD = 12;
/** A company's RFC (persona moral): 3 letters, incorporation date YYMMDD, homoclave and check digit. */
const COMPANY_RFC_RE = /^[A-ZÑ&]{3}[0-9]{6}[A-Z0-9]{3}$/;

interface FormState {
  tipo: TipoPersona;
  nombre: string;
  paterno: string;
  materno: string;
  email: string;
  password: string;
  confirm: string;
  curp: string;
  rfc: string;
}

type Errors = Partial<Record<keyof FormState, string>>;

/** Form order, for focusing the first invalid input after a failed submit. */
const FIELD_ORDER: readonly (keyof FormState)[] = ["nombre", "paterno", "email", "password", "confirm", "curp", "rfc"];

/** Company RFC as the backend checks it at sign-up (kyc::rfc_valido): structure and check digit. */
function companyRfcError(rfc: string): string | null {
  if ([...rfc].length !== 12) return "A company's RFC has 12 characters.";
  if (!COMPANY_RFC_RE.test(rfc)) return "The RFC does not follow the official format (3 letters, date YYMMDD and homoclave).";
  if (rfcCheckDigit(rfc.slice(0, 11)) !== rfc.slice(11)) return KYC_MESSAGES.rfcCheckDigit;
  return null;
}

interface IdCheck {
  curp?: string;
  rfc?: string;
  /** Valid CURP of an individual (birth date, sex and state derived from it). */
  parsed: ParsedCurp | null;
  /** Non-blocking: identity verification will go to a manual review. */
  warnings: string[];
}

/**
 * The optional CURP and RFC, checked with the same rules identity
 * verification applies later (src/lib/kyc.ts mirrors backend/src/kyc): a
 * CURP or RFC accepted here will not be rejected at step 1 of onboarding.
 */
function checkIds(f: FormState, today: string): IdCheck {
  const out: IdCheck = { parsed: null, warnings: [] };
  const curp = f.tipo === "FISICA" ? f.curp.trim() : "";
  const rfc = f.rfc.trim().toUpperCase();
  if (curp) {
    const parsed = parseCurp(curp);
    if (!parsed.ok) {
      out.curp = parsed.error;
    } else {
      const age = ageOn(parsed.value.fechaNacimiento, today);
      if (age < 18) out.curp = KYC_MESSAGES.underage;
      else if (age > 120) out.curp = KYC_MESSAGES.implausibleBirth;
      else out.parsed = parsed.value;
    }
  }
  if (rfc) {
    if (f.tipo === "MORAL") {
      out.rfc = companyRfcError(rfc) ?? undefined;
    } else {
      const valid = validateRfc(rfc);
      if (!valid.ok) {
        out.rfc = valid.error;
      } else if (out.parsed) {
        const cross = rfcCurpCheck(valid.value, out.parsed);
        if (cross.errors[0]) out.rfc = cross.errors[0];
        out.warnings.push(...cross.warnings);
      }
    }
  }
  if (out.parsed && f.nombre.trim() && f.paterno.trim()) {
    out.warnings.push(...curpNameWarnings(out.parsed, f.nombre, f.paterno, f.materno.trim() || null));
  }
  return out;
}

function validate(f: FormState, ids: IdCheck): Errors {
  const e: Errors = {};
  if (!f.nombre.trim()) e.nombre = f.tipo === "FISICA" ? "Enter your given name." : "Enter the legal name.";
  if (f.tipo === "FISICA" && !f.paterno.trim()) e.paterno = "Enter your paternal surname.";
  if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(f.email.trim())) e.email = "Enter a valid email address.";
  if ([...f.password].length < MIN_PASSWORD) e.password = `Use at least ${MIN_PASSWORD} characters.`;
  if (f.confirm !== f.password) e.confirm = "Passwords do not match.";
  if (ids.curp) e.curp = ids.curp;
  if (ids.rfc) e.rfc = ids.rfc;
  return e;
}

export default function RegisterPage() {
  const router = useRouter();
  const register = useAuthStore((s) => s.register);
  // Already signed in: verified clients go to the dashboard, the rest to onboarding.
  const { claim, release } = useClientHomeRedirect();
  const [form, setForm] = useState<FormState>({
    tipo: "FISICA",
    nombre: "",
    paterno: "",
    materno: "",
    email: "",
    password: "",
    confirm: "",
    curp: "",
    rfc: "",
  });
  const [touched, setTouched] = useState(false);
  // ID fields are checked as soon as they are left, not only on submit.
  const [blurred, setBlurred] = useState<{ curp?: boolean; rfc?: boolean }>({});
  const [idsOpen, setIdsOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const ids = checkIds(form, todayInMexicoCity());
  const allErrors = validate(form, ids);
  const errors: Errors = touched
    ? allErrors
    : { ...(blurred.curp ? { curp: allErrors.curp } : {}), ...(blurred.rfc ? { rfc: allErrors.rfc } : {}) };

  const set = <K extends keyof FormState>(key: K, value: FormState[K]) => setForm((f) => ({ ...f, [key]: value }));

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setTouched(true);
    setError(null);
    if (allErrors.curp || allErrors.rfc) setIdsOpen(true);
    const firstInvalid = FIELD_ORDER.find((f) => allErrors[f]);
    if (firstInvalid) {
      // After the commit: the IDs section may only now be opening.
      requestAnimationFrame(() => document.getElementById(firstInvalid)?.focus());
      return;
    }
    const input: RegistroInput = {
      tipo_persona: form.tipo,
      nombre: form.nombre.trim(),
      email: form.email.trim(),
      password: form.password,
      ...(form.tipo === "FISICA" && form.paterno.trim() ? { apellido_paterno: form.paterno.trim() } : {}),
      ...(form.tipo === "FISICA" && form.materno.trim() ? { apellido_materno: form.materno.trim() } : {}),
      ...(form.tipo === "FISICA" && ids.parsed ? { curp: ids.parsed.curp } : {}),
      ...(form.rfc.trim() ? { rfc: form.rfc.trim().toUpperCase() } : {}),
    };
    setSubmitting(true);
    claim();
    try {
      await register(input);
      // Every new client starts with a PENDIENTE KYC file: deposits, transfers
      // and investments stay locked until identity verification is complete.
      router.replace(ONBOARDING_PATH);
    } catch (e) {
      release();
      setError(describeError(e));
      setSubmitting(false);
    }
  };

  return (
    <form onSubmit={onSubmit} className="space-y-5" noValidate>
      <div className="space-y-1">
        <h1 className="text-xl font-semibold tracking-tight">Open an account</h1>
        <p className="text-sm text-muted">
          {form.tipo === "MORAL"
            ? "You get a vault account with its own CLABE right away. Deposits, transfers and investments unlock once your company is verified at a branch with its legal representative."
            : "You get a vault account with its own CLABE right away. Next, verify your identity to unlock deposits, transfers and investments."}
        </p>
      </div>

      {error ? <Alert tone="error">{error}</Alert> : null}

      <fieldset className="grid grid-cols-2 gap-1 rounded-lg border border-border bg-surface-2 p-1">
        <legend className="sr-only">Account holder type</legend>
        {(
          [
            ["FISICA", "Individual"],
            ["MORAL", "Company"],
          ] as const
        ).map(([value, label]) => (
          <label
            key={value}
            className={cx(
              "cursor-pointer rounded-md px-3 py-1.5 text-center text-sm font-medium transition",
              form.tipo === value ? "bg-surface text-ink shadow-sm" : "text-muted hover:text-ink",
            )}
          >
            <input
              type="radio"
              name="tipo"
              value={value}
              checked={form.tipo === value}
              onChange={() => set("tipo", value)}
              className="sr-only"
            />
            {label}
          </label>
        ))}
      </fieldset>

      <Field label={form.tipo === "FISICA" ? "Given name(s)" : "Legal name"} htmlFor="nombre" error={errors.nombre}>
        <Input
          id="nombre"
          autoComplete={form.tipo === "FISICA" ? "given-name" : "organization"}
          value={form.nombre}
          onChange={(e) => set("nombre", e.target.value)}
          invalid={Boolean(errors.nombre)}
          aria-describedby={fieldDescribedBy("nombre", errors.nombre, undefined)}
        />
      </Field>

      {form.tipo === "FISICA" ? (
        <div className="grid grid-cols-2 gap-3">
          <Field label="Paternal surname" htmlFor="paterno" error={errors.paterno}>
            <Input
              id="paterno"
              autoComplete="family-name"
              value={form.paterno}
              onChange={(e) => set("paterno", e.target.value)}
              invalid={Boolean(errors.paterno)}
            aria-describedby={fieldDescribedBy("paterno", errors.paterno, undefined)}
            />
          </Field>
          <Field label="Maternal surname" htmlFor="materno" hint="Optional">
            <Input
              id="materno"
              value={form.materno}
              onChange={(e) => set("materno", e.target.value)}
              aria-describedby="materno-hint"
            />
          </Field>
        </div>
      ) : null}

      <Field label="Email" htmlFor="email" error={errors.email}>
        <Input
          id="email"
          type="email"
          autoComplete="email"
          value={form.email}
          onChange={(e) => set("email", e.target.value)}
          invalid={Boolean(errors.email)}
          aria-describedby={fieldDescribedBy("email", errors.email, undefined)}
        />
      </Field>

      <div className="grid grid-cols-2 gap-3">
        <Field label="Password" htmlFor="password" error={errors.password} hint={`${MIN_PASSWORD}+ characters`}>
          <Input
            id="password"
            type="password"
            autoComplete="new-password"
            value={form.password}
            onChange={(e) => set("password", e.target.value)}
            invalid={Boolean(errors.password)}
            aria-describedby={fieldDescribedBy("password", errors.password, `${MIN_PASSWORD}+ characters`)}
          />
        </Field>
        <Field label="Confirm password" htmlFor="confirm" error={errors.confirm}>
          <Input
            id="confirm"
            type="password"
            autoComplete="new-password"
            value={form.confirm}
            onChange={(e) => set("confirm", e.target.value)}
            invalid={Boolean(errors.confirm)}
            aria-describedby={fieldDescribedBy("confirm", errors.confirm, undefined)}
          />
        </Field>
      </div>

      <details
        className="group rounded-lg border border-border px-3 py-2 text-sm"
        open={idsOpen}
        onToggle={(e) => setIdsOpen(e.currentTarget.open)}
      >
        <summary className="cursor-pointer text-ink-2 select-none">Tax and identity IDs (optional)</summary>
        {/* One column: an 18-character CURP does not fit in half of the card. */}
        <div className="mt-3 grid gap-3">
          {form.tipo === "FISICA" ? (
            <Field
              label="CURP"
              htmlFor="curp"
              error={errors.curp}
              hint={
                ids.parsed && !allErrors.curp
                  ? `Born ${formatDate(ids.parsed.fechaNacimiento)} · ${SEXO_LABELS[ids.parsed.sexo]} · ${entidadName(ids.parsed.entidad) ?? ids.parsed.entidad}`
                  : "18 characters, as on your INE"
              }
            >
              <Input
                id="curp"
                autoComplete="off"
                spellCheck={false}
                value={form.curp}
                // Not maxLength: it would cut a pasted value before its spaces are removed.
                onChange={(e) => set("curp", cleanIdInput(e.target.value).slice(0, 18))}
                onBlur={() => setBlurred((b) => ({ ...b, curp: true }))}
                invalid={Boolean(errors.curp)}
                aria-describedby={fieldDescribedBy("curp", errors.curp, true)}
                className="font-mono uppercase"
              />
            </Field>
          ) : null}
          <Field
            label="RFC"
            htmlFor="rfc"
            error={errors.rfc}
            hint={form.tipo === "FISICA" ? "13 characters, with homoclave" : "12 characters, with homoclave"}
          >
            <Input
              id="rfc"
              autoComplete="off"
              spellCheck={false}
              value={form.rfc}
              onChange={(e) => set("rfc", cleanIdInput(e.target.value).slice(0, 13))}
              onBlur={() => setBlurred((b) => ({ ...b, rfc: true }))}
              invalid={Boolean(errors.rfc)}
              aria-describedby={fieldDescribedBy("rfc", errors.rfc, true)}
              className="font-mono uppercase"
            />
          </Field>
        </div>
        {ids.warnings.length && !allErrors.curp && !allErrors.rfc ? (
          <div
            className="mt-3 flex gap-2 rounded-lg border border-warning/30 bg-warning-soft px-3 py-2 text-[12px] leading-relaxed text-warning"
            role="status"
            data-testid="register-id-warnings"
          >
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
            <div className="space-y-1">
              {ids.warnings.map((w) => (
                <p key={w}>{w}</p>
              ))}
              <p>
                You can still open the account, but your identity verification will be reviewed by a person instead of
                being approved automatically. Check for typos first.
              </p>
            </div>
          </div>
        ) : null}
      </details>

      <Button type="submit" className="w-full" size="lg" loading={submitting}>
        <UserPlus className="size-4" aria-hidden />
        Create account
      </Button>

      <p className="text-xs leading-relaxed text-ink-2">
        Synthetic Capital is responsible for the personal data you give here (name, email, CURP, RFC). To get our full
        privacy notice (aviso de privacidad integral) or exercise your ARCO rights, write to{" "}
        <a href={`mailto:${COMPLIANCE_EMAIL}`} className="font-medium break-all text-accent hover:underline">
          {COMPLIANCE_EMAIL}
        </a>
        .
      </p>

      <p className="text-center text-sm text-muted">
        Already a client?{" "}
        <Link href="/auth/login" className="font-medium text-accent hover:underline">
          Sign in
        </Link>
      </p>
    </form>
  );
}
