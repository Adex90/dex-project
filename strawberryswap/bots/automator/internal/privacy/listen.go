// Package privacy enforces the Automator's network-metadata guarantees IN CODE rather than in documentation:
//
//	G1  The intent endpoint can only be bound to loopback or a Unix socket. It is meant to be reached ONLY through
//	    a Tor onion service, so the process never sees a client IP at all (the peer is always 127.0.0.1 / the socket).
//	G2  Even so, the HTTP layer redacts RemoteAddr and strips every identifying header before any handler runs.
//	G3  Logs cannot carry IPs, onion addresses, user agents or intent data: address fields are dropped and values are
//	    pattern-scrubbed; timestamps are coarsened so log lines cannot be correlated with individual requests.
//	G4  Outbound traffic (chain RPC, signer) goes through a Tor SOCKS proxy with per-destination stream isolation,
//	    so neither node operators nor ISPs learn the Automator's address.
//	G5  The process disables core dumps / ptrace attach, and keeps intents in memory only (nothing is persisted).
//
// What this CANNOT do: hide that a Tor user connects to Tor from their own ISP, defeat a global passive adversary
// correlating traffic timing, or protect users who submit over clearnet. See deploy/README.md.
package privacy

import (
	"errors"
	"fmt"
	"net"
	"os"
	"strings"
)

type Mode int

const (
	// Onion is the default: loopback / Unix socket only.
	Onion Mode = iota
	// Clearnet permits any bind address. Users' IPs become visible to the host/ISP; it exists only for testing and
	// for operators who put their own anonymising front-end in place. It must be chosen explicitly.
	Clearnet
)

var ErrNotPrivate = errors.New("privacy: refusing to listen on a non-loopback address in onion mode")

// ValidateAddr applies G1 without opening anything (used by config validation and tests).
func ValidateAddr(addr string, mode Mode) error {
	if strings.HasPrefix(addr, "unix:") {
		if p := strings.TrimPrefix(addr, "unix:"); p == "" || !strings.HasPrefix(p, "/") {
			return fmt.Errorf("privacy: unix socket path must be absolute")
		}
		return nil
	}
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("privacy: bad listen address: %w", err)
	}
	if mode == Clearnet {
		return nil
	}
	ip := net.ParseIP(strings.Trim(host, "[]"))
	if ip == nil || !ip.IsLoopback() { // literal loopback IPs only: no DNS names, no 0.0.0.0, no public addresses
		return ErrNotPrivate
	}
	return nil
}

// Listen opens the listener. Unix sockets are created 0660 (so the Tor user, in the same group, can connect)
// and a stale socket file is replaced — but a non-socket file at that path is never deleted.
func Listen(addr string, mode Mode) (net.Listener, error) {
	if err := ValidateAddr(addr, mode); err != nil {
		return nil, err
	}
	if p, ok := strings.CutPrefix(addr, "unix:"); ok {
		if fi, err := os.Lstat(p); err == nil {
			if fi.Mode()&os.ModeSocket == 0 {
				return nil, fmt.Errorf("privacy: %s exists and is not a socket", p)
			}
			_ = os.Remove(p)
		}
		old := umask(0o117)
		l, err := net.Listen("unix", p)
		umask(old)
		return l, err
	}
	return net.Listen("tcp", addr)
}
