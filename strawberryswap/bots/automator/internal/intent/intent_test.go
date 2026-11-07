package intent

import (
	"encoding/hex"
	"encoding/json"
	"math/big"
	"os"
	"strings"
	"testing"
)

func valid() Intent {
	return Intent{Dir: 0, Value: big.NewInt(1000), DestPubKey: big.NewInt(11), RefundPubKey: big.NewInt(22), DestBlinding: big.NewInt(33), MinOut: big.NewInt(5), Deadline: 1_800_000_000}
}

func TestValidateRejectsMalformed(t *testing.T) {
	mut := []func(*Intent){
		func(i *Intent) { i.Dir = 2 }, func(i *Intent) { i.Value = big.NewInt(0) },
		func(i *Intent) { i.Value = new(big.Int).Lsh(big.NewInt(1), 112) }, func(i *Intent) { i.MinOut = new(big.Int).Lsh(big.NewInt(1), 112) },
		func(i *Intent) { i.DestPubKey = big.NewInt(0) }, func(i *Intent) { i.RefundPubKey = nil },
		func(i *Intent) { i.Deadline = 0 }, func(i *Intent) { i.DestBlinding = new(big.Int).Lsh(big.NewInt(1), 255) },
	}
	for k, m := range mut {
		in := valid()
		m(&in)
		if in.Validate() == nil {
			t.Fatalf("mutation %d accepted", k)
		}
	}
	ok := valid()
	if err := ok.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestJSONRejectsNumbersUnknownFieldsAndNegatives(t *testing.T) {
	good := `{"dir":1,"value":"1000","destPubKey":"0x0b","refundPubKey":"22","destBlinding":"33","minOut":"5","deadline":1800000000}`
	var in Intent
	if err := json.Unmarshal([]byte(good), &in); err != nil || in.DestPubKey.Int64() != 11 {
		t.Fatalf("good intent: %v", err)
	}
	for _, bad := range []string{
		strings.Replace(good, `"1000"`, `1000`, 1),                    // JSON number (precision loss)
		strings.Replace(good, `"dir":1,`, `"dir":1,"extra":true,`, 1), // unknown field
		strings.Replace(good, `"1000"`, `"-5"`, 1),                    // negative
		strings.Replace(good, `"1000"`, `"12abc"`, 1),
	} {
		if err := json.Unmarshal([]byte(bad), &in); err == nil {
			t.Fatalf("accepted malformed intent: %s", bad)
		}
	}
}

func TestCommitmentBindsEveryTerm(t *testing.T) {
	base := valid()
	c0 := base.Commitment()
	for name, m := range map[string]func(*Intent){
		"dir": func(i *Intent) { i.Dir = 1 }, "value": func(i *Intent) { i.Value = big.NewInt(1001) },
		"dest": func(i *Intent) { i.DestPubKey = big.NewInt(12) }, "refund": func(i *Intent) { i.RefundPubKey = big.NewInt(23) },
		"blind": func(i *Intent) { i.DestBlinding = big.NewInt(34) }, "minOut": func(i *Intent) { i.MinOut = big.NewInt(4) },
		"deadline": func(i *Intent) { i.Deadline++ },
	} {
		x := valid()
		m(&x)
		if x.Commitment().Cmp(c0) == 0 {
			t.Fatalf("changing %s does not change the commitment: the term is not bound", name)
		}
	}
	if base.Nullifier(3).Cmp(base.Nullifier(4)) == 0 {
		t.Fatal("nullifier must depend on the leaf index")
	}
}

func TestSealedOutputOnlyOpensWithTheOwnersKey(t *testing.T) {
	in := valid()
	blob, err := in.Seal(1, big.NewInt(1234567))
	if err != nil {
		t.Fatal(err)
	}
	asset, v, err := OpenOutput(in.DestBlinding, blob)
	if err != nil || asset != 1 || v.Int64() != 1234567 {
		t.Fatalf("owner could not open: %v %d %v", err, asset, v)
	}
	if _, _, err := OpenOutput(big.NewInt(99), blob); err == nil {
		t.Fatal("a different key must not open the blob")
	}
	b2, _ := in.Seal(1, big.NewInt(1234567))
	if string(b2) == string(blob) {
		t.Fatal("sealing must be randomised")
	}
}

// A sealed output produced by THIS code must stay openable by the browser wallet (frontend/src/zk/notes.ts
// `openSealedOutput`). testdata/sealed.golden.json is read by both test suites; regenerate with UPDATE_GOLDEN=1.
func TestSealedGoldenForTheBrowserWallet(t *testing.T) {
	const path = "../../testdata/sealed.golden.json"
	type golden struct {
		Blinding string `json:"blinding"`
		AssetOut int    `json:"assetOut"`
		Value    string `json:"value"`
		Blob     string `json:"blob"`
	}
	if os.Getenv("UPDATE_GOLDEN") != "" {
		in := valid()
		in.DestBlinding, _ = new(big.Int).SetString("123456789012345678901234567890123456789", 10)
		blob, err := in.Seal(1, big.NewInt(1999433000000000000))
		if err != nil {
			t.Fatal(err)
		}
		b, _ := json.MarshalIndent(golden{in.DestBlinding.String(), 1, "1999433000000000000", "0x" + hex.EncodeToString(blob)}, "", " ")
		if err := os.WriteFile(path, b, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("missing golden (run with UPDATE_GOLDEN=1): %v", err)
	}
	var g golden
	if err := json.Unmarshal(raw, &g); err != nil {
		t.Fatal(err)
	}
	blinding, _ := new(big.Int).SetString(g.Blinding, 10)
	blob, _ := hex.DecodeString(strings.TrimPrefix(g.Blob, "0x"))
	asset, v, err := OpenOutput(blinding, blob)
	if err != nil || asset != g.AssetOut || v.String() != g.Value {
		t.Fatalf("golden does not open: %v %d %v", err, asset, v)
	}
}
