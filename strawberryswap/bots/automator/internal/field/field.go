// Package field holds BN254 scalar-field helpers shared by every component that must agree bit-for-bit with the
// circuits and with contracts/src/privacy/NoteTree.sol.
package field

import (
	"errors"
	"fmt"
	"math/big"

	"github.com/iden3/go-iden3-crypto/poseidon"
	"golang.org/x/crypto/sha3"
)

// Prime is the BN254 scalar field order (the circuits' native field).
var Prime, _ = new(big.Int).SetString("21888242871839275222246405745257275088548364400416034343698204186575808495617", 10)

// WAD is the 1e18 fixed-point unit used for prices.
var WAD = new(big.Int).Exp(big.NewInt(10), big.NewInt(18), nil)

var ErrNotInField = errors.New("value is not a reduced field element")

// Keccak256 is the Ethereum (legacy) Keccak-256.
func Keccak256(data ...[]byte) [32]byte {
	h := sha3.NewLegacyKeccak256()
	for _, d := range data {
		h.Write(d)
	}
	var out [32]byte
	copy(out[:], h.Sum(nil))
	return out
}

// LabelToField = uint256(keccak256(label)) mod p — how every nothing-up-my-sleeve constant is derived.
func LabelToField(label string) *big.Int {
	k := Keccak256([]byte(label))
	return new(big.Int).Mod(new(big.Int).SetBytes(k[:]), Prime)
}

// InField reports 0 <= x < p.
func InField(x *big.Int) bool { return x != nil && x.Sign() >= 0 && x.Cmp(Prime) < 0 }

// Hash is Poseidon over BN254 (circomlib-compatible). Every input must already be reduced.
func Hash(in ...*big.Int) (*big.Int, error) {
	for _, x := range in {
		if !InField(x) {
			return nil, fmt.Errorf("poseidon input: %w", ErrNotInField)
		}
	}
	return poseidon.Hash(in)
}

// MustHash panics on invalid input; for values the caller has already validated.
func MustHash(in ...*big.Int) *big.Int {
	h, err := Hash(in...)
	if err != nil {
		panic(err)
	}
	return h
}

// Bytes32 returns x as a 32-byte big-endian array.
func Bytes32(x *big.Int) [32]byte {
	var out [32]byte
	x.FillBytes(out[:])
	return out
}

// Dec renders a big.Int as a decimal string (the format snarkjs input files use).
func Dec(x *big.Int) string { return x.String() }

// Bytes32Slice returns x as a 32-byte big-endian slice.
func Bytes32Slice(x *big.Int) []byte {
	b := Bytes32(x)
	return b[:]
}
