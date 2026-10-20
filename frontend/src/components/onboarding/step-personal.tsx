"use client";

import { useMemo, useRef, useState, type FormEvent } from "react";
import { ArrowRight, Save } from "lucide-react";

import { Alert, Button } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { DatosGuardados, VistaExpediente } from "@/lib/api/types";
import {
  ESTADOS,
  PROFILE_LABELS,
  PROFILE_MAX_LENGTH,
  SEXO_LABELS,
  checkProfile,
  cleanIdInput,
  describeKycMessage,
  entidadName,
  todayInMexicoCity,
  type ProfileField,
} from "@/lib/kyc";

import {
  PROFILE_FIELDS,
  PROFILE_INPUT_IDS,
  profileFormFromFile,
  profileInputFromForm,
  profileProblem,
  type ProfileForm,
} from "./logic";
import { FormField, OkNote } from "./parts";

const birthDate = new Intl.DateTimeFormat("en-US", { year: "numeric", month: "long", day: "numeric", timeZone: "UTC" });

/** Fields whose error appears as soon as they are complete, without waiting for blur. */
const COMPLETE_AT: Partial<Record<ProfileField, number>> = { curp: 18, rfc: 13, codigo_postal: 5 };

function focusField(field: ProfileField) {
  document.getElementById(PROFILE_INPUT_IDS[field])?.focus();
}

/** Step 1: names, CURP, RFC and home address, validated as the customer types. */
export function StepPersonal({
  token,
  file,
  onSaved,
  onContinue,
  onReload,
  onUnauthorized,
}: {
  token: string;
  file: VistaExpediente;
  onSaved: (saved: DatosGuardados) => void;
  onContinue: () => void;
  onReload: () => void;
  onUnauthorized: () => void;
}) {
  const [form, setForm] = useState<ProfileForm>(() => profileFormFromFile(file));
  const [touched, setTouched] = useState<Partial<Record<ProfileField, boolean>>>({});
  const [submitted, setSubmitted] = useState(false);
  const [serverErrors, setServerErrors] = useState<Partial<Record<ProfileField, string>>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState<{ snapshot: string; warnings: string[] } | null>(null);
  const savedAlertRef = useRef<HTMLDivElement>(null);

  const today = useMemo(() => todayInMexicoCity(), []);
  const check = useMemo(() => checkProfile(profileInputFromForm(form), today), [form, today]);

  const set = (field: ProfileField, value: string) => {
    setForm((f) => ({ ...f, [field]: value }));
    setServerErrors((e) => {
      if (!(field in e)) return e;
      const next = { ...e };
      delete next[field];
      return next;
    });
  };
  const touch = (field: ProfileField) => setTouched((t) => (t[field] ? t : { ...t, [field]: true }));

  const visible = (field: ProfileField) => {
    const complete = COMPLETE_AT[field];
    return submitted || Boolean(touched[field]) || (complete !== undefined && form[field].length >= complete);
  };
  const errorFor = (field: ProfileField): string | undefined =>
    serverErrors[field] ?? (visible(field) ? check.errors[field] : undefined);

  const summary = submitted
    ? PROFILE_FIELDS.flatMap((f) => {
        const message = errorFor(f);
        return message ? [{ field: f, message }] : [];
      })
    : [];

  const savedCurrent = saved !== null && saved.snapshot === JSON.stringify(form);

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setSubmitted(true);
    setFormError(null);
    const body = check.body;
    if (!body) {
      const first = PROFILE_FIELDS.find((f) => check.errors[f]);
      if (first) focusField(first);
      return;
    }
    setSaving(true);
    try {
      const result = await api.kycSaveProfile(token, body);
      onSaved(result);
      if (result.advertencias.length === 0) {
        onContinue();
        return;
      }
      setSaved({ snapshot: JSON.stringify(form), warnings: result.advertencias.map(describeKycMessage) });
      requestAnimationFrame(() => savedAlertRef.current?.focus());
    } catch (error) {
      if (error instanceof ApiError && error.isUnauthorized) {
        onUnauthorized();
        return;
      }
      const problem = error instanceof ApiError ? profileProblem(error.code, error.message) : null;
      if (problem) {
        setFormError(problem.message);
        if (problem.field) {
          setServerErrors({ [problem.field]: problem.message });
          focusField(problem.field);
        }
        if (problem.reload) onReload();
      } else {
        setFormError(describeError(error));
      }
    } finally {
      setSaving(false);
    }
  };

  const text = (field: ProfileField, props: { autoComplete?: string; optional?: boolean; hint?: string; className?: string } = {}) => (
    <FormField
      id={PROFILE_INPUT_IDS[field]}
      label={PROFILE_LABELS[field]}
      optional={props.optional}
      hint={props.hint}
      error={errorFor(field)}
      className={props.className}
      value={form[field]}
      autoComplete={props.autoComplete ?? "off"}
      maxLength={field in PROFILE_MAX_LENGTH ? PROFILE_MAX_LENGTH[field as keyof typeof PROFILE_MAX_LENGTH] : undefined}
      onChange={(e) => set(field, e.target.value)}
      onBlur={() => touch(field)}
    />
  );

  const curp = check.curp;
  const rfcOk = curp !== null && !check.errors.rfc && form.rfc.length === 13;

  return (
    <form onSubmit={onSubmit} className="space-y-6" noValidate aria-describedby="step1-intro">
      <p id="step1-intro" className="sr-only">
        All fields are required unless marked optional. Birth date, sex and birth state are taken from your CURP.
      </p>

      {formError ? <Alert tone="error">{formError}</Alert> : null}

      {summary.length > 0 ? (
        <Alert tone="error" title={summary.length === 1 ? "Check 1 field" : `Check ${summary.length} fields`}>
          <ul className="list-disc space-y-0.5 pl-4">
            {summary.map(({ field, message }) => (
              <li key={field}>
                <a
                  href={`#${PROFILE_INPUT_IDS[field]}`}
                  className="underline underline-offset-2"
                  onClick={(e) => {
                    e.preventDefault();
                    focusField(field);
                  }}
                >
                  {message}
                </a>
              </li>
            ))}
          </ul>
        </Alert>
      ) : null}

      <fieldset className="space-y-4">
        <legend className="mb-3 text-sm font-semibold text-ink">Your name, exactly as on your INE</legend>
        {text("nombres", { autoComplete: "given-name" })}
        <div className="grid gap-4 sm:grid-cols-2">
          {text("apellido_paterno", { autoComplete: "family-name" })}
          {text("apellido_materno", { optional: true, hint: "Leave empty if you have only one surname." })}
        </div>
      </fieldset>

      <fieldset className="space-y-4">
        <legend className="mb-3 text-sm font-semibold text-ink">Identity numbers</legend>
        <FormField
          id="curp"
          label="CURP"
          value={form.curp}
          onChange={(e) => set("curp", cleanIdInput(e.target.value).slice(0, 18))}
          onBlur={() => touch("curp")}
          error={errorFor("curp")}
          autoComplete="off"
          autoCapitalize="characters"
          spellCheck={false}
          inputClassName="font-mono tracking-wide uppercase"
          placeholder="18 characters"
          note={
            curp ? (
              <OkNote>
                Born {birthDate.format(new Date(`${curp.fechaNacimiento}T00:00:00Z`))} · {SEXO_LABELS[curp.sexo]} ·{" "}
                {entidadName(curp.entidad) ?? curp.entidad}
              </OkNote>
            ) : null
          }
          hint={curp ? "Taken from your CURP. If this is not right, check the CURP." : "Your birth date, sex and birth state are read from it."}
        />
        <FormField
          id="rfc"
          label="RFC"
          value={form.rfc}
          onChange={(e) => set("rfc", cleanIdInput(e.target.value).slice(0, 13))}
          onBlur={() => touch("rfc")}
          error={errorFor("rfc")}
          autoComplete="off"
          autoCapitalize="characters"
          spellCheck={false}
          inputClassName="font-mono tracking-wide uppercase"
          placeholder="13 characters"
          note={rfcOk ? <OkNote>Check digit OK · birth date matches your CURP</OkNote> : null}
          hint={
            curp
              ? `Usually starts with ${curp.curp.slice(0, 10)} (from your CURP), followed by a 3-character homoclave.`
              : "Your tax ID (13 characters), as issued by the SAT."
          }
        />
        {check.warnings.length > 0 ? (
          <Alert tone="warning" title="Please double-check">
            <ul className="list-disc space-y-0.5 pl-4">
              {check.warnings.map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
            <p className="mt-1">
              You can continue, but a compliance analyst will review your file by hand, which takes longer.
            </p>
          </Alert>
        ) : null}
      </fieldset>

      <fieldset className="space-y-4">
        <legend className="mb-3 text-sm font-semibold text-ink">Home address</legend>
        {text("calle", { autoComplete: "address-line1" })}
        <div className="grid grid-cols-2 gap-4">
          {text("numero_exterior")}
          {text("numero_interior", { optional: true, autoComplete: "address-line2" })}
        </div>
        {text("colonia", { autoComplete: "address-level3" })}
        <FormField
          id="municipio"
          label="Municipality or borough (alcaldía)"
          value={form.municipio}
          onChange={(e) => set("municipio", e.target.value)}
          onBlur={() => touch("municipio")}
          error={errorFor("municipio")}
          autoComplete="address-level2"
          maxLength={PROFILE_MAX_LENGTH.municipio}
        />
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-[1fr_8rem]">
          <FormField
            id="estado"
            label={PROFILE_LABELS.estado}
            value={form.estado}
            onChange={(e) => set("estado", e.target.value)}
            onBlur={() => touch("estado")}
            error={errorFor("estado")}
            autoComplete="address-level1"
            maxLength={PROFILE_MAX_LENGTH.estado}
            list="onboarding-estados"
          />
          <FormField
            id="codigo-postal"
            label={PROFILE_LABELS.codigo_postal}
            value={form.codigo_postal}
            onChange={(e) => set("codigo_postal", e.target.value.replace(/\D/g, "").slice(0, 5))}
            onBlur={() => touch("codigo_postal")}
            error={errorFor("codigo_postal")}
            autoComplete="postal-code"
            inputMode="numeric"
            inputClassName="font-mono tabular-nums"
            placeholder="5 digits"
          />
        </div>
        <datalist id="onboarding-estados">
          {ESTADOS.map((e) => (
            <option key={e.code} value={e.name} />
          ))}
        </datalist>
      </fieldset>

      {saved && savedCurrent ? (
        <div ref={savedAlertRef} tabIndex={-1} className="space-y-3 rounded-lg focus:outline-none">
          <Alert tone="warning" title="Saved. A person will review these details">
            <ul className="list-disc space-y-0.5 pl-4">
              {saved.warnings.map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
            <p className="mt-1">Correct them and save again, or continue: your file will go to manual review.</p>
          </Alert>
          <Button type="button" className="w-full" size="lg" onClick={onContinue}>
            Continue with these details
            <ArrowRight className="size-4" aria-hidden />
          </Button>
        </div>
      ) : null}

      <Button
        type="submit"
        className="w-full"
        size="lg"
        variant={saved && savedCurrent ? "secondary" : "primary"}
        loading={saving}
      >
        <Save className="size-4" aria-hidden />
        Save and continue
      </Button>
    </form>
  );
}
