package prover

// The committed batch_swap (N = 4) proving artifacts in zk/ are the ones the testnet's on-chain verifier checks:
// every constant of contracts/test/fixtures/BatchSwapGroth16Verifier.dev.sol comes from zk/batch_swap_n4.vkey.json,
// and zk/SHA256SUMS pins the exact bytes the automator proves with.

import (
	"bufio"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

const zkDir = "../../zk"

func readFile(t *testing.T, path string) []byte {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	return b
}

// verifierConstants returns the `uint256 constant NAME = DIGITS;` lines of the Solidity verifier.
func verifierConstants(t *testing.T) map[string]string {
	t.Helper()
	src := readFile(t, "../../../../contracts/test/fixtures/BatchSwapGroth16Verifier.dev.sol")
	out := map[string]string{}
	sc := bufio.NewScanner(strings.NewReader(string(src)))
	for sc.Scan() {
		rest, ok := strings.CutPrefix(strings.TrimSpace(sc.Text()), "uint256 constant ")
		if !ok {
			continue
		}
		name, value, ok := strings.Cut(rest, "=")
		if !ok {
			continue
		}
		out[strings.TrimSpace(name)] = strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(value), ";"))
	}
	return out
}

func TestTheVerifierIsBuiltFromTheCommittedVerificationKey(t *testing.T) {
	var vkey struct {
		Protocol string     `json:"protocol"`
		Curve    string     `json:"curve"`
		NPublic  int        `json:"nPublic"`
		Alpha1   []string   `json:"vk_alpha_1"`
		Beta2    [][]string `json:"vk_beta_2"`
		Gamma2   [][]string `json:"vk_gamma_2"`
		Delta2   [][]string `json:"vk_delta_2"`
		IC       [][]string `json:"IC"`
	}
	if err := json.Unmarshal(readFile(t, filepath.Join(zkDir, "batch_swap_n4.vkey.json")), &vkey); err != nil {
		t.Fatal(err)
	}
	if vkey.Protocol != "groth16" || vkey.Curve != "bn128" {
		t.Fatalf("protocol %q curve %q", vkey.Protocol, vkey.Curve)
	}
	// batch_swap with N = 4: 8 header signals + 2 per slot (the pool's batchSize is 4).
	if vkey.NPublic != 16 || len(vkey.IC) != 17 {
		t.Fatalf("nPublic %d, %d IC points; want 16 and 17", vkey.NPublic, len(vkey.IC))
	}

	want := map[string]string{"alphax": vkey.Alpha1[0], "alphay": vkey.Alpha1[1]}
	// snarkjs's template writes each G2 coordinate as (c1, c0), the order of the pairing precompile.
	for name, g2 := range map[string][][]string{"beta": vkey.Beta2, "gamma": vkey.Gamma2, "delta": vkey.Delta2} {
		want[name+"x1"] = g2[0][1]
		want[name+"x2"] = g2[0][0]
		want[name+"y1"] = g2[1][1]
		want[name+"y2"] = g2[1][0]
	}
	for i, p := range vkey.IC {
		want[fmt.Sprintf("IC%dx", i)] = p[0]
		want[fmt.Sprintf("IC%dy", i)] = p[1]
	}

	got := verifierConstants(t)
	for _, field := range []string{"r", "q"} {
		if _, ok := got[field]; !ok {
			t.Fatalf("the verifier declares no field size %s", field)
		}
		delete(got, field)
	}
	if len(got) != 14+2*17 {
		t.Fatalf("%d verifier constants, want %d", len(got), 14+2*17)
	}
	if !reflect.DeepEqual(got, want) {
		for k, v := range want {
			if got[k] != v {
				t.Errorf("%s: verifier %s, vkey %s", k, got[k], v)
			}
		}
		t.Fatal("the verifier does not match zk/batch_swap_n4.vkey.json")
	}
}

func TestTheChecksumsPinEveryArtifact(t *testing.T) {
	sums := strings.Split(strings.TrimSpace(string(readFile(t, filepath.Join(zkDir, "SHA256SUMS")))), "\n")
	var files []string
	for _, line := range sums {
		fields := strings.Fields(line)
		if len(fields) != 2 || len(fields[0]) != 64 {
			t.Fatalf("malformed line %q", line)
		}
		files = append(files, fields[1])
		f, err := os.Open(filepath.Join(zkDir, fields[1]))
		if err != nil {
			t.Fatal(err)
		}
		h := sha256.New()
		if _, err := io.Copy(h, f); err != nil {
			t.Fatal(err)
		}
		_ = f.Close()
		if got := hex.EncodeToString(h.Sum(nil)); got != fields[0] {
			t.Errorf("%s: sha256 %s, SHA256SUMS says %s", fields[1], got, fields[0])
		}
	}
	if want := []string{"main_n4.wasm", "batch_swap_n4.zkey", "batch_swap_n4.vkey.json"}; !reflect.DeepEqual(files, want) {
		t.Fatalf("SHA256SUMS lists %v, want %v", files, want)
	}
}
