"use client";

import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Loader2, Lock, RotateCcw, ScanFace, VideoOff } from "lucide-react";

import { Alert, Button, cx } from "@/components/ui/primitives";
import { checkVerificationImages, imageLabel } from "@/lib/kyc";

import {
  FRAME_INTERVAL_MS,
  SELFIE_FRAMES,
  SELFIE_MAX_SIDE,
  describeCameraProblem,
  pickChallenge,
  type Challenge,
  type VerificationAction,
} from "./logic";
import { ImageError, captureVideoFrame, sleep, useCamera } from "./media";

/** What the page answers after POST /kyc/verificar (on success it moves to step 4). */
export type VerificationOutcome = { ok: true } | { ok: false; message: string; action: VerificationAction };

type Phase = "idle" | "starting" | "countdown" | "challenge" | "submitting" | "error";

const COUNTDOWN_FROM = 3;
const COUNTDOWN_TICK_MS = 800;
/** Extra captures allowed when a frame comes out byte-identical to an earlier one. */
const MAX_RECAPTURES = 6;

function randomUnit(): number {
  const n = crypto.getRandomValues(new Uint32Array(1))[0] ?? 0;
  return n / 2 ** 32;
}

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/**
 * Step 3: explicit biometric consent, then a live camera check: a random
 * challenge is shown while 4 distinct frames are captured, and everything
 * (INE photos + frames) is submitted for verification. There is deliberately
 * no file upload here: liveness must come from a live camera.
 */
export function StepLiveness({
  ineImages,
  onSubmit,
  onBack,
  onEditProfile,
}: {
  ineImages: { frente: string; reverso: string };
  onSubmit: (selfie: { frames: string[]; reto: string }) => Promise<VerificationOutcome>;
  onBack: () => void;
  onEditProfile: () => void;
}) {
  const { videoRef, state: camera, start, stop } = useCamera("user");
  const [consent, setConsent] = useState(false);
  const [phase, setPhase] = useState<Phase>("idle");
  const [count, setCount] = useState(COUNTDOWN_FROM);
  const [challenge, setChallenge] = useState<Challenge | null>(null);
  const [framesTaken, setFramesTaken] = useState(0);
  const [error, setError] = useState<{ message: string; action: VerificationAction } | null>(null);
  const runRef = useRef<AbortController | null>(null);
  const consentRef = useRef<HTMLInputElement>(null);

  useEffect(() => () => runRef.current?.abort(), []);

  const busy = phase === "starting" || phase === "countdown" || phase === "challenge" || phase === "submitting";

  const cancelRun = () => {
    runRef.current?.abort();
    runRef.current = null;
    stop();
    setPhase("idle");
    setChallenge(null);
  };

  const run = async () => {
    if (!consent || busy) return;
    runRef.current?.abort();
    const ctrl = new AbortController();
    runRef.current = ctrl;
    setError(null);
    setChallenge(null);
    setFramesTaken(0);
    setPhase("starting");
    const live = camera.status === "live" || (await start());
    if (ctrl.signal.aborted) return;
    if (!live) {
      setPhase("idle");
      return;
    }
    try {
      setPhase("countdown");
      for (let n = COUNTDOWN_FROM; n >= 1; n--) {
        setCount(n);
        await sleep(COUNTDOWN_TICK_MS, ctrl.signal);
      }
      const picked = pickChallenge(randomUnit());
      setChallenge(picked);
      setPhase("challenge");
      await sleep(500, ctrl.signal);

      const frames: string[] = [];
      let captures = 0;
      while (frames.length < SELFIE_FRAMES) {
        const video = videoRef.current;
        if (!video) throw new ImageError("The camera preview is not available. Try again.");
        if (captures >= SELFIE_FRAMES + MAX_RECAPTURES) {
          throw new ImageError(
            "The camera image is not changing between frames. Make sure you are in front of a working camera, not a still picture, and try again.",
          );
        }
        captures += 1;
        const frame = captureVideoFrame(video, SELFIE_MAX_SIDE, imageLabel(frames.length));
        // Byte-identical frames are rejected by the API: capture again.
        if (!frames.includes(frame)) {
          frames.push(frame);
          setFramesTaken(frames.length);
        }
        if (frames.length < SELFIE_FRAMES) await sleep(FRAME_INTERVAL_MS, ctrl.signal);
      }
      stop();

      const problems = checkVerificationImages({ ...ineImages, frames });
      if (problems.length > 0) {
        setError({ message: problems.join(" "), action: "selfie" });
        setPhase("error");
        return;
      }
      setPhase("submitting");
      const outcome = await onSubmit({ frames, reto: picked.code });
      if (ctrl.signal.aborted) return;
      if (!outcome.ok) {
        setError({ message: outcome.message, action: outcome.action });
        setPhase("error");
        if (outcome.action === "consent") {
          setConsent(false);
          consentRef.current?.focus();
        }
      }
    } catch (e) {
      if (isAbort(e)) return;
      stop();
      setError({
        message: e instanceof ImageError ? e.message : "The liveness check could not be completed. Try again.",
        action: "selfie",
      });
      setPhase("error");
    }
  };

  const problem = camera.status === "error" ? describeCameraProblem(camera.problem, "selfie") : null;
  const previewOn = camera.status === "live" || camera.status === "starting";

  const announcement =
    phase === "starting"
      ? "Starting camera."
      : phase === "countdown"
        ? "Get ready. Center your face in the oval."
        : phase === "challenge" && challenge
          ? `Now: ${challenge.text}.`
          : phase === "submitting"
            ? "Verifying your identity. This can take a few seconds."
            : "";

  return (
    <div className="space-y-5">
      <section aria-labelledby="biometric-notice-title" className="space-y-2 rounded-lg border border-border bg-surface-2/60 p-4 text-sm">
        <h3 id="biometric-notice-title" className="flex items-center gap-1.5 font-semibold text-ink">
          <Lock className="size-4 text-accent" aria-hidden />
          Biometric data notice
        </h3>
        <p className="text-ink-2">
          To open your account remotely we must confirm that you are the person on the INE. Your camera takes a few
          frames of your face, which are compared with the photo on your credential. Under Mexico&apos;s personal data
          protection law (LFPDPPP), biometric data is <strong>sensitive personal data</strong>, so we need your express
          consent.
        </p>
        <ul className="list-disc space-y-1 pl-4 text-xs text-muted">
          <li>
            <span className="font-medium text-ink-2">Purpose:</span> identity verification (liveness check and 1:1 face
            match) required by anti-money-laundering rules. It is not used for anything else.
          </li>
          <li>
            <span className="font-medium text-ink-2">Who processes it:</span> Synthetic Capital and its
            identity-verification provider.
          </li>
          <li>
            <span className="font-medium text-ink-2">What we keep:</span> the result, cryptographic fingerprints
            (SHA-256) of the images and the time of your consent, not the images themselves.
          </li>
          <li>
            <span className="font-medium text-ink-2">Your choice:</span> without consent remote verification is not
            possible; you can verify in person at a branch instead. You can exercise your ARCO rights (access,
            rectification, cancellation, opposition) at any time.
          </li>
        </ul>
        <label htmlFor="consentimiento" className="flex cursor-pointer items-start gap-2.5 pt-1 text-sm text-ink">
          <input
            ref={consentRef}
            id="consentimiento"
            type="checkbox"
            checked={consent}
            disabled={busy}
            onChange={(e) => {
              setConsent(e.target.checked);
              if (!e.target.checked) cancelRun();
            }}
            className="mt-0.5 size-4 shrink-0 accent-accent"
          />
          <span>
            I consent to Synthetic Capital processing my biometric data (images of my face and of my INE) to verify my
            identity, as described above.
          </span>
        </label>
      </section>

      <div
        className={cx(
          "relative mx-auto aspect-[3/4] w-full max-w-72 overflow-hidden rounded-xl bg-black",
          !previewOn && "bg-surface-2",
        )}
      >
        <video
          ref={videoRef}
          className={cx("size-full -scale-x-100 object-cover", !previewOn && "invisible")}
          playsInline
          muted
          autoPlay
          aria-label="Camera preview (mirrored)"
        />
        {camera.status === "live" ? (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center" aria-hidden>
            <div
              className={cx(
                "aspect-[3/4] w-[62%] rounded-[50%] border-2 shadow-[0_0_0_9999px_rgba(0,0,0,0.5)] transition-colors",
                phase === "challenge" ? "border-accent" : "border-white/90",
              )}
            />
          </div>
        ) : null}
        {!previewOn ? (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 px-6 text-center text-xs text-muted">
            {problem ? <VideoOff className="size-7" aria-hidden /> : <ScanFace className="size-8" aria-hidden />}
            {phase === "submitting" ? "Camera off. Verifying…" : "The camera turns on when you start the check."}
          </div>
        ) : null}
        {camera.status === "starting" ? (
          <div className="absolute inset-0 flex items-center justify-center gap-2 text-sm text-white/85" aria-hidden>
            <Loader2 className="size-4 animate-spin" /> Starting camera…
          </div>
        ) : null}
        {phase === "countdown" && camera.status === "live" ? (
          <div className="absolute inset-x-0 top-0 flex flex-col items-center gap-0.5 bg-gradient-to-b from-black/70 to-transparent px-3 pt-3 pb-6 text-white" aria-hidden>
            <span className="text-3xl font-semibold tabular-nums">{count}</span>
            <span className="text-xs">Center your face in the oval</span>
          </div>
        ) : null}
        {phase === "challenge" && challenge ? (
          <div className="absolute inset-x-0 bottom-0 space-y-1.5 bg-black/70 px-3 py-3 text-center text-white" aria-hidden>
            <p className="text-lg font-semibold">{challenge.text}</p>
            <div className="flex justify-center gap-1.5">
              {Array.from({ length: SELFIE_FRAMES }, (_, i) => (
                <span key={i} className={cx("size-2 rounded-full", i < framesTaken ? "bg-accent" : "bg-white/35")} />
              ))}
            </div>
          </div>
        ) : null}
      </div>

      <p className="sr-only" role="status" aria-live="polite">
        {announcement}
      </p>

      {phase === "submitting" ? (
        <div className="flex items-start gap-3 rounded-lg border border-info/30 bg-info-soft px-3 py-3 text-sm text-info">
          <Loader2 className="mt-0.5 size-4 shrink-0 animate-spin" aria-hidden />
          <div>
            <p className="font-semibold">Verifying your identity…</p>
            <p className="text-[13px] opacity-90">
              Uploading {SELFIE_FRAMES} frames and your INE photos, checking liveness and matching your face with your
              credential. Do not close this page.
            </p>
          </div>
        </div>
      ) : null}

      {problem ? (
        <Alert tone="error" title={problem.title}>
          {problem.message}
        </Alert>
      ) : null}

      {error ? (
        <Alert tone="error" title="Verification not submitted">
          <p>{error.message}</p>
          {error.action === "id" || error.action === "profile" ? (
            <Button
              type="button"
              variant="secondary"
              size="sm"
              className="mt-2"
              onClick={error.action === "id" ? onBack : onEditProfile}
            >
              {error.action === "id" ? "Edit ID details and photos" : "Edit personal data"}
            </Button>
          ) : null}
        </Alert>
      ) : null}

      {phase !== "submitting" ? (
        <ul className="list-disc space-y-0.5 pl-4 text-xs text-muted">
          <li>Face a light source; remove glasses, hats or masks.</li>
          <li>Hold the camera at eye level and follow the instruction that appears.</li>
        </ul>
      ) : null}

      <div className="flex gap-3">
        <Button type="button" variant="secondary" size="lg" onClick={onBack} disabled={busy}>
          <ArrowLeft className="size-4" aria-hidden />
          Back
        </Button>
        {phase === "countdown" || phase === "challenge" ? (
          <Button type="button" variant="ghost" size="lg" className="flex-1" onClick={cancelRun}>
            Cancel
          </Button>
        ) : (
          <Button
            type="button"
            size="lg"
            className="flex-1"
            onClick={() => void run()}
            disabled={!consent || busy}
            loading={phase === "starting" || phase === "submitting"}
          >
            {phase === "error" || problem ? <RotateCcw className="size-4" aria-hidden /> : <ScanFace className="size-4" aria-hidden />}
            {phase === "error" || problem ? "Start liveness check again" : "Start liveness check"}
          </Button>
        )}
      </div>
      {!consent && !busy ? (
        <p className="text-center text-xs text-muted">Check the consent box above to start.</p>
      ) : null}
    </div>
  );
}
