//go:build linux

package privacy

import "syscall"

func umask(m int) int { return syscall.Umask(m) }

// Harden implements the process part of G5: no core dumps, and not attachable by ptrace/gdb of the same uid
// (PR_SET_DUMPABLE = 0), so intents in memory cannot be lifted from a crashed or inspected process.
func Harden() error {
	if err := syscall.Setrlimit(syscall.RLIMIT_CORE, &syscall.Rlimit{Cur: 0, Max: 0}); err != nil {
		return err
	}
	const prSetDumpable = 4
	if _, _, e := syscall.RawSyscall(syscall.SYS_PRCTL, prSetDumpable, 0, 0); e != 0 {
		return e
	}
	return nil
}
