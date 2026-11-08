// LOCAL E2E ONLY: stands in for the MPC/TSS signing gateway the Automator talks to in production.
// POST / {chainId, to, data, gasLimit} -> {txHash}. It forwards to an UNLOCKED anvil account, so this process holds no key.
import { createServer } from "node:http";

const RPC = process.env.RPC_URL ?? "http://127.0.0.1:8545";
const FROM = process.env.SIGNER_ACCOUNT ?? "0xa0Ee7A142d267C1f36714E4a8F75612F20a79720"; // anvil account #9
const PORT = Number(process.env.SIGNER_PORT ?? 8788);

createServer(async (req, res) => {
  if (req.method !== "POST") return res.writeHead(405).end();
  let body = "";
  for await (const c of req) body += c;
  try {
    const { to, data, gasLimit } = JSON.parse(body);
    const r = await fetch(RPC, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "eth_sendTransaction", params: [{ from: FROM, to, data, gas: "0x" + BigInt(gasLimit || 5_000_000).toString(16) }] }),
    });
    const j = await r.json();
    if (j.error) return res.writeHead(502).end(JSON.stringify({ error: j.error.message }));
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ txHash: j.result }));
  } catch (e) {
    res.writeHead(400).end(JSON.stringify({ error: String(e.message) }));
  }
}).listen(PORT, "127.0.0.1", () => console.log(`dev-signer on :${PORT}`));
