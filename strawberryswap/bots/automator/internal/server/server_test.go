package server

import (
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"strawberryswap/automator/internal/intent"
	"strawberryswap/automator/internal/service"
)

type fakeBackend struct{ err error }

func (f *fakeBackend) Submit(in *intent.Intent) (string, error) {
	if f.err != nil {
		return "", f.err
	}
	return "r1", nil
}
func (f *fakeBackend) Status(r string) (service.Status, bool) {
	if r == "r1" {
		return service.Status{State: service.Queued}, true
	}
	return service.Status{}, false
}
func (f *fakeBackend) Info() service.Info { return service.Info{BatchSize: 4} }

const good = `{"dir":0,"value":"1000","destPubKey":"11","refundPubKey":"22","destBlinding":"33","minOut":"5","deadline":1800000000}`

func do(h http.Handler, method, path, ct, body string) *httptest.ResponseRecorder {
	req := httptest.NewRequest(method, path, strings.NewReader(body))
	if ct != "" {
		req.Header.Set("Content-Type", ct)
	}
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	return rec
}

func TestIntentEndpoint(t *testing.T) {
	cases := []struct {
		name   string
		err    error
		ct     string
		body   string
		status int
	}{
		{"ok", nil, "application/json", good, 202},
		{"wrong content type", nil, "text/plain", good, 415},
		{"no content type", nil, "", good, 415},
		{"malformed json", nil, "application/json", `{"dir":`, 400},
		{"json number rather than string", nil, "application/json", strings.Replace(good, `"1000"`, `1000`, 1), 400},
		{"oversize body", nil, "application/json", `{"pad":"` + strings.Repeat("a", 4096) + `"}`, 413},
		{"invalid intent", intent.ErrInvalid, "application/json", good, 400},
		{"unknown note", service.ErrUnknownNote, "application/json", good, 404},
		{"capacity", service.ErrBusy, "application/json", good, 503},
		{"internal error is not echoed", errors.New("db password is hunter2"), "application/json", good, 500},
	}
	for _, c := range cases {
		rec := do(New(&fakeBackend{err: c.err}, 4), "POST", "/v1/intents", c.ct, c.body)
		if rec.Code != c.status {
			t.Errorf("%s: status %d, want %d", c.name, rec.Code, c.status)
		}
		if strings.Contains(rec.Body.String(), "hunter2") || strings.Contains(rec.Body.String(), "1000") {
			t.Errorf("%s: response echoes internals or input: %s", c.name, rec.Body.String())
		}
	}
}

func TestStatusInfoHealth(t *testing.T) {
	h := New(&fakeBackend{}, 4)
	if r := do(h, "GET", "/v1/intents/r1", "", ""); r.Code != 200 || !strings.Contains(r.Body.String(), "queued") {
		t.Fatal("status")
	}
	if r := do(h, "GET", "/v1/intents/nope", "", ""); r.Code != 404 {
		t.Fatal("unknown receipt")
	}
	if r := do(h, "GET", "/v1/info", "", ""); r.Code != 200 {
		t.Fatal("info")
	}
	if r := do(h, "GET", "/healthz", "", ""); r.Code != 204 {
		t.Fatal("health")
	}
	if r := do(h, "GET", "/v1/intents", "", ""); r.Code != 405 && r.Code != 404 {
		t.Fatalf("GET on the submit path must not succeed (%d)", r.Code)
	}
}

func TestConcurrencyLimiterShedsLoad(t *testing.T) {
	block := make(chan struct{})
	entered := make(chan struct{}, 4)
	h := New(blockingBackend{block, entered}, 1)
	done := make(chan *httptest.ResponseRecorder, 1)
	go func() { done <- do(h, "POST", "/v1/intents", "application/json", good) }()
	<-entered // first request holds the only slot
	if r := do(h, "POST", "/v1/intents", "application/json", good); r.Code != 503 {
		t.Fatalf("second concurrent request must be shed with 503, got %d", r.Code)
	}
	close(block)
	if r := <-done; r.Code != 202 {
		t.Fatalf("first request should complete, got %d", r.Code)
	}
}

type blockingBackend struct{ block, entered chan struct{} }

func (b blockingBackend) Submit(*intent.Intent) (string, error) {
	b.entered <- struct{}{}
	<-b.block
	return "r", nil
}
func (b blockingBackend) Status(string) (service.Status, bool) { return service.Status{}, false }
func (b blockingBackend) Info() service.Info                   { return service.Info{} }

func TestCORSPreflightAndHeadersWithoutCredentials(t *testing.T) {
	h := New(&fakeBackend{}, 4)
	pre := do(h, "OPTIONS", "/v1/intents", "", "")
	if pre.Code != 204 {
		t.Fatalf("preflight status %d", pre.Code)
	}
	for k, want := range map[string]string{"Access-Control-Allow-Origin": "*", "Access-Control-Allow-Methods": "GET, POST, OPTIONS", "Access-Control-Allow-Headers": "Content-Type"} {
		if got := pre.Header().Get(k); got != want {
			t.Errorf("%s = %q, want %q", k, got, want)
		}
	}
	post := do(h, "POST", "/v1/intents", "application/json", good)
	if post.Header().Get("Access-Control-Allow-Origin") != "*" {
		t.Error("actual responses must carry the CORS header too")
	}
	for _, r := range []*httptest.ResponseRecorder{pre, post} {
		if r.Header().Get("Access-Control-Allow-Credentials") != "" {
			t.Error("credentials must never be allowed")
		}
	}
}
