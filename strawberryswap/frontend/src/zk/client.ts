import type { Api, ApiKey, ProgressMessage, WorkerRequest, WorkerResponse } from "./protocol";

export type ProverStatus =
  | { phase: "idle" }
  | { phase: "loading"; loaded: number; total: number; label: string }
  | { phase: "ready"; devKeys: boolean }
  | { phase: "error"; message: string };

type Pending = { resolve: (v: unknown) => void; reject: (e: Error) => void };

/**
 * Main-thread handle to the prover worker. Strictly typed against `Api`; exposes an external-store interface so React
 * can subscribe with `useSyncExternalStore` (see hooks/useProver). The worker is created lazily, in the browser only.
 */
export class ProverClient {
  private worker: Worker | undefined;
  private seq = 0;
  private readonly pending = new Map<number, Pending>();
  private readonly listeners = new Set<() => void>();
  private status: ProverStatus = { phase: "idle" };
  /** Last progress message of a running proof (the UI shows it next to an elapsed-time counter). */
  private lastProgress: ProgressMessage | undefined;

  getStatus = (): ProverStatus => this.status;
  getProgress = (): ProgressMessage | undefined => this.lastProgress;
  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  };

  private emit(): void {
    for (const l of this.listeners) l();
  }

  private ensureWorker(): Worker {
    if (this.worker) return this.worker;
    // `new URL(..., import.meta.url)` is what lets Next's bundler emit the worker as its own static chunk.
    const w = new Worker(new URL("./prover.worker.ts", import.meta.url), { type: "module" });
    w.onmessage = (ev: MessageEvent<WorkerResponse>) => {
      const m = ev.data;
      if (m.type === "progress") {
        this.lastProgress = m;
        if (this.status.phase === "loading") this.status = { phase: "loading", loaded: m.loaded, total: m.total, label: m.label };
        this.emit();
        return;
      }
      const p = this.pending.get(m.id);
      if (!p) return;
      this.pending.delete(m.id);
      if (m.ok) p.resolve(m.result);
      else p.reject(new Error(m.error));
    };
    w.onerror = (ev) => {
      const err = new Error(ev.message || "prover worker crashed");
      for (const p of this.pending.values()) p.reject(err);
      this.pending.clear();
      this.status = { phase: "error", message: err.message };
      this.worker = undefined;
      this.emit();
    };
    this.worker = w;
    return w;
  }

  call<K extends ApiKey>(type: K, payload: Api[K]["req"]): Promise<Api[K]["res"]> {
    const w = this.ensureWorker();
    const id = ++this.seq;
    return new Promise<Api[K]["res"]>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      const req = { id, type, payload } as WorkerRequest;
      w.postMessage(req);
    });
  }

  /** Idempotent. Resolves when the artifacts are downloaded, verified and the prover is built. */
  async init(): Promise<void> {
    if (this.status.phase === "ready") return;
    this.status = { phase: "loading", loaded: 0, total: 1, label: "starting" };
    this.emit();
    try {
      const r = await this.call("init", {});
      this.status = { phase: "ready", devKeys: r.devKeys };
    } catch (e) {
      this.status = { phase: "error", message: e instanceof Error ? e.message : "init failed" };
      throw e;
    } finally {
      this.emit();
    }
  }

  terminate(): void {
    this.worker?.terminate();
    this.worker = undefined;
    this.pending.clear();
    this.status = { phase: "idle" };
    this.emit();
  }
}

let singleton: ProverClient | undefined;
/** One worker (and one wallet) per tab. */
export function getProverClient(): ProverClient {
  singleton ??= new ProverClient();
  return singleton;
}
