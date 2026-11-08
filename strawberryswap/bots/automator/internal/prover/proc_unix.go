//go:build unix

package prover

import (
	"os/exec"
	"syscall"
	"time"
)

// configure makes cancellation kill the WHOLE process group: a prover that forks helpers (node → workers) must not
// leave orphans holding witness data or the stderr pipe open after a timeout.
func configure(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error { return syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL) }
	cmd.WaitDelay = 2 * time.Second
}
