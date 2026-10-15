"use client";

import { useMemo, useRef, useState, type ChangeEvent } from "react";
import { ArrowLeft, ArrowRight, Camera, CheckCircle2, IdCard, Loader2, Upload } from "lucide-react";

import { Alert, Button, cx } from "@/components/ui/primitives";
import { cleanIdInput, todayInMexicoCity, type CredentialField, type ParsedCurp } from "@/lib/kyc";

import { CameraCapture } from "./camera-capture";
import {
  ACCEPTED_IMAGE_TYPES,
  CREDENTIAL_FIELDS,
  CREDENTIAL_INPUT_IDS,
  ID_MAX_SIDE,
  checkCredentialDraft,
  checkIneImages,
  expectedClaveDate,
  type IneDraft,
} from "./logic";
import { ImageError, reencodeImageFile } from "./media";
import { FormField, OkNote } from "./parts";

type Side = "frente" | "reverso";

const SIDES: Record<Side, { title: string; label: string; contents: string; guide: string }> = {
  frente: {
    title: "Front",
    label: "INE front",
    contents: "Your photo, name, address and voter key.",
    guide: "Fit the front of your INE inside the frame",
  },
  reverso: {
    title: "Back",
    label: "INE back",
    contents: "The QR codes and the machine-readable lines at the bottom.",
    guide: "Fit the back of your INE inside the frame",
  },
};

/** Length at which a field's error appears without waiting for blur. */
const COMPLETE_AT: Record<CredentialField, number> = {
  clave_elector: 18,
  cic: 9,
  identificador_ciudadano: 13,
  emision: 4,
  vigencia: 4,
};

/** Step 2: both sides of the voting credential and its typed data. */
export function StepIne({
  curp,
  draft,
  onChange,
  previousAttempt,
  onViewStatus,
  onBack,
  onContinue,
}: {
  /** CURP saved in step 1: the credential must belong to the same person. */
  curp: ParsedCurp;
  draft: IneDraft;
  onChange: (update: (draft: IneDraft) => IneDraft) => void;
  /** Reasons of the last rejected attempt, when the customer is retrying. */
  previousAttempt: { errors: string[]; remaining: number } | null;
  onViewStatus: () => void;
  onBack: () => void;
  onContinue: () => void;
}) {
  const [camera, setCamera] = useState<Side | null>(null);
  const [touched, setTouched] = useState<Partial<Record<CredentialField, boolean>>>({});
  const [submitted, setSubmitted] = useState(false);

  const today = useMemo(() => todayInMexicoCity(), []);
  const check = useMemo(() => checkCredentialDraft(draft, curp, today), [draft, curp, today]);
  const imageErrors = submitted ? checkIneImages(draft) : {};

  const errorFor = (field: CredentialField) =>
    submitted || touched[field] || draft[field].length >= COMPLETE_AT[field] ? check.fieldErrors[field] : undefined;
  const set = (field: CredentialField, value: string) => onChange((d) => ({ ...d, [field]: value }));
  const touch = (field: CredentialField) => setTouched((t) => (t[field] ? t : { ...t, [field]: true }));

  const onSubmit = () => {
    setSubmitted(true);
    const missing = checkIneImages(draft);
    const firstSide = (["frente", "reverso"] as const).find((s) => missing[s]);
    if (firstSide) {
      document.getElementById(`ine-${firstSide}-camera`)?.focus();
      return;
    }
    if (!check.input) {
      const first = CREDENTIAL_FIELDS.find((f) => check.fieldErrors[f]);
      if (first) document.getElementById(CREDENTIAL_INPUT_IDS[first])?.focus();
      return;
    }
    setCamera(null);
    onContinue();
  };

  const claveOk = draft.clave_elector.length === 18 && !check.fieldErrors.clave_elector;

  return (
    <div className="space-y-6">
      {previousAttempt ? (
        <Alert tone="warning" title="Your previous attempt was not approved">
          {previousAttempt.errors.length > 0 ? (
            <ul className="list-disc space-y-0.5 pl-4">
              {previousAttempt.errors.map((e) => (
                <li key={e}>{e}</li>
              ))}
            </ul>
          ) : null}
          <p className="mt-1">
            Retake the photos if needed. You have {previousAttempt.remaining}{" "}
            {previousAttempt.remaining === 1 ? "attempt" : "attempts"} left.{" "}
            <button type="button" onClick={onViewStatus} className="font-semibold underline underline-offset-2">
              View details
            </button>
          </p>
        </Alert>
      ) : null}

      <fieldset className="space-y-4">
        <legend className="mb-1 text-sm font-semibold text-ink">Photos of your credential</legend>
        <ul className="mb-3 list-disc space-y-0.5 pl-4 text-xs text-muted">
          <li>Use your original INE, not a photocopy or a picture of a screen.</li>
          <li>Good light, no glare, all four corners visible and nothing covering the text.</li>
          <li>Photos are re-encoded on your device; location and device metadata are removed.</li>
        </ul>
        {(["frente", "reverso"] as const).map((side) => (
          <IdSide
            key={side}
            side={side}
            value={draft[side]}
            error={imageErrors[side] ?? null}
            cameraOpen={camera === side}
            onOpenCamera={() => setCamera(side)}
            onCloseCamera={() => setCamera(null)}
            onImage={(dataUrl) => {
              setCamera(null);
              onChange((d) => ({ ...d, [side]: dataUrl }));
            }}
          />
        ))}
      </fieldset>

      <fieldset className="space-y-4">
        <legend className="mb-3 text-sm font-semibold text-ink">Credential details</legend>
        <FormField
          id="clave-elector"
          label="Voter key (clave de elector)"
          value={draft.clave_elector}
          onChange={(e) => set("clave_elector", cleanIdInput(e.target.value).slice(0, 18))}
          onBlur={() => touch("clave_elector")}
          error={errorFor("clave_elector")}
          autoComplete="off"
          autoCapitalize="characters"
          spellCheck={false}
          inputClassName="font-mono tracking-wide uppercase"
          placeholder="18 characters"
          note={claveOk ? <OkNote>Birth date and sex match your CURP</OkNote> : null}
          hint={`On the front ("CLAVE DE ELECTOR"). Characters 7–12 are your birth date: ${expectedClaveDate(curp)}.`}
        />
        <div className="grid grid-cols-2 gap-4">
          <FormField
            id="cic"
            label="CIC"
            value={draft.cic}
            onChange={(e) => set("cic", e.target.value.replace(/\D/g, "").slice(0, 9))}
            onBlur={() => touch("cic")}
            error={errorFor("cic")}
            autoComplete="off"
            inputMode="numeric"
            inputClassName="font-mono tabular-nums"
            placeholder="9 digits"
            hint={'On the back: the 9 digits right after "IDMEX" in the first line at the bottom.'}
          />
          <FormField
            id="identificador"
            label="Citizen identifier"
            optional
            value={draft.identificador_ciudadano}
            onChange={(e) => set("identificador_ciudadano", e.target.value.replace(/\D/g, "").slice(0, 13))}
            onBlur={() => touch("identificador_ciudadano")}
            error={errorFor("identificador_ciudadano")}
            autoComplete="off"
            inputMode="numeric"
            inputClassName="font-mono tabular-nums"
            placeholder="9 or 13 digits"
            hint="The last 9 digits of that same line (13-digit OCR on older credentials)."
          />
        </div>
        <div className="grid grid-cols-2 gap-4">
          <FormField
            id="emision"
            label="Issue year (emisión)"
            value={draft.emision}
            onChange={(e) => set("emision", e.target.value.replace(/\D/g, "").slice(0, 4))}
            onBlur={() => touch("emision")}
            error={errorFor("emision")}
            autoComplete="off"
            inputMode="numeric"
            inputClassName="tabular-nums"
            placeholder="YYYY"
          />
          <FormField
            id="vigencia"
            label="Expiry year (vigencia)"
            value={draft.vigencia}
            onChange={(e) => set("vigencia", e.target.value.replace(/\D/g, "").slice(0, 4))}
            onBlur={() => touch("vigencia")}
            error={errorFor("vigencia")}
            autoComplete="off"
            inputMode="numeric"
            inputClassName="tabular-nums"
            placeholder="YYYY"
            hint="Valid through December 31 of that year."
          />
        </div>
        {check.warnings.length > 0 ? (
          <Alert tone="warning" title="Please double-check">
            {check.warnings.join(" ")} You can continue; a person will review your file.
          </Alert>
        ) : null}
      </fieldset>

      <div className="flex gap-3">
        <Button type="button" variant="secondary" size="lg" onClick={onBack}>
          <ArrowLeft className="size-4" aria-hidden />
          Back
        </Button>
        <Button type="button" size="lg" className="flex-1" onClick={onSubmit}>
          Continue
          <ArrowRight className="size-4" aria-hidden />
        </Button>
      </div>
    </div>
  );
}

/** One side of the credential: thumbnail, camera capture or file upload. */
function IdSide({
  side,
  value,
  error,
  cameraOpen,
  onOpenCamera,
  onCloseCamera,
  onImage,
}: {
  side: Side;
  value: string | null;
  error: string | null;
  cameraOpen: boolean;
  onOpenCamera: () => void;
  onCloseCamera: () => void;
  onImage: (dataUrl: string) => void;
}) {
  const info = SIDES[side];
  const fileRef = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const message = uploadError ?? error;
  const titleId = `ine-${side}-title`;
  const messageId = `ine-${side}-error`;

  const onFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    // Reset so choosing the same file again still fires a change.
    event.target.value = "";
    if (!file) return;
    setBusy(true);
    setUploadError(null);
    try {
      onImage(await reencodeImageFile(file, ID_MAX_SIDE, info.label));
    } catch (e) {
      setUploadError(e instanceof ImageError ? e.message : `${info.label}: we could not read that file as an image.`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      role="group"
      aria-labelledby={titleId}
      aria-describedby={message ? messageId : undefined}
      className={cx("space-y-3 rounded-lg border p-3", message ? "border-danger/50" : "border-border")}
    >
      <div className="flex items-start gap-3">
        <div className="flex h-16 w-24 shrink-0 items-center justify-center overflow-hidden rounded-md border border-dashed border-border-strong bg-surface-2">
          {value ? (
            // eslint-disable-next-line @next/next/no-img-element -- local data URL, nothing to optimize
            <img src={value} alt={`Captured ${info.title.toLowerCase()} of your INE`} className="size-full object-cover" />
          ) : busy ? (
            <Loader2 className="size-5 animate-spin text-muted" aria-hidden />
          ) : (
            <IdCard className="size-6 text-muted" aria-hidden />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <p id={titleId} className="flex items-center gap-1.5 text-sm font-semibold text-ink">
            {info.title} of your INE
            {value ? (
              <>
                <CheckCircle2 className="size-4 text-verified" aria-hidden />
                <span className="sr-only">(captured)</span>
              </>
            ) : null}
          </p>
          <p className="text-xs text-muted">{busy ? "Processing photo…" : info.contents}</p>
        </div>
      </div>

      {cameraOpen ? (
        <CameraCapture label={info.label} guide={info.guide} onCapture={onImage} onCancel={onCloseCamera} />
      ) : (
        <div className="flex flex-wrap gap-2">
          <Button id={`ine-${side}-camera`} type="button" variant="secondary" size="sm" onClick={onOpenCamera} disabled={busy}>
            <Camera className="size-3.5" aria-hidden />
            {value ? "Retake with camera" : "Use camera"}
          </Button>
          <Button type="button" variant="secondary" size="sm" onClick={() => fileRef.current?.click()} disabled={busy}>
            <Upload className="size-3.5" aria-hidden />
            {value ? "Upload another" : "Upload"}
          </Button>
        </div>
      )}
      <input
        ref={fileRef}
        id={`ine-${side}-file`}
        type="file"
        accept={ACCEPTED_IMAGE_TYPES.join(",")}
        className="sr-only"
        tabIndex={-1}
        aria-hidden
        onChange={(e) => void onFile(e)}
      />

      {message ? (
        <p id={messageId} className="text-xs text-danger">
          {message}
        </p>
      ) : null}
    </div>
  );
}
