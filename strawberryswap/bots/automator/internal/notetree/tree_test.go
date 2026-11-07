package notetree

import (
	"math/big"
	"testing"

	"strawberryswap/automator/internal/field"
)

// Recompute a root from a leaf and its path using only the public rule (the contract's / circuit's rule).
func rootFromPath(leaf *big.Int, el []*big.Int, bits []int) *big.Int {
	cur := leaf
	for i := range el {
		if bits[i] == 0 {
			cur = field.MustHash(cur, el[i])
		} else {
			cur = field.MustHash(el[i], cur)
		}
	}
	return cur
}

func TestPathsVerifyAgainstRoot(t *testing.T) {
	tr := New(Depth)
	var leaves []*big.Int
	for i := 0; i < 37; i++ {
		l := field.MustHash(big.NewInt(int64(i)), big.NewInt(99))
		leaves = append(leaves, l)
		if idx, err := tr.Insert(l); err != nil || idx != i {
			t.Fatal(idx, err)
		}
	}
	for i, l := range leaves {
		el, bits, err := tr.Path(i)
		if err != nil {
			t.Fatal(err)
		}
		if rootFromPath(l, el, bits).Cmp(tr.Root()) != 0 {
			t.Fatalf("path for leaf %d does not hash to the root", i)
		}
	}
	if got, ok := tr.IndexOf(leaves[5]); !ok || got != 5 {
		t.Fatal("IndexOf")
	}
}

func TestEmptyRootAndRejects(t *testing.T) {
	tr := New(Depth)
	want := field.MustHash(tr.zeros[Depth-1], tr.zeros[Depth-1])
	if tr.Root().Cmp(want) != 0 {
		t.Fatal("empty root")
	}
	if _, err := tr.Insert(field.Prime); err == nil {
		t.Fatal("must reject non-field leaves")
	}
	if _, _, err := tr.Path(0); err == nil {
		t.Fatal("path of a missing leaf must error")
	}
	small := New(2)
	for i := 0; i < 4; i++ {
		if _, err := small.Insert(big.NewInt(int64(i + 1))); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := small.Insert(big.NewInt(9)); err != ErrFull {
		t.Fatal("full tree must refuse")
	}
}
