package chain

import (
	"bytes"
	"encoding/hex"
	"math/big"
	"os"
	"strings"
	"testing"
)

func rep(b byte) [32]byte {
	var x [32]byte
	for i := range x {
		x[i] = b
	}
	return x
}

// The hand-rolled encoder must be byte-identical to Foundry's `cast calldata` (golden produced by cast).
func TestEncodeSettleBatchMatchesCast(t *testing.T) {
	raw, err := os.ReadFile("../../testdata/settleBatch.calldata.hex")
	if err != nil {
		t.Fatal(err)
	}
	want, err := hex.DecodeString(strings.TrimPrefix(strings.TrimSpace(string(raw)), "0x"))
	if err != nil {
		t.Fatal(err)
	}
	b := &Batch{
		Proof: bytes.Repeat([]byte{0xab}, 256), Root: rep(0x11), TNow: 1800000000,
		Nullifiers: [][32]byte{rep(0x22), rep(0x33)}, Commitments: [][32]byte{rep(0x44), rep(0x55)},
		EncryptedNotes: [][]byte{{0xde, 0xad, 0xbe, 0xef}, bytes.Repeat([]byte{0xcd}, 61)},
		In0:            big.NewInt(7), In1: big.NewInt(0), Out0: big.NewInt(0), Out1: big.NewInt(9),
		X: big.NewInt(0).Mul(big.NewInt(3_000_000), big.NewInt(1e18)), Y: big.NewInt(0).Mul(big.NewInt(6_000_000), big.NewInt(1e18)),
	}
	got := EncodeSettleBatch(b)
	if !bytes.Equal(got, want) {
		t.Fatalf("calldata differs from cast (got %d bytes, want %d)", len(got), len(want))
	}
}

func TestEmptyNotesAndSelector(t *testing.T) {
	b := &Batch{Proof: make([]byte, 256), In0: big.NewInt(0), In1: big.NewInt(0), Out0: big.NewInt(0), Out1: big.NewInt(0), X: big.NewInt(1), Y: big.NewInt(1)}
	cd := EncodeSettleBatch(b)
	if len(cd)%32 != 4 {
		t.Fatal("calldata must be selector + whole words")
	}
	if Selector("transfer(address,uint256)") != [4]byte{0xa9, 0x05, 0x9c, 0xbb} {
		t.Fatal("selector helper wrong")
	}
}
