"use client";

/**
 * Camera and image helpers of the onboarding wizard (browser only).
 *
 * Every image that leaves the browser goes through a canvas and is encoded
 * again as JPEG: that drops EXIF metadata (GPS position, device, time),
 * bounds the resolution and keeps each image well under the API's 3 MB.
 */
import { useCallback, useEffect, useRef, useState } from "react";

import { MAX_IMAGE_BYTES, MIN_IMAGE_BYTES, inspectImage } from "@/lib/kyc";

import {
  ACCEPTED_IMAGE_TYPES,
  JPEG_QUALITY,
  MAX_UPLOAD_BYTES,
  fitWithin,
  type CameraProblem,
} from "./logic";

/** An image problem the customer can act on (message in English). */
export class ImageError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ImageError";
  }
}

// -----------------------------------------------------------------------------
// Encoding
// -----------------------------------------------------------------------------

/**
 * JPEG data URL of a canvas between 1 KB and 3 MB: lowers the quality if a
 * very detailed photo comes out too large, refuses blank/tiny images.
 */
export function encodeCanvasJpeg(canvas: HTMLCanvasElement, label: string): string {
  let last = "";
  for (const quality of [JPEG_QUALITY, 0.75, 0.62, 0.5]) {
    last = canvas.toDataURL("image/jpeg", quality);
    const check = inspectImage(label, last);
    if (check.ok) return last;
    if (check.code !== "SIZE") throw new ImageError(check.error);
    // Under 1 KB at the best quality: nothing to gain by lowering it.
    const bytes = Math.floor(((last.length - last.indexOf(",") - 1) * 3) / 4);
    if (bytes < MIN_IMAGE_BYTES) {
      throw new ImageError(`${label}: the image is too small or blank. Use a sharper photo that shows the whole credential.`);
    }
  }
  const bytes = Math.floor(((last.length - last.indexOf(",") - 1) * 3) / 4);
  if (bytes > MAX_IMAGE_BYTES) throw new ImageError(`${label}: the photo is too large even after compressing it.`);
  throw new ImageError(`${label}: the image could not be processed.`);
}

function drawToCanvas(source: CanvasImageSource, width: number, height: number, maxSide: number): HTMLCanvasElement {
  const size = fitWithin(width, height, maxSide);
  const canvas = document.createElement("canvas");
  canvas.width = size.width;
  canvas.height = size.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new ImageError("Your browser could not process the image.");
  // Transparent PNG areas would turn black in a JPEG.
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, size.width, size.height);
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(source, 0, 0, size.width, size.height);
  return canvas;
}

/** Current video frame as a JPEG data URL (longest side <= `maxSide`). */
export function captureVideoFrame(video: HTMLVideoElement, maxSide: number, label: string): string {
  if (!video.videoWidth || !video.videoHeight) throw new ImageError("The camera is not sending images yet. Wait a moment and try again.");
  return encodeCanvasJpeg(drawToCanvas(video, video.videoWidth, video.videoHeight, maxSide), label);
}

function loadImageElement(file: Blob): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve(img);
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new ImageError("We could not read that file as an image."));
    };
    img.src = url;
  });
}

/**
 * An uploaded photo re-encoded as JPEG without metadata (EXIF orientation is
 * applied first, so a rotated phone photo stays upright).
 */
export async function reencodeImageFile(file: File, maxSide: number, label: string): Promise<string> {
  if (!(ACCEPTED_IMAGE_TYPES as readonly string[]).includes(file.type)) {
    throw new ImageError(`${label}: only JPEG, PNG or WebP photos are accepted.`);
  }
  if (file.size < MIN_IMAGE_BYTES) throw new ImageError(`${label}: the file is too small to be a photo of your credential.`);
  if (file.size > MAX_UPLOAD_BYTES) throw new ImageError(`${label}: the file is too large (40 MB maximum).`);
  if (typeof createImageBitmap === "function") {
    try {
      const bitmap = await createImageBitmap(file);
      try {
        return encodeCanvasJpeg(drawToCanvas(bitmap, bitmap.width, bitmap.height, maxSide), label);
      } finally {
        bitmap.close();
      }
    } catch (error) {
      if (error instanceof ImageError) throw error;
      // Fall back to an <img> element (older Safari).
    }
  }
  const img = await loadImageElement(file);
  return encodeCanvasJpeg(drawToCanvas(img, img.naturalWidth, img.naturalHeight, maxSide), label);
}

// -----------------------------------------------------------------------------
// Camera
// -----------------------------------------------------------------------------

/** Whether getUserMedia can be called here at all. */
export function cameraSupport(): "ok" | "insecure" | "unsupported" {
  if (typeof window === "undefined") return "unsupported";
  if (!window.isSecureContext) return "insecure";
  if (!navigator.mediaDevices?.getUserMedia) return "unsupported";
  return "ok";
}

export type CameraState =
  | { status: "idle" }
  | { status: "starting" }
  | { status: "live" }
  | { status: "error"; problem: CameraProblem };

function waitForFrames(video: HTMLVideoElement, timeoutMs: number): Promise<boolean> {
  if (video.videoWidth > 0 && video.readyState >= 2) return Promise.resolve(true);
  return new Promise((resolve) => {
    const done = (ok: boolean) => {
      clearTimeout(timer);
      video.removeEventListener("loadeddata", onReady);
      video.removeEventListener("playing", onReady);
      resolve(ok);
    };
    const onReady = () => {
      if (video.videoWidth > 0) done(true);
    };
    const timer = setTimeout(() => done(video.videoWidth > 0), timeoutMs);
    video.addEventListener("loadeddata", onReady);
    video.addEventListener("playing", onReady);
  });
}

/**
 * A camera stream bound to a <video>. `start` asks for the camera (the
 * browser prompts the first time); the tracks are stopped by `stop`, by a
 * new `start` and when the component unmounts, so the camera light never
 * stays on after leaving the step.
 */
export function useCamera(facingMode: "user" | "environment") {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  // Bumped by every start/stop: a getUserMedia that resolves late is discarded.
  const generation = useRef(0);
  const [state, setState] = useState<CameraState>({ status: "idle" });

  const release = useCallback(() => {
    generation.current += 1;
    for (const track of streamRef.current?.getTracks() ?? []) track.stop();
    streamRef.current = null;
    if (videoRef.current) videoRef.current.srcObject = null;
  }, []);

  const stop = useCallback(() => {
    release();
    setState({ status: "idle" });
  }, [release]);

  const start = useCallback(async (): Promise<boolean> => {
    release();
    const mine = generation.current;
    const support = cameraSupport();
    if (support !== "ok") {
      setState({ status: "error", problem: support });
      return false;
    }
    setState({ status: "starting" });
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: false,
        video:
          facingMode === "environment"
            ? { facingMode, width: { ideal: 1920 }, height: { ideal: 1080 } }
            : { facingMode, width: { ideal: 1280 }, height: { ideal: 720 } },
      });
    } catch (error) {
      if (mine !== generation.current) return false;
      const name = error instanceof DOMException || error instanceof Error ? error.name : "Error";
      setState({ status: "error", problem: name });
      return false;
    }
    if (mine !== generation.current) {
      for (const track of stream.getTracks()) track.stop();
      return false;
    }
    streamRef.current = stream;
    for (const track of stream.getVideoTracks()) {
      track.addEventListener("ended", () => {
        if (streamRef.current === stream) {
          release();
          setState({ status: "error", problem: "ended" });
        }
      });
    }
    const video = videoRef.current;
    if (!video) {
      release();
      setState({ status: "error", problem: "no-frame" });
      return false;
    }
    video.srcObject = stream;
    await video.play().catch(() => undefined);
    if (mine !== generation.current) return false;
    const ready = await waitForFrames(video, 6000);
    if (mine !== generation.current) return false;
    if (!ready) {
      release();
      setState({ status: "error", problem: "no-frame" });
      return false;
    }
    setState({ status: "live" });
    return true;
  }, [facingMode, release]);

  useEffect(() => release, [release]);

  return { videoRef, state, start, stop };
}

/** Resolves after `ms`, or rejects with an AbortError when `signal` aborts. */
export function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(new DOMException("Aborted", "AbortError"));
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}
