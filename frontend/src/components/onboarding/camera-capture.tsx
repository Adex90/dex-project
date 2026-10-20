"use client";

import { useEffect, useRef, useState } from "react";
import { Camera, Loader2, RotateCcw, X } from "lucide-react";

import { Alert, Button } from "@/components/ui/primitives";

import { describeCameraProblem, ID_MAX_SIDE } from "./logic";
import { ImageError, captureVideoFrame, useCamera } from "./media";

/**
 * Rear camera with a live preview and a card-shaped guide, to photograph one
 * side of the credential. The camera starts on mount and stops on unmount
 * (and when the page is hidden: "Try the camera again" restarts it).
 * Keyboard focus moves into it on mount (Cancel) and to Capture once live.
 */
export function CameraCapture({
  label,
  guide,
  onCapture,
  onCancel,
}: {
  /** "INE front" / "INE back" (used in messages and the capture button). */
  label: string;
  guide: string;
  onCapture: (dataUrl: string) => void;
  onCancel: () => void;
}) {
  const { videoRef, state, start, stop } = useCamera("environment");
  const [error, setError] = useState<string | null>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const captureRef = useRef<HTMLButtonElement>(null);
  const live = state.status === "live";

  useEffect(() => {
    cancelRef.current?.focus();
    void start();
  }, [start]);

  useEffect(() => {
    if (live) captureRef.current?.focus();
  }, [live]);

  const capture = () => {
    const video = videoRef.current;
    if (!video) return;
    try {
      const dataUrl = captureVideoFrame(video, ID_MAX_SIDE, label);
      stop();
      onCapture(dataUrl);
    } catch (e) {
      setError(e instanceof ImageError ? e.message : "The photo could not be taken. Try again.");
    }
  };

  const problem = state.status === "error" ? describeCameraProblem(state.problem, "id") : null;

  return (
    <div className="space-y-3">
      <div className="relative aspect-[4/3] overflow-hidden rounded-lg bg-black">
        <video
          ref={videoRef}
          className="size-full object-cover"
          playsInline
          muted
          autoPlay
          aria-label={`Camera preview for the ${label}`}
        />
        {state.status === "live" ? (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center" aria-hidden>
            <div className="aspect-[1.586] w-[86%] rounded-xl border-2 border-white/90 shadow-[0_0_0_9999px_rgba(0,0,0,0.45)]" />
          </div>
        ) : null}
        {state.status === "starting" || state.status === "idle" ? (
          <div className="absolute inset-0 flex items-center justify-center gap-2 text-sm text-white/80" role="status">
            <Loader2 className="size-4 animate-spin" aria-hidden /> Starting camera…
          </div>
        ) : null}
        {state.status === "live" ? (
          <p className="absolute inset-x-0 bottom-0 bg-black/55 px-3 py-1.5 text-center text-xs text-white">{guide}</p>
        ) : null}
      </div>

      {problem ? (
        <Alert tone="error" title={problem.title}>
          {problem.message}
        </Alert>
      ) : null}
      {error ? <Alert tone="error">{error}</Alert> : null}

      <div className="flex flex-wrap gap-2">
        {problem ? (
          <Button type="button" variant="secondary" size="sm" onClick={() => void start()}>
            <RotateCcw className="size-3.5" aria-hidden />
            Try the camera again
          </Button>
        ) : (
          <Button ref={captureRef} type="button" size="sm" onClick={capture} disabled={!live}>
            <Camera className="size-3.5" aria-hidden />
            Capture
          </Button>
        )}
        <Button
          ref={cancelRef}
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => {
            stop();
            onCancel();
          }}
        >
          <X className="size-3.5" aria-hidden />
          Cancel
        </Button>
      </div>
    </div>
  );
}
