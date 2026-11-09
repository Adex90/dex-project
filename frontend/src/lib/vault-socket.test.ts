/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { useVaultStore } from "../store/vault.ts";
import {
  CLOSE_SESSION_ENDED,
  VAULT_PROTOCOL,
  VaultSocket,
  reconnectDelay,
  vaultSocketUrl,
  type SocketLike,
  type VaultSocketOptions,
} from "./vault-socket.ts";

const vault = () => useVaultStore.getState();

/* A socket the test plays the server for. */
class FakeSocket implements SocketLike {
  static created: FakeSocket[] = [];
  readyState = 0;
  url: string;
  protocols: string[];
  sent: string[] = [];
  closedWith: { code?: number; reason?: string } | null = null;
  onopen: SocketLike["onopen"] = null;
  onmessage: SocketLike["onmessage"] = null;
  onclose: SocketLike["onclose"] = null;
  onerror: SocketLike["onerror"] = null;

  constructor(url: string, protocols: string[]) {
    this.url = url;
    this.protocols = protocols;
    FakeSocket.created.push(this);
  }
  serverOpens() {
    this.readyState = 1;
    this.onopen?.({});
  }
  serverSends(message: object) {
    this.onmessage?.({ data: JSON.stringify(message) });
  }
  serverCloses(code: number, reason = "") {
    this.readyState = 3;
    this.onclose?.({ code, reason });
  }
  send(data: string) {
    this.sent.push(data);
  }
  close(code?: number, reason?: string) {
    this.closedWith = { code, reason };
    this.readyState = 3;
  }
}

/* Timers the test advances by hand. */
function manualClock() {
  const timers: { run: () => void; ms: number; cancelled: boolean; done: boolean }[] = [];
  return {
    schedule(run: () => void, ms: number) {
      const timer = { run, ms, cancelled: false, done: false };
      timers.push(timer);
      return () => {
        timer.cancelled = true;
      };
    },
    pending() {
      return timers.filter((t) => !t.cancelled && !t.done);
    },
    /** Fires the next pending timer and returns its delay. */
    fire(): number {
      const timer = this.pending()[0];
      assert.ok(timer, "a reconnection is scheduled");
      timer.done = true;
      timer.run();
      return timer.ms;
    },
  };
}

interface Harness {
  channel: VaultSocket;
  clock: ReturnType<typeof manualClock>;
  advances: number;
  sessionEnded: number;
  expired: boolean;
  current: () => FakeSocket;
}

function harness(overrides: Partial<VaultSocketOptions> = {}): Harness {
  const clock = manualClock();
  const h: Harness = {
    clock,
    advances: 0,
    sessionEnded: 0,
    expired: false,
    current: () => FakeSocket.created.at(-1)!,
    channel: null as unknown as VaultSocket,
  };
  h.channel = new VaultSocket({
    url: "ws://api.test/api/v1/vault/ws",
    token: "header.payload.signature",
    receive: (data) => vault().receive(data),
    currentSeq: () => vault().lastSeq,
    setStatus: (status) => vault().setRealtimeStatus(status),
    onSessionEnded: () => (h.sessionEnded += 1),
    onAdvance: () => (h.advances += 1),
    isTokenExpired: () => h.expired,
    createSocket: (url, protocols) => new FakeSocket(url, protocols),
    schedule: clock.schedule.bind(clock),
    random: () => 1,
    ...overrides,
  });
  return h;
}

beforeEach(() => {
  vault().reset();
  FakeSocket.created = [];
});

/* The backend's frames, field for field (backend/src/boveda/modelos.rs). */

function balance(total: string, cash = total, invested = "0.00000000") {
  return { total_mxn: total, available_cash: cash, invested_assets: invested, unrealized_pnl_percentage: "0.00" };
}

function snapshotFrame(seq: number, total = "0.00000000", status = "pending") {
  return {
    type: "vault.snapshot",
    seq,
    vault: {
      user_id: "7f3c9a52-1d4e-4b8a-9c6f-2e5d8a1b3c4d",
      clabe: "646180157000000017",
      bank_name: "STP",
      balance: balance(total),
      reserve_proof: { status, zk_root_hash: status === "pending" ? "0".repeat(64) : "d8489e".padEnd(64, "a") },
      holdings: [],
    },
  };
}

function speiFrame(seq: number, amount: string, total: string) {
  return {
    type: "spei.transfer",
    seq,
    transfer: {
      tracking_key: `PRUEBA${seq}`.padEnd(26, "0"),
      direction: "inbound",
      status: "settled",
      amount,
      counterparty_name: "ORDENANTE DE PRUEBA",
      counterparty_bank: "012",
      occurred_at: "2026-11-09T04:44:08.123456Z",
    },
    balance: balance(total),
  };
}

/* URL and backoff */

test("vaultSocketUrl: same host as the API, ws for http and wss for https", () => {
  assert.equal(vaultSocketUrl("http://localhost:8080"), "ws://localhost:8080/api/v1/vault/ws");
  assert.equal(vaultSocketUrl("https://bank.example.mx/"), "wss://bank.example.mx/api/v1/vault/ws");
  assert.equal(vaultSocketUrl("https://bank.example.mx/backend"), "wss://bank.example.mx/backend/api/v1/vault/ws");
});

test("reconnectDelay: doubles from 1 s to a 30 s ceiling, jittered down to half", () => {
  assert.deepEqual(
    [0, 1, 2, 3, 4, 5, 6, 9].map((n) => reconnectDelay(n, () => 1)),
    [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000],
  );
  assert.equal(reconnectDelay(0, () => 0), 500);
  assert.equal(reconnectDelay(3, () => 0.5), 6_000);
});

/* The live stream, end to end through the real store */

test("connects with the JWT as a subprotocol and goes live", () => {
  const h = harness();
  h.channel.start();
  assert.equal(vault().realtime, "connecting");
  assert.deepEqual(h.current().protocols, [VAULT_PROTOCOL, "bearer.header.payload.signature"]);
  h.current().serverOpens();
  assert.equal(vault().realtime, "live");
});

test("a SPEI deposit updates total_mxn at once; a reserve proof and a holding follow", () => {
  const h = harness();
  h.channel.start();
  h.current().serverOpens();
  // A new account: version 0, nothing in it yet.
  h.current().serverSends(snapshotFrame(0));
  assert.equal(vault().lastSeq, 0);
  assert.equal(vault().balance?.total_mxn, "0.00000000");

  h.current().serverSends(speiFrame(1, "2500.00000000", "2500.00000000"));
  assert.equal(vault().balance?.total_mxn, "2500.00000000");
  assert.equal(vault().balance?.available_cash, "2500.00000000");
  assert.equal(vault().recentTransfers[0]?.amount, "2500.00000000");
  assert.equal(vault().lastSeq, 1);

  // A published reserve cut: the same version, a new proof.
  h.current().serverSends(snapshotFrame(1, "2500.00000000", "verified"));
  assert.equal(vault().reserveProof?.status, "verified");
  assert.equal(vault().balance?.total_mxn, "2500.00000000");

  const holding = {
    ticker: "AUG1",
    name: "Tokenized gold, 1 g",
    shares: "1.00000000",
    current_price: "1612.40000000",
    total_value: "1612.40000000",
    daily_change_pct: "0.84",
  };
  h.current().serverSends({ type: "holding.updated", seq: 2, holding, balance: balance("2500.00000000", "887.60000000", "1612.40000000") });
  assert.deepEqual(vault().holdings, [holding]);
  assert.equal(vault().balance?.invested_assets, "1612.40000000");
  assert.equal(h.current().sent.length, 0, "an orderly stream asks for nothing");
});

test("a gap asks the server for a resync, and the resync snapshot clears stale", () => {
  const h = harness();
  h.channel.start();
  h.current().serverOpens();
  h.current().serverSends(snapshotFrame(5, "100.00000000"));
  h.current().serverSends(speiFrame(7, "50.00000000", "150.00000000")); // 6 was merged away
  assert.equal(vault().stale, true);
  assert.deepEqual(h.current().sent, [JSON.stringify({ type: "resync" })]);
  assert.equal(vault().balance?.total_mxn, "150.00000000", "the newer balance applies meanwhile");
  h.current().serverSends(snapshotFrame(7, "150.00000000"));
  assert.equal(vault().stale, false);
});

test("a delta before any snapshot asks for one; an invalid frame is ignored", () => {
  const h = harness();
  h.channel.start();
  h.current().serverOpens();
  h.current().serverSends(speiFrame(3, "1.00000000", "1.00000000"));
  assert.deepEqual(h.current().sent, [JSON.stringify({ type: "resync" })]);
  h.current().onmessage?.({ data: "{not json" });
  assert.equal(h.current().sent.length, 1);
  assert.equal(vault().balance, null);
});

test("onAdvance: when the version passes what the channel has seen, not for the first or a same-version snapshot", () => {
  const h = harness();
  h.channel.start();
  h.current().serverOpens();
  h.current().serverSends(snapshotFrame(3, "10.00000000"));
  assert.equal(h.advances, 0, "the first snapshot is the baseline");
  h.current().serverSends(snapshotFrame(3, "10.00000000", "verified"));
  assert.equal(h.advances, 0, "a new reserve proof moves no money");
  h.current().serverSends(speiFrame(4, "5.00000000", "15.00000000"));
  assert.equal(h.advances, 1);
  h.current().serverSends(speiFrame(4, "5.00000000", "15.00000000"));
  assert.equal(h.advances, 1, "a replay is not news");
});

/* Reconnection */

test("a deploy (1001) reconnects within a second, and the new snapshot catches up", () => {
  const h = harness({ random: () => 0.5 });
  h.channel.start();
  h.current().serverOpens();
  h.current().serverSends(snapshotFrame(3, "10.00000000"));
  const first = h.current();

  first.serverCloses(1001, "el servidor se reinicia");
  assert.equal(vault().realtime, "reconnecting");
  assert.equal(vault().lastSeq, null, "the stream baseline is gone: REST may fill in meanwhile");
  assert.equal(vault().balance?.total_mxn, "10.00000000", "the last figures stay on screen");
  assert.equal(h.clock.fire(), 750);

  const second = h.current();
  assert.notEqual(second, first);
  second.serverOpens();
  // A deposit arrived while the task restarted.
  second.serverSends(snapshotFrame(4, "60.00000000"));
  assert.equal(vault().realtime, "live");
  assert.equal(vault().balance?.total_mxn, "60.00000000");
  assert.equal(h.advances, 1, "what changed offline reloads the rest of the dashboard");
});

test("failed attempts back off up to 30 s; a working stream starts the backoff over", () => {
  const h = harness();
  h.channel.start();
  const delays: number[] = [];
  for (let i = 0; i < 7; i++) {
    h.current().serverCloses(1006); // refused or unreachable before opening
    delays.push(h.clock.fire());
  }
  assert.deepEqual(delays, [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]);

  h.current().serverOpens();
  h.current().serverSends(snapshotFrame(1, "1.00000000"));
  h.current().serverCloses(1006);
  assert.equal(h.clock.fire(), 1_000);
});

test("4001 ends the session: no reconnection, the app signs in again", () => {
  const h = harness();
  h.channel.start();
  h.current().serverOpens();
  h.current().serverSends(snapshotFrame(1, "1.00000000"));
  h.current().serverCloses(CLOSE_SESSION_ENDED, "sesión revocada");
  assert.equal(h.sessionEnded, 1);
  assert.equal(vault().realtime, "offline");
  assert.equal(h.clock.pending().length, 0);
  assert.equal(FakeSocket.created.length, 1);
});

test("a token that expired while offline is not retried", () => {
  const h = harness();
  h.channel.start();
  h.current().serverOpens();
  h.current().serverCloses(1006);
  h.expired = true;
  h.clock.fire();
  assert.equal(h.sessionEnded, 1);
  assert.equal(FakeSocket.created.length, 1, "no handshake with a dead token");
});

test("stop closes on purpose (1000): nothing reconnects, a late event changes nothing", () => {
  const h = harness();
  h.channel.start();
  const socket = h.current();
  socket.serverOpens();
  h.channel.stop();
  assert.deepEqual(socket.closedWith, { code: 1000, reason: "client leaving" });
  assert.equal(vault().realtime, "offline");
  socket.serverCloses(1006);
  assert.equal(h.clock.pending().length, 0);
  assert.equal(FakeSocket.created.length, 1);
});

test("wake reconnects at once when a retry is waiting, and only then", () => {
  const h = harness();
  h.channel.start();
  h.channel.wake();
  assert.equal(FakeSocket.created.length, 1, "connecting already: nothing to wake");
  h.current().serverCloses(1006);
  assert.equal(h.clock.pending().length, 1);
  h.channel.wake(); // the network is back
  assert.equal(FakeSocket.created.length, 2);
  assert.equal(h.clock.pending().length, 0, "the waiting retry is cancelled");
});

test("a socket the browser refuses to create is retried later", () => {
  let calls = 0;
  const h = harness({
    createSocket: (url, protocols) => {
      calls += 1;
      if (calls === 1) throw new SyntaxError("refused");
      return new FakeSocket(url, protocols);
    },
  });
  h.channel.start();
  assert.equal(FakeSocket.created.length, 0);
  h.clock.fire();
  assert.equal(FakeSocket.created.length, 1);
});
