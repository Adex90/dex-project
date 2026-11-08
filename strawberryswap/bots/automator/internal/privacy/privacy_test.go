package privacy

import (
	"bytes"
	"context"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestOnionModeRefusesNonLoopback(t *testing.T) {
	for _, bad := range []string{"0.0.0.0:8080", ":8080", "192.168.1.5:80", "203.0.113.7:80", "example.com:80", "localhost:80", "[::]:80", "unix:relative.sock", "unix:"} {
		if err := ValidateAddr(bad, Onion); err == nil {
			t.Errorf("%q accepted in onion mode", bad)
		}
	}
	for _, ok := range []string{"127.0.0.1:8080", "127.5.5.5:1", "[::1]:8080", "unix:/run/automator/intents.sock"} {
		if err := ValidateAddr(ok, Onion); err != nil {
			t.Errorf("%q rejected: %v", ok, err)
		}
	}
	if err := ValidateAddr("0.0.0.0:8080", Clearnet); err != nil {
		t.Error("clearnet mode (explicit opt-in) must allow it")
	}
	if _, err := Listen("0.0.0.0:0", Onion); err == nil {
		t.Fatal("Listen must enforce the same rule")
	}
}

func TestUnixSocketPermsAndStaleHandling(t *testing.T) {
	dir := t.TempDir()
	sock := filepath.Join(dir, "i.sock")
	l, err := Listen("unix:"+sock, Onion)
	if err != nil {
		t.Fatal(err)
	}
	fi, _ := os.Stat(sock)
	if perm := fi.Mode().Perm(); perm&0o007 != 0 {
		t.Fatalf("socket is world-accessible: %o", perm)
	}
	l.Close()
	l2, err := Listen("unix:"+sock, Onion) // stale socket file replaced
	if err != nil {
		t.Fatalf("stale socket not replaced: %v", err)
	}
	l2.Close()
	regular := filepath.Join(dir, "precious.txt")
	_ = os.WriteFile(regular, []byte("keep"), 0o600)
	if _, err := Listen("unix:"+regular, Onion); err == nil {
		t.Fatal("must not clobber a regular file")
	}
	if b, _ := os.ReadFile(regular); string(b) != "keep" {
		t.Fatal("regular file was damaged")
	}
}

func TestScrubRedactsNetworkAndIntentIdentifiers(t *testing.T) {
	for _, in := range []string{
		"conn from 203.0.113.7:4455", "ipv6 2001:db8::1 failed", "peer fe80::1ff:fe23:4567:890a", "dial [2001:db8:85a3::8a2e:370:7334]:443", "mapped ::ffff:203.0.113.7 seen", "loopback ::1 here",
		"service dwtqxmzk3d2gpcnuw6ahwqbvtnpjy7ufcl2e7ypwn3bgzhlvv6nrwmad.onion", "note 0x00000000000000000000000000000000deadbeefdeadbeef",
		"amount 1000000000000000000000", "field 13751787724388560630309404960166971921655704192150233860338172926954813920239",
	} {
		out := Scrub(in)
		if !strings.Contains(out, "<redacted>") {
			t.Errorf("not scrubbed: %q -> %q", in, out)
		}
	}
	for _, keep := range []string{"epoch 42 settled 3 intents", "timestamp 1800000000", "batch 16 slots", "took 12:30:45 total", "mac aa:bb:cc:dd:ee:ff", "ratio 1.5 ok"} {
		if Scrub(keep) != keep {
			t.Errorf("over-scrubbed: %q -> %q", keep, Scrub(keep))
		}
	}
}

func TestLoggerDropsBannedKeysAndCoarsensTime(t *testing.T) {
	var buf bytes.Buffer
	log := NewLogger(&buf, slog.LevelInfo)
	log.Info("served request from 198.51.100.9", "remote_addr", "198.51.100.9:5", "user_agent", "curl/8", "intent", "secret", "epoch", 7, "note", "0xaaaaaaaaaaaaaaaaaaaaaaaa")
	out := buf.String()
	for _, leak := range []string{"198.51.100.9", "curl", "secret", "aaaaaaaa"} {
		if strings.Contains(out, leak) {
			t.Fatalf("log leaks %q:\n%s", leak, out)
		}
	}
	if !strings.Contains(out, "epoch=7") {
		t.Fatalf("operational data must survive:\n%s", out)
	}
	// time truncated to a 15-minute boundary
	i := strings.Index(out, "time=")
	ts, err := time.Parse(time.RFC3339Nano, strings.Fields(out[i+5:])[0])
	if err != nil || ts.Minute()%15 != 0 || ts.Second() != 0 || ts.Nanosecond() != 0 {
		t.Fatalf("timestamp not coarsened: %v %v", ts, err)
	}
}

// What a handler can see after the middleware: no peer, no identifying headers.
func TestMiddlewareBlindsHandlers(t *testing.T) {
	var seenAddr string
	var seenHeaders http.Header
	h := Middleware(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seenAddr, seenHeaders = r.RemoteAddr, r.Header.Clone()
	}))
	req := httptest.NewRequest("POST", "/v1/intents", nil)
	req.RemoteAddr = "203.0.113.7:4444"
	for k, v := range map[string]string{"X-Forwarded-For": "198.51.100.1", "User-Agent": "Mozilla", "Cookie": "a=b", "Referer": "http://x", "Content-Type": "application/json"} {
		req.Header.Set(k, v)
	}
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if seenAddr != "redacted" || strings.Contains(seenAddr, "203.0.113.7") {
		t.Fatalf("handler saw the peer: %q", seenAddr)
	}
	for _, k := range []string{"X-Forwarded-For", "User-Agent", "Cookie", "Referer"} {
		if seenHeaders.Get(k) != "" {
			t.Fatalf("header %s reached the handler", k)
		}
	}
	if seenHeaders.Get("Content-Type") == "" {
		t.Fatal("functional headers must be preserved")
	}
	if rec.Header().Get("Cache-Control") != "no-store" || rec.Header().Get("Set-Cookie") != "" {
		t.Fatal("response hardening headers missing / cookie set")
	}
}

// net/http logs peer addresses in its own error lines; they must pass through the scrubber.
func TestServerErrorLogIsScrubbed(t *testing.T) {
	var buf bytes.Buffer
	srv := NewServer(http.NotFoundHandler(), NewLogger(&buf, slog.LevelDebug))
	srv.ErrorLog.Printf("http: TLS handshake error from %s: EOF", "198.51.100.9:51234")
	if strings.Contains(buf.String(), "198.51.100.9") {
		t.Fatalf("server error log leaks the peer: %s", buf.String())
	}
}

func TestEndToEndNoPeerInResponseOrLog(t *testing.T) {
	var logBuf bytes.Buffer
	log := NewLogger(&logBuf, slog.LevelDebug)
	srv := NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		log.Info("handled", "peer", r.RemoteAddr, "ua", r.UserAgent())
		w.Write([]byte("ok"))
	}), log)
	l, err := Listen("127.0.0.1:0", Onion)
	if err != nil {
		t.Fatal(err)
	}
	go srv.Serve(l)
	defer srv.Close()
	req, _ := http.NewRequest("GET", "http://"+l.Addr().String()+"/", nil)
	req.Header.Set("User-Agent", "SuperSecretAgent/1.0")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if strings.Contains(logBuf.String(), "127.0.0.1") || strings.Contains(logBuf.String(), "SuperSecretAgent") {
		t.Fatalf("log leaked: %s", logBuf.String())
	}
}

func TestTorTransportUsesRemoteDNSAndIsolation(t *testing.T) {
	tr, err := TorTransport("127.0.0.1:9050", "rpc")
	if err != nil {
		t.Fatal(err)
	}
	req, _ := http.NewRequest("GET", "https://rpc.example/", nil)
	u, err := tr.Proxy(req)
	if err != nil || u.Scheme != "socks5h" || u.User.Username() != "rpc" {
		t.Fatalf("proxy = %v (%v): need socks5h with isolation id", u, err)
	}
	tr2, _ := TorTransport("127.0.0.1:9050", "signer")
	u2, _ := tr2.Proxy(req)
	if u.User.Username() == u2.User.Username() {
		t.Fatal("different destinations must use different circuit-isolation ids")
	}
}

func TestCheckSOCKS(t *testing.T) {
	l, _ := net.Listen("tcp", "127.0.0.1:0")
	defer l.Close()
	go func() {
		c, err := l.Accept()
		if err != nil {
			return
		}
		buf := make([]byte, 3)
		c.Read(buf)
		c.Write([]byte{5, 0})
		c.Close()
	}()
	if err := CheckSOCKS(context.Background(), l.Addr().String()); err != nil {
		t.Fatal(err)
	}
	dead, _ := net.Listen("tcp", "127.0.0.1:0")
	addr := dead.Addr().String()
	dead.Close()
	if err := CheckSOCKS(context.Background(), addr); err == nil {
		t.Fatal("a dead proxy must fail the startup check")
	}
}

func TestHarden(t *testing.T) {
	if err := Harden(); err != nil && !strings.Contains(err.Error(), "only implemented on Linux") {
		t.Fatal(err)
	}
}
