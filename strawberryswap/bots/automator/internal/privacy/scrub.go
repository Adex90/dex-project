package privacy

import (
	"io"
	"log/slog"
	"net"
	"regexp"
	"strings"
	"time"
)

var (
	scrubbers = []*regexp.Regexp{
		regexp.MustCompile(`\b(?:\d{1,3}\.){3}\d{1,3}\b`),    // IPv4
		regexp.MustCompile(`(?i)\b[a-z2-7]{16,56}\.onion\b`), // onion service names
		regexp.MustCompile(`(?i)\b0x[0-9a-f]{16,}\b`),        // long hex (hashes, notes, keys)
		regexp.MustCompile(`\b\d{15,}\b`),                    // long decimals (field elements, amounts)
	}
	// attributes that must never reach a log line, whatever their value
	bannedKeys = map[string]bool{
		"remote_addr": true, "remoteaddr": true, "ip": true, "client": true, "client_ip": true, "peer": true,
		"user_agent": true, "useragent": true, "referer": true, "host": true, "intent": true, "witness": true,
		"nullifier": true, "commitment": true, "value": true, "dest": true, "receipt": true,
	}
)

// ipv6Candidate finds runs of hex digits / colons / dots; a run is redacted only if the standard library parses it
// as an IP. (A regex cannot recognise IPv6 reliably: compressed forms like 2001:db8::1 and IPv4-mapped forms
// defeat it, while times like 12:30:45 and MACs must not be touched.)
var ipv6Candidate = regexp.MustCompile(`[0-9A-Fa-f:.]{2,}`)

// Scrub redacts anything that could identify a network peer or reveal intent data.
func Scrub(s string) string {
	for _, re := range scrubbers {
		s = re.ReplaceAllString(s, "<redacted>")
	}
	return ipv6Candidate.ReplaceAllStringFunc(s, func(tok string) string {
		if strings.Count(tok, ":") >= 2 && net.ParseIP(strings.Trim(tok, ".")) != nil {
			return "<redacted>"
		}
		return tok
	})
}

// NewLogger returns a structured logger that enforces G3. Timestamps are truncated to 15 minutes: operational logs
// stay useful, but a line can no longer be matched against the arrival time of a particular request.
func NewLogger(w io.Writer, level slog.Level) *slog.Logger {
	return slog.New(slog.NewTextHandler(w, &slog.HandlerOptions{
		Level: level,
		ReplaceAttr: func(groups []string, a slog.Attr) slog.Attr {
			if bannedKeys[a.Key] {
				return slog.Attr{}
			}
			switch a.Value.Kind() {
			case slog.KindTime:
				return slog.Time(a.Key, a.Value.Time().Truncate(15*time.Minute))
			case slog.KindString, slog.KindAny:
				return slog.String(a.Key, Scrub(a.Value.String()))
			}
			return a
		},
	}))
}
