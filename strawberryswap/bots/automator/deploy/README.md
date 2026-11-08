# Deploying the Automator with network-metadata privacy

## What the architecture guarantees
| # | guarantee | enforced by |
|---|---|---|
| G1 | The node has **no clearnet listener**; the intent API exists only as a Tor v3 onion service over a Unix socket | `privacy.Listen` refuses non-loopback addresses; `torrc`: `HiddenServicePort 80 unix:…`; no TCP port in `ss -ltn` |
| G2 | The process **never sees a client address**; even if it did, handlers cannot read it or identifying headers | peer is always Tor; `privacy.Middleware` redacts `RemoteAddr` and strips `X-Forwarded-For`, `User-Agent`, … |
| G3 | **Logs cannot leak** IPs / onion names / intent data, and timestamps are coarse | `privacy.NewLogger` (banned keys, pattern scrub, 15-min time truncation); `net/http` error log routed through it |
| G4 | **Outbound** chain-RPC and signer traffic goes through Tor with per-destination circuits; no fallback path | `privacy.TorTransport` (`socks5h`, isolation ids); startup `CheckSOCKS`; systemd `IPAddressDeny=any` except loopback |
| G5 | Intents live **in RAM only**; no core dumps / ptrace; witness scratch files on tmpfs and deleted | `privacy.Harden`, `LimitCORE=0`, prover `os.RemoveAll` |

Each is a tested property, not a recommendation (`go test ./internal/privacy/`).

## Install (Debian/Ubuntu sketch)
```bash
useradd --system --home /nonexistent --shell /usr/sbin/nologin automator
usermod -aG automator debian-tor                       # lets Tor connect to the 0660 unix socket
install -m0755 automator /usr/local/bin/automator
install -d -m0750 -o root -g automator /etc/automator && cp config.example.json /etc/automator/config.json
install -m0600 /dev/null /etc/automator/secrets.env    # AUTOMATOR_SIGNER_TOKEN=…
cp deploy/tor/torrc /etc/tor/torrc && cp deploy/systemd/automator.service /etc/systemd/system/
systemctl restart tor && systemctl enable --now automator
cat /var/lib/tor/automator/hostname                    # publish this .onion address to wallets
```
The signing key is NOT on this host: `signerUrl` is an MPC/TSS gateway (ideally broadcasting through a private relay
so batches do not appear in the public mempool before inclusion).

## Verify it yourself
```bash
ss -ltnp | grep automator          # → nothing (no TCP listener); only the unix socket exists
curl --socks5-hostname 127.0.0.1:9050 http://<hostname>.onion/healthz   # reachable via Tor
journalctl -u automator | grep -E '([0-9]{1,3}\.){3}[0-9]{1,3}|\.onion'   # → nothing
sudo -u automator curl -m 5 https://example.com                          # → fails (IPAddressDeny)
```

## What this does NOT protect (read this)
* A user's **own ISP** can see that they use Tor (use bridges if that matters).
* A **global passive adversary** correlating timing between a user's Tor traffic and on-chain batches could narrow an
  anonymity set: keep `intervalSeconds`/`jitterSeconds` large and `minSwaps` ≥ the anonymity set you promise.
* The Automator **sees intent contents** (it must, to prove them). Choose an operator accordingly, or run your own.
* On-chain privacy leaks by arithmetic exist regardless of the network layer: the NET flow per asset is public, and a
  batch with one real swap would reveal its size (hence `minSwaps`).
* The deposit and withdrawal edges (`depositNote`/`withdrawNote`) reveal their amounts and the calling address.
* Operator mistakes (e.g. `devAllowDirectEgress`, `devAllowClearnetListen`) are privacy downgrades by design; they
  print a warning and exist only for local development.
