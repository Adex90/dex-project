//go:build !unix

package prover

import (
	"os/exec"
	"time"
)

func configure(cmd *exec.Cmd) { cmd.WaitDelay = 2 * time.Second }
