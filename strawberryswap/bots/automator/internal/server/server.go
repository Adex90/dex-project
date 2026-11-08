// Package server exposes the Automator's intake API. It is deliberately tiny and stateless-looking: no accounts,
// no cookies, no per-client state, and — because it is only ever reached through an onion service — no client address.
package server

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"

	"strawberryswap/automator/internal/intent"
	"strawberryswap/automator/internal/service"
)

const maxBody = 2 << 10 // an intent is ~400 bytes

type Backend interface {
	Submit(*intent.Intent) (string, error)
	Status(string) (service.Status, bool)
	Info() service.Info
}

// New returns the API handler. `maxInflight` bounds concurrent requests (the only abuse control that does not need
// to know who the caller is); excess requests get 503 immediately.
func New(b Backend, maxInflight int) http.Handler {
	sem := make(chan struct{}, maxInflight)
	mux := http.NewServeMux()
	mux.HandleFunc("POST /v1/intents", func(w http.ResponseWriter, r *http.Request) {
		select {
		case sem <- struct{}{}:
			defer func() { <-sem }()
		default:
			writeErr(w, http.StatusServiceUnavailable, "busy")
			return
		}
		if ct := r.Header.Get("Content-Type"); ct != "application/json" {
			writeErr(w, http.StatusUnsupportedMediaType, "content-type must be application/json")
			return
		}
		body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, maxBody))
		if err != nil {
			writeErr(w, http.StatusRequestEntityTooLarge, "body too large")
			return
		}
		var in intent.Intent
		if err := json.Unmarshal(body, &in); err != nil {
			writeErr(w, http.StatusBadRequest, "malformed intent")
			return
		}
		receipt, err := b.Submit(&in)
		switch {
		case err == nil:
			writeJSON(w, http.StatusAccepted, map[string]string{"receipt": receipt})
		case errors.Is(err, intent.ErrInvalid):
			writeErr(w, http.StatusBadRequest, "invalid intent")
		case errors.Is(err, service.ErrUnknownNote):
			writeErr(w, http.StatusNotFound, "intent note not found in the note tree")
		case errors.Is(err, service.ErrBusy):
			writeErr(w, http.StatusServiceUnavailable, "busy")
		default:
			writeErr(w, http.StatusInternalServerError, "internal error") // never echo internals
		}
	})
	mux.HandleFunc("GET /v1/intents/{receipt}", func(w http.ResponseWriter, r *http.Request) {
		st, ok := b.Status(r.PathValue("receipt"))
		if !ok {
			writeErr(w, http.StatusNotFound, "unknown receipt")
			return
		}
		writeJSON(w, http.StatusOK, st)
	})
	mux.HandleFunc("GET /v1/info", func(w http.ResponseWriter, r *http.Request) { writeJSON(w, http.StatusOK, b.Info()) })
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(http.StatusNoContent) })
	return cors(mux)
}

// cors lets the static frontend (served from any origin: an IPFS gateway, an ENS name, localhost) call this API from
// the browser. The policy is a constant wildcard on purpose:
//   - privacy.Middleware strips `Origin` (and every other identifying header) before handlers run, so the server
//     CANNOT reflect an origin, and must not want to;
//   - it is safe because the API is credential-free: no cookies, no ambient authority, no per-client state — an
//     intent is only useful if its note exists on-chain, and "Allow-Credentials" is never sent.
func cors(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("Access-Control-Allow-Origin", "*")
		h.Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
		h.Set("Access-Control-Allow-Headers", "Content-Type")
		h.Set("Access-Control-Max-Age", "600")
		if r.Method == http.MethodOptions { // preflight for the JSON POST
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(v)
}

func writeErr(w http.ResponseWriter, code int, msg string) {
	writeJSON(w, code, map[string]string{"error": msg})
}
