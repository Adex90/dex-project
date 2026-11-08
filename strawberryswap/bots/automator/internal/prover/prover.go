// Package prover wraps the Groth16 prover for circuits/batch_swap. The witness contains every user's private
// intent terms, so it is written only to a private tmpfs directory and removed immediately after proving.
package prover

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"time"
)

// Proof is the on-wire proof (abi.encode(uint256[2] a, uint256[2][2] b, uint256[2] c) = 256 bytes) together with
// the public signals the prover reported.
type Proof struct {
	Bytes  []byte
	Public []*big.Int
}

type Prover interface {
	// Prove generates a proof for `witness` and checks that the prover's public signals equal `expected`
	// (a mismatch means the witness and the on-chain public-input layout disagree — never submit such a proof).
	Prove(ctx context.Context, witness map[string]any, expected []*big.Int) (*Proof, error)
}

// SnarkJS shells out to `snarkjs groth16 fullprove` (witness generation in WASM + proving). Swap in a faster
// native prover (rapidsnark) behind the same interface for production throughput.
type SnarkJS struct {
	Bin     string        // path to the snarkjs executable
	Wasm    string        // circuit witness generator
	Zkey    string        // proving key
	TmpDir  string        // private scratch directory (use tmpfs, e.g. /dev/shm)
	Timeout time.Duration // hard cap on one proof
}

func (p *SnarkJS) Prove(ctx context.Context, witness map[string]any, expected []*big.Int) (*Proof, error) {
	if p.Timeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, p.Timeout)
		defer cancel()
	}
	dir, err := os.MkdirTemp(p.TmpDir, "prove-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(dir) // witness holds private intent data: never leave it behind
	if err := os.Chmod(dir, 0o700); err != nil {
		return nil, err
	}
	in, proofPath, pubPath := filepath.Join(dir, "input.json"), filepath.Join(dir, "proof.json"), filepath.Join(dir, "public.json")
	raw, err := json.Marshal(witness)
	if err != nil {
		return nil, err
	}
	if err := os.WriteFile(in, raw, 0o600); err != nil {
		return nil, err
	}

	// The prover runs inside the private scratch directory, so every other path must be absolute.
	wasm, err := filepath.Abs(p.Wasm)
	if err != nil {
		return nil, err
	}
	zkey, err := filepath.Abs(p.Zkey)
	if err != nil {
		return nil, err
	}
	cmd := exec.CommandContext(ctx, p.Bin, "groth16", "fullprove", in, wasm, zkey, proofPath, pubPath)
	cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "HOME=" + dir} // no inherited secrets
	cmd.Dir = dir
	configure(cmd)
	var stderr limitedBuffer
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		// stderr can echo witness fields on a constraint failure: classify it, never log it verbatim
		if bytes.Contains(stderr.Bytes(), []byte("Assert Failed")) {
			return nil, errors.New("prover: witness violates a circuit constraint")
		}
		return nil, fmt.Errorf("prover: snarkjs failed: %w: %s", err, firstLine(stderr.Bytes()))
	}
	return readProof(proofPath, pubPath, expected)
}

func readProof(proofPath, pubPath string, expected []*big.Int) (*Proof, error) {
	pb, err := os.ReadFile(proofPath)
	if err != nil {
		return nil, err
	}
	var pj struct {
		A []string   `json:"pi_a"`
		B [][]string `json:"pi_b"`
		C []string   `json:"pi_c"`
	}
	if err := json.Unmarshal(pb, &pj); err != nil {
		return nil, err
	}
	if len(pj.A) < 2 || len(pj.B) < 2 || len(pj.C) < 2 || len(pj.B[0]) < 2 || len(pj.B[1]) < 2 {
		return nil, errors.New("prover: malformed proof.json")
	}
	// snarkjs' Solidity verifiers take pi_b with each coordinate pair swapped (Fp2 ordering).
	words := []string{pj.A[0], pj.A[1], pj.B[0][1], pj.B[0][0], pj.B[1][1], pj.B[1][0], pj.C[0], pj.C[1]}
	out := make([]byte, 0, 256)
	for _, w := range words {
		v, ok := new(big.Int).SetString(w, 10)
		if !ok {
			return nil, errors.New("prover: bad coordinate")
		}
		var b [32]byte
		v.FillBytes(b[:])
		out = append(out, b[:]...)
	}

	ub, err := os.ReadFile(pubPath)
	if err != nil {
		return nil, err
	}
	var ps []string
	if err := json.Unmarshal(ub, &ps); err != nil {
		return nil, err
	}
	pub := make([]*big.Int, len(ps))
	for i, s := range ps {
		v, ok := new(big.Int).SetString(s, 10)
		if !ok {
			return nil, errors.New("prover: bad public signal")
		}
		pub[i] = v
	}
	if len(pub) != len(expected) {
		return nil, fmt.Errorf("prover: %d public signals, expected %d", len(pub), len(expected))
	}
	for i := range pub {
		if pub[i].Cmp(expected[i]) != 0 {
			return nil, fmt.Errorf("prover: public signal %d differs from the on-chain layout", i)
		}
	}
	return &Proof{Bytes: out, Public: pub}, nil
}

// Mock returns a syntactically valid but cryptographically meaningless proof (for tests and dry runs against a
// MockVerifier). It is the ONLY prover that never touches a witness file.
type Mock struct{ Calls int }

func (m *Mock) Prove(_ context.Context, _ map[string]any, expected []*big.Int) (*Proof, error) {
	m.Calls++
	return &Proof{Bytes: make([]byte, 256), Public: expected}, nil
}

type limitedBuffer struct{ buf bytes.Buffer }

func (l *limitedBuffer) Write(p []byte) (int, error) {
	if l.buf.Len() < 4096 {
		l.buf.Write(p)
	}
	return len(p), nil
}
func (l *limitedBuffer) Bytes() []byte { return l.buf.Bytes() }

var digitRun = regexp.MustCompile(`\d{8,}`)

// firstLine returns a short, digit-scrubbed excerpt of the prover's stderr: enough to diagnose a missing file or
// bad flag, too little to reproduce any witness value.
func firstLine(b []byte) string {
	line, _, _ := bytes.Cut(bytes.TrimSpace(b), []byte("\n"))
	s := digitRun.ReplaceAllString(string(line), "<n>")
	if len(s) > 160 {
		s = s[:160] + "…"
	}
	return s
}
