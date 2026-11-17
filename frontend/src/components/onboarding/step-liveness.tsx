"use client";

import { useEffect, useRef, useState } from "react";
import { CheckCircle2, Loader2, Lock, RotateCcw, ScanFace, VideoOff } from "lucide-react";

import { Alert, Button, cx } from "@/components/ui/primitives";
import { COMPLIANCE_EMAIL } from "@/lib/config";
import { checkVerificationImages, imageLabel } from "@/lib/kyc";

import {
  FRAME_INTERVAL_MS,
  SELFIE_FRAMES,
  SELFIE_MAX_SIDE,
  describeCameraProblem,
  pickChallenge,
  type Challenge,
} from "./logic";
import { ImageError, captureVideoFrame, sleep, useCamera } from "./media";

/** What the liveness check hands to the page: the frames and the challenge shown. */
export interface LivenessCapture {
  frames: string[];
  reto: string;
}

type Phase = "idle" | "starting" | "countdown" | "challenge" | "error";

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

/** The camera was released mid-run (page hidden, track ended): useCamera already reports why. */
class CameraStopped extends Error {}

/**
 * Step 3, first half: explicit biometric consent, then a live camera check. A
 * random challenge is shown while 4 distinct frames are captured. The frames
 * stay in this tab's memory until the review step submits them with
 * everything else; the camera turns off as soon as they are taken. There is
 * deliberately no file upload here: liveness must come from a live camera.
 */
export function StepLiveness({
  ineImages,
  captured,
  initialConsent,
  onConsentChange,
  onCaptured,
  onRetake,
}: {
  ineImages: { frente: string; reverso: string };
  /** The frames of this session, if the check was already done. */
  captured: LivenessCapture | null;
  /** Consent given earlier in this session (kept when the step is revisited). */
  initialConsent: boolean;
  onConsentChange: (consent: boolean) => void;
  onCaptured: (capture: LivenessCapture) => void;
  /** Discards the captured frames to take them again. */
  onRetake: () => void;
}) {
  const { videoRef, state: camera, start, stop } = useCamera("user");
  const [consent, setConsent] = useState(initialConsent);
  const [phase, setPhase] = useState<Phase>("idle");
  const [count, setCount] = useState(COUNTDOWN_FROM);
  const [challenge, setChallenge] = useState<Challenge | null>(null);
  const [framesTaken, setFramesTaken] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const runRef = useRef<AbortController | null>(null);
  const doneRef = useRef<HTMLDivElement>(null);

  useEffect(() => () => runRef.current?.abort(), []);

  const busy = phase === "starting" || phase === "countdown" || phase === "challenge";

  const changeConsent = (next: boolean) => {
    setConsent(next);
    onConsentChange(next);
    if (!next) {
      cancelRun();
      // Without consent no biometric data may stay, not even in memory.
      if (captured) onRetake();
    }
  };

  const cancelRun = () => {
    runRef.current?.abort();
    runRef.current = null;
    stop();
    setPhase("idle");
    setChallenge(null);
  };

  /** Throws when useCamera released the stream (page hidden, camera unplugged or revoked). */
  const assertCameraOn = () => {
    if (!videoRef.current?.srcObject) throw new CameraStopped();
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
        assertCameraOn();
      }
      const picked = pickChallenge(randomUnit());
      setChallenge(picked);
      setPhase("challenge");
      await sleep(500, ctrl.signal);

      const frames: string[] = [];
      let captures = 0;
      while (frames.length < SELFIE_FRAMES) {
        assertCameraOn();
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
        setError(problems.join(" "));
        setPhase("error");
        return;
      }
      setPhase("idle");
      setChallenge(null);
      onCaptured({ frames, reto: picked.code });
      requestAnimationFrame(() => doneRef.current?.focus());
    } catch (e) {
      if (isAbort(e)) return;
      if (e instanceof CameraStopped) {
        // Nothing was kept; the camera's own explanation ("turned off", "stopped") stays visible.
        setPhase("idle");
        setChallenge(null);
        return;
      }
      stop();
      setError(e instanceof ImageError ? e.message : "The liveness check could not be completed. Try again.");
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
          : "";

  return (
    <section aria-labelledby="liveness-title" className="space-y-5" data-testid="liveness">
      <h3 id="liveness-title" className="text-sm font-semibold text-ink">
        Liveness check
      </h3>
      <div
        aria-labelledby="biometric-notice-title"
        className="space-y-2 border border-border bg-surface-2/60 p-4 text-sm"
      >
        <h4 id="biometric-notice-title" className="flex items-center gap-1.5 font-semibold text-ink">
          <Lock className="size-4 text-synthetic-yellow" aria-hidden />
          Biometric data notice
        </h4>
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
            <span className="font-medium text-ink-2">What we keep:</span> not the images themselves, but a permanent
            audit record of each attempt: the result and the provider&apos;s signed report, the face-match score (not
            shown to you), cryptographic fingerprints (SHA-256) of the images, the CURP and voter key read from your
            INE, the IP address and browser of the submission, and the time of your consent.
          </li>
          <li>
            <span className="font-medium text-ink-2">How long:</span> anti-money-laundering rules require us to keep
            identification records for at least 10 years after the relationship ends, so a cancellation request cannot
            erase this audit record before then.
          </li>
          <li>
            <span className="font-medium text-ink-2">Your choice:</span> remote verification requires this consent; if
            you do not give it, contact us to ask about alternatives. To get our full privacy notice (aviso de
            privacidad integral), exercise your ARCO rights (access, rectification, cancellation, opposition) or
            withdraw this consent, write to{" "}
            <a
              href={`mailto:${COMPLIANCE_EMAIL}`}
              className="font-medium text-synthetic-yellow underline underline-offset-2"
            >
              {COMPLIANCE_EMAIL}
            </a>
            .
          </li>
        </ul>
        <label htmlFor="consentimiento" className="flex cursor-pointer items-start gap-2.5 pt-1 text-sm text-ink">
          <input
            id="consentimiento"
            type="checkbox"
            checked={consent}
            onChange={(e) => changeConsent(e.target.checked)}
            className="mt-0.5 size-4 shrink-0 accent-synthetic-yellow"
          />
          <span>
            I consent to Synthetic Capital processing my biometric data (images of my face and of my INE) to verify my
            identity, as described above.
          </span>
        </label>
      </div>

      {captured ? (
        <div
          ref={doneRef}
          tabIndex={-1}
          className="flex flex-wrap items-center justify-between gap-3 border-2 border-synthetic-yellow bg-synthetic-yellow/[0.05] px-4 py-4 focus:outline-none"
          data-testid="liveness-captured"
        >
          <p className="flex items-start gap-2.5 text-sm text-ink">
            <CheckCircle2 className="mt-0.5 size-5 shrink-0 text-synthetic-yellow" aria-hidden />
            <span>
              <span className="block font-semibold">Liveness check complete</span>
              <span className="block text-xs text-muted">
                {captured.frames.length} frames captured. The camera is off; they are sent only when you submit.
              </span>
            </span>
          </p>
          <Button type="button" variant="secondary" size="sm" onClick={onRetake}>
            <RotateCcw className="size-3.5" aria-hidden />
            Retake
          </Button>
        </div>
      ) : (
        <>
          <div
            className={cx(
              "relative mx-auto aspect-[3/4] w-full max-w-72 overflow-hidden border border-border bg-black",
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
                    phase === "challenge" ? "border-synthetic-yellow" : "border-white/90",
                  )}
                />
              </div>
            ) : null}
            {!previewOn ? (
              <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 px-6 text-center text-xs text-muted">
                {problem ? <VideoOff className="size-7" aria-hidden /> : <ScanFace className="size-8" aria-hidden />}
                The camera turns on when you start the check.
              </div>
            ) : null}
            {camera.status === "starting" ? (
              <div
                className="absolute inset-0 flex items-center justify-center gap-2 text-sm text-white/85"
                aria-hidden
              >
                <Loader2 className="size-4 animate-spin" /> Starting camera…
              </div>
            ) : null}
            {phase === "countdown" && camera.status === "live" ? (
              <div
                className="absolute inset-x-0 top-0 flex flex-col items-center gap-0.5 bg-gradient-to-b from-black/70 to-transparent px-3 pt-3 pb-6 text-white"
                aria-hidden
              >
                <span className="text-3xl font-semibold tabular-nums">{count}</span>
                <span className="text-xs">Center your face in the oval</span>
              </div>
            ) : null}
            {phase === "challenge" && challenge ? (
              <div
                className="absolute inset-x-0 bottom-0 space-y-1.5 bg-black/70 px-3 py-3 text-center text-white"
                aria-hidden
              >
                <p className="text-lg font-semibold">{challenge.text}</p>
                <div className="flex justify-center gap-1.5">
                  {Array.from({ length: SELFIE_FRAMES }, (_, i) => (
                    <span key={i} className={cx("size-2", i < framesTaken ? "bg-synthetic-yellow" : "bg-white/35")} />
                  ))}
                </div>
              </div>
            ) : null}
          </div>

          <p className="sr-only" role="status" aria-live="polite">
            {announcement}
          </p>

          {problem ? (
            <Alert tone="error" title={problem.title}>
              {problem.message}
            </Alert>
          ) : null}
          {error ? (
            <Alert tone="error" title="Liveness check not completed">
              {error}
            </Alert>
          ) : null}

          <ul className="list-disc space-y-0.5 pl-4 text-xs text-muted">
            <li>Face a light source; remove glasses, hats or masks.</li>
            <li>Hold the camera at eye level and follow the instruction that appears.</li>
          </ul>

          {busy ? (
            // Also while the camera starts: a permission prompt left unanswered must not trap the customer.
            <Button type="button" variant="secondary" size="lg" className="w-full" onClick={cancelRun}>
              {phase === "starting" ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              Cancel
            </Button>
          ) : (
            <Button type="button" size="lg" className="w-full" onClick={() => void run()} disabled={!consent}>
              {phase === "error" || problem ? (
                <RotateCcw className="size-4" aria-hidden />
              ) : (
                <ScanFace className="size-4" aria-hidden />
              )}
              {phase === "error" || problem ? "Start liveness check again" : "Start liveness check"}
            </Button>
          )}
          {!consent ? <p className="text-center text-xs text-muted">Check the consent box above to start.</p> : null}
        </>
      )}
    </section>
  );
}
