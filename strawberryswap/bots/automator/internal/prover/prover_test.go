package prover

import (
	"context"
	"math/big"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func script(t *testing.T, body string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "fake-snarkjs")
	if err := os.WriteFile(p, []byte("#!/bin/sh\n"+body+"\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	return p
}

// args: groth16 fullprove <input> <wasm> <zkey> <proof> <public>   ($1 $2 $3 $4 $5 $6 $7)
const okScript = `
printf '%s' '{"pi_a":["1","2","1"],"pi_b":[["3","4"],["5","6"],["1","0"]],"pi_c":["7","8","1"],"protocol":"groth16"}' > "$6"
echo '["10","20","30"]' > "$7"
`

func TestProofEncodingSwapsPiBAndChecksPublicSignals(t *testing.T) {
	p := &SnarkJS{Bin: script(t, okScript), Wasm: "w", Zkey: "z", TmpDir: t.TempDir(), Timeout: 10 * time.Second}
	exp := []*big.Int{big.NewInt(10), big.NewInt(20), big.NewInt(30)}
	proof, err := p.Prove(context.Background(), map[string]any{"x": "1"}, exp)
	if err != nil {
		t.Fatal(err)
	}
	if len(proof.Bytes) != 256 {
		t.Fatalf("proof must be 256 bytes, got %d", len(proof.Bytes))
	}
	words := make([]int64, 8)
	for i := range words {
		words[i] = new(big.Int).SetBytes(proof.Bytes[i*32 : (i+1)*32]).Int64()
	}
	// a0 a1 | b01 b00 b11 b10 (pairs swapped, as snarkjs' Solidity verifiers expect) | c0 c1
	want := []int64{1, 2, 4, 3, 6, 5, 7, 8}
	for i := range want {
		if words[i] != want[i] {
			t.Fatalf("word %d = %d, want %d (all: %v)", i, words[i], want[i], words)
		}
	}

	if _, err := p.Prove(context.Background(), nil, []*big.Int{big.NewInt(10), big.NewInt(20), big.NewInt(31)}); err == nil || !strings.Contains(err.Error(), "differs from the on-chain layout") {
		t.Fatalf("a public-signal mismatch must be refused, got %v", err)
	}
	if _, err := p.Prove(context.Background(), nil, exp[:2]); err == nil {
		t.Fatal("wrong signal count must be refused")
	}
}

func TestWitnessFilesAreRemovedAndConstraintErrorsDoNotLeak(t *testing.T) {
	tmp := t.TempDir()
	fail := script(t, `echo "Assert Failed. input value=123456789012345678901234567890" >&2; exit 1`)
	p := &SnarkJS{Bin: fail, Wasm: "w", Zkey: "z", TmpDir: tmp, Timeout: 10 * time.Second}
	_, err := p.Prove(context.Background(), map[string]any{"secret": "123456789012345678901234567890"}, nil)
	if err == nil {
		t.Fatal("expected failure")
	}
	if strings.Contains(err.Error(), "123456789") {
		t.Fatalf("error leaks witness data: %v", err)
	}
	left, _ := os.ReadDir(tmp)
	if len(left) != 0 {
		t.Fatalf("witness scratch files left behind: %v", left)
	}
}

func TestTimeoutKillsTheProver(t *testing.T) {
	p := &SnarkJS{Bin: script(t, "sleep 5"), Wasm: "w", Zkey: "z", TmpDir: t.TempDir(), Timeout: 200 * time.Millisecond}
	start := time.Now()
	if _, err := p.Prove(context.Background(), nil, nil); err == nil || time.Since(start) > 3*time.Second {
		t.Fatalf("prover must be cut off at the timeout (took %v, err %v)", time.Since(start), err)
	}
}

// The witness file is created 0600 inside a 0700 directory, and the directory is removed afterwards.
func TestInputFileIsPrivateWhileProving(t *testing.T) {
	probe := filepath.Join(t.TempDir(), "mode.txt")
	p := &SnarkJS{
		Bin:  script(t, `stat -c %a "$3" > `+probe+`; stat -c %a "$(dirname "$3")" >> `+probe+`; exit 1`),
		Wasm: "w", Zkey: "z", TmpDir: t.TempDir(), Timeout: 10 * time.Second,
	}
	_, _ = p.Prove(context.Background(), map[string]any{"a": "b"}, nil)
	got, err := os.ReadFile(probe)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "600\n700\n" {
		t.Fatalf("witness file / dir modes = %q, want 600 and 700", got)
	}
}
