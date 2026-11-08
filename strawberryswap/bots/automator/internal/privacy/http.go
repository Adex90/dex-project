package privacy

import (
	"log/slog"
	"net/http"
	"time"
)

// identifying request headers removed before any handler can read (or log) them
var stripped = []string{
	"X-Forwarded-For", "X-Real-Ip", "Forwarded", "Via", "X-Forwarded-Host", "X-Forwarded-Proto", "X-Client-Ip",
	"Cf-Connecting-Ip", "True-Client-Ip", "User-Agent", "Referer", "Origin", "Accept-Language", "Cookie", "Dnt",
}

// Middleware enforces G2. After it runs, no handler can observe a network peer: RemoteAddr is a constant.
func Middleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		r.RemoteAddr = "redacted"
		for _, h := range stripped {
			r.Header.Del(h)
		}
		h := w.Header()
		h.Set("Cache-Control", "no-store")
		h.Set("Referrer-Policy", "no-referrer")
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("Content-Security-Policy", "default-src 'none'")
		next.ServeHTTP(w, r)
	})
}

// NewServer builds an http.Server with slowloris-resistant timeouts, a bounded header size and an ErrorLog routed
// through the scrubbing logger (net/http's own error lines embed peer addresses).
func NewServer(h http.Handler, log *slog.Logger) *http.Server {
	return &http.Server{
		Handler:                      Middleware(h),
		ReadHeaderTimeout:            5 * time.Second,
		ReadTimeout:                  10 * time.Second,
		WriteTimeout:                 30 * time.Second,
		IdleTimeout:                  30 * time.Second,
		MaxHeaderBytes:               8 << 10,
		DisableGeneralOptionsHandler: true,
		ErrorLog:                     slog.NewLogLogger(log.Handler(), slog.LevelWarn),
	}
}
