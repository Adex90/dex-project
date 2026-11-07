// Injected before any page script: an EIP-1193 provider backed by an UNLOCKED anvil account. It plays the part of the
// user's browser wallet (MetaMask etc.) — it signs the key-derivation message and sends transactions.
(() => {
  const RPC = "http://127.0.0.1:8545";
  const ACCOUNT = "__ACCOUNT__";
  let id = 0;
  const call = async (method, params) => {
    const r = await fetch(RPC, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }) });
    const j = await r.json();
    if (j.error) { const e = new Error(j.error.message); e.code = j.error.code; throw e; }
    return j.result;
  };
  window.ethereum = {
    isMock: true,
    async request({ method, params }) {
      switch (method) {
        case "eth_requestAccounts":
        case "eth_accounts": return [ACCOUNT];
        case "wallet_switchEthereumChain": return null;
        case "personal_sign": {
          const sig = await call(method, params);
          if (window.__recordSig) await window.__recordSig(sig); // lets the test prove the signature never leaves the page
          return sig;
        }
        default: return call(method, params);
      }
    },
    on() {}, removeListener() {},
  };
})();
