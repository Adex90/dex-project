//go:build !linux

package privacy

import "errors"

func umask(m int) int { return m }

// Harden is Linux-only; elsewhere the caller is told, and must treat the platform as development-only.
func Harden() error { return errors.New("privacy: process hardening is only implemented on Linux") }
