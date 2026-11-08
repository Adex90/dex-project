package privacy

import (
	"context"
	"errors"
	"net"
	"net/http"
	"net/url"
	"time"
)

// TorTransport implements G4: every request goes through the Tor SOCKS5 proxy with REMOTE DNS (socks5h, so no
// local resolver leak). `isolation` becomes the SOCKS username: with Tor's IsolateSOCKSAuth, each distinct value
// gets its own circuit, so the chain-RPC provider and the signer gateway cannot link their views of this node.
func TorTransport(socksAddr, isolation string) (*http.Transport, error) {
	if socksAddr == "" {
		return nil, errors.New("privacy: empty SOCKS address")
	}
	u := &url.URL{Scheme: "socks5h", User: url.UserPassword(isolation, "x"), Host: socksAddr}
	return &http.Transport{
		Proxy:                 http.ProxyURL(u),
		TLSHandshakeTimeout:   30 * time.Second,
		ResponseHeaderTimeout: 60 * time.Second,
		MaxIdleConns:          4,
		IdleConnTimeout:       30 * time.Second,
		ForceAttemptHTTP2:     true,
	}, nil
}

// CheckSOCKS verifies that something speaking SOCKS5 answers at addr — the startup check that stops the node from
// running with a dead Tor and silently falling back to anything else (there is no fallback path in this binary).
func CheckSOCKS(ctx context.Context, addr string) error {
	d := net.Dialer{Timeout: 5 * time.Second}
	c, err := d.DialContext(ctx, "tcp", addr)
	if err != nil {
		return err
	}
	defer c.Close()
	_ = c.SetDeadline(time.Now().Add(5 * time.Second))
	if _, err := c.Write([]byte{5, 1, 0}); err != nil { // SOCKS5, 1 method: no-auth
		return err
	}
	var r [2]byte
	if _, err := c.Read(r[:]); err != nil || r[0] != 5 {
		return errors.New("privacy: not a SOCKS5 proxy")
	}
	return nil
}
