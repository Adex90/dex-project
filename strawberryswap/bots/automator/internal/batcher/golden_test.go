package batcher

import (
	"encoding/json"
	"math/big"
	"os"
	"reflect"
	"testing"

	"strawberryswap/automator/internal/field"
	"strawberryswap/automator/internal/intent"
	"strawberryswap/automator/internal/notetree"
)

type golden struct {
	Constants struct{ BatchPubKey, BatchNK, Zero string }
	Poseidon  []struct {
		In  []string
		Out string
	}
	Tree struct {
		Leaves []string
		Root   string
		Path3  []string
		Bits3  []int
	}
	Intents []struct {
		Label      string
		Index      int
		JSON       json.RawMessage
		Commitment string
		Nullifier  string
	}
	Pool     struct{ X, Y, R0, R1 string }
	TNow     uint64
	N        int
	Clearing struct {
		PLo, PRem                        string
		Payouts                          []string
		NetIn0, NetIn1, NetOut0, NetOut1 string
	}
	Order   []string
	Witness map[string]any
}

func dec(t *testing.T, s string) *big.Int {
	t.Helper()
	v, ok := new(big.Int).SetString(s, 10)
	if !ok {
		t.Fatalf("bad integer %q", s)
	}
	return v
}

func load(t *testing.T) golden {
	t.Helper()
	b, err := os.ReadFile("../../testdata/golden.json")
	if err != nil {
		t.Fatal(err)
	}
	var g golden
	if err := json.Unmarshal(b, &g); err != nil {
		t.Fatal(err)
	}
	return g
}

// The Go implementation must reproduce the JS reference (which the circuit tests are written against) EXACTLY.
func TestCrossLanguageGolden(t *testing.T) {
	g := load(t)

	if field.LabelToField("StrawberrySwap.batch.pubkey").String() != g.Constants.BatchPubKey ||
		field.LabelToField("StrawberrySwap.batch.nullifierKey").String() != g.Constants.BatchNK ||
		field.LabelToField("StrawberrySwap.NoteTree.zero").String() != g.Constants.Zero {
		t.Fatal("nothing-up-my-sleeve constants differ from the circuit's")
	}
	for _, p := range g.Poseidon {
		var in []*big.Int
		for _, s := range p.In {
			in = append(in, dec(t, s))
		}
		if got := field.MustHash(in...).String(); got != p.Out {
			t.Fatalf("poseidon(%v) = %s, want %s", p.In, got, p.Out)
		}
	}

	tree := notetree.New(notetree.Depth)
	for _, l := range g.Tree.Leaves {
		if _, err := tree.Insert(dec(t, l)); err != nil {
			t.Fatal(err)
		}
	}
	if tree.Root().String() != g.Tree.Root {
		t.Fatalf("tree root %s != %s", tree.Root(), g.Tree.Root)
	}
	el, bits, err := tree.Path(3)
	if err != nil {
		t.Fatal(err)
	}
	for i := range el {
		if el[i].String() != g.Tree.Path3[i] || bits[i] != g.Tree.Bits3[i] {
			t.Fatalf("merkle path differs at level %d", i)
		}
	}

	cands := map[string]Candidate{}
	var all []Candidate
	for _, it := range g.Intents {
		var in intent.Intent
		if err := json.Unmarshal(it.JSON, &in); err != nil {
			t.Fatal(err)
		}
		if err := in.Validate(); err != nil {
			t.Fatal(err)
		}
		if in.Commitment().String() != it.Commitment {
			t.Fatalf("%s: commitment differs", it.Label)
		}
		if in.Nullifier(it.Index).String() != it.Nullifier {
			t.Fatalf("%s: nullifier differs", it.Label)
		}
		c := Candidate{Intent: &in, LeafIndex: it.Index}
		cands[it.Label] = c
		all = append(all, c)
	}

	pool := Pool{X: dec(t, g.Pool.X), Y: dec(t, g.Pool.Y), R0: dec(t, g.Pool.R0), R1: dec(t, g.Pool.R1)}
	plan, err := Build(pool, all, g.TNow, Config{N: g.N, MinSwaps: 1})
	if err != nil {
		t.Fatal(err)
	}
	if plan.PLo.String() != g.Clearing.PLo || plan.PRem.String() != g.Clearing.PRem {
		t.Fatalf("price differs: %s/%s want %s/%s", plan.PLo, plan.PRem, g.Clearing.PLo, g.Clearing.PRem)
	}
	for i, p := range g.Clearing.Payouts {
		if plan.Slots[i].Payout.String() != p {
			t.Fatalf("payout %d: %s want %s", i, plan.Slots[i].Payout, p)
		}
	}
	for name, pair := range map[string][2]string{
		"netIn0": {plan.NetIn0.String(), g.Clearing.NetIn0}, "netIn1": {plan.NetIn1.String(), g.Clearing.NetIn1},
		"netOut0": {plan.NetOut0.String(), g.Clearing.NetOut0}, "netOut1": {plan.NetOut1.String(), g.Clearing.NetOut1},
	} {
		if pair[0] != pair[1] {
			t.Fatalf("%s: %s want %s", name, pair[0], pair[1])
		}
	}
	for i, label := range g.Order {
		if plan.Slots[i].Cand.Intent.Commitment().Cmp(cands[label].Intent.Commitment()) != 0 {
			t.Fatalf("slot %d order: expected intent %q", i, label)
		}
	}

	// The whole circuit witness, key by key, as snarkjs would read it.
	w, err := plan.Witness(tree, g.TNow, pool, g.N)
	if err != nil {
		t.Fatal(err)
	}
	gotJSON, _ := json.Marshal(w)
	var got map[string]any
	_ = json.Unmarshal(gotJSON, &got)
	for k, want := range g.Witness {
		if !reflect.DeepEqual(got[k], want) {
			t.Fatalf("witness[%q] differs\n go : %v\n js : %v", k, got[k], want)
		}
	}
	if len(got) != len(g.Witness) {
		t.Fatalf("witness has %d keys, JS reference %d", len(got), len(g.Witness))
	}
}
