// Package chain talks to the EVM: a minimal ABI encoder for DarkPool.settleBatch (checked against Foundry's `cast`),
// a JSON-RPC reader, and a remote-signer client. No private key ever lives in this process.
package chain

import (
	"encoding/binary"
	"math/big"

	"strawberryswap/automator/internal/field"
)

// Batch mirrors DarkPool.Batch.
type Batch struct {
	Proof                []byte
	Root                 [32]byte
	TNow                 uint64
	Nullifiers           [][32]byte
	Commitments          [][32]byte
	EncryptedNotes       [][]byte
	In0, In1, Out0, Out1 *big.Int
}

// Selector returns the first 4 bytes of keccak256(signature).
func Selector(sig string) [4]byte {
	k := field.Keccak256([]byte(sig))
	var s [4]byte
	copy(s[:], k[:4])
	return s
}

const settleBatchSig = "settleBatch((bytes,bytes32,uint64,bytes32[],bytes32[],bytes[],uint256,uint256,uint256,uint256))"

func word(v *big.Int) []byte { b := make([]byte, 32); v.FillBytes(b); return b }
func wordU(v uint64) []byte  { b := make([]byte, 32); binary.BigEndian.PutUint64(b[24:], v); return b }

func pad32(n int) int { return (n + 31) / 32 * 32 }

func encBytes(b []byte) []byte {
	out := append(wordU(uint64(len(b))), b...)
	return append(out, make([]byte, pad32(len(b))-len(b))...)
}

func encB32Array(a [][32]byte) []byte {
	out := wordU(uint64(len(a)))
	for _, x := range a {
		out = append(out, x[:]...)
	}
	return out
}

func encBytesArray(a [][]byte) []byte {
	out := wordU(uint64(len(a)))
	var tails []byte
	off := 32 * len(a) // offsets are relative to the start of the element area (just after the length word)
	for _, x := range a {
		out = append(out, wordU(uint64(off+len(tails)))...)
		tails = append(tails, encBytes(x)...)
	}
	return append(out, tails...)
}

// EncodeSettleBatch returns the calldata for settleBatch(Batch): selector ‖ offset(0x20) ‖ tuple. The tuple has
// four dynamic members (proof, nullifiers, commitments, encryptedNotes) whose offsets are relative to the tuple start.
func EncodeSettleBatch(b *Batch) []byte {
	proof, nul, com, enc := encBytes(b.Proof), encB32Array(b.Nullifiers), encB32Array(b.Commitments), encBytesArray(b.EncryptedNotes)
	const headWords = 10
	off := headWords * 32
	offProof, offNul := off, off+len(proof)
	offCom, offEnc := offNul+len(nul), offNul+len(nul)+len(com)

	tuple := wordU(uint64(offProof))
	tuple = append(tuple, b.Root[:]...)
	tuple = append(tuple, wordU(b.TNow)...)
	tuple = append(tuple, wordU(uint64(offNul))...)
	tuple = append(tuple, wordU(uint64(offCom))...)
	tuple = append(tuple, wordU(uint64(offEnc))...)
	for _, v := range []*big.Int{b.In0, b.In1, b.Out0, b.Out1} {
		tuple = append(tuple, word(v)...)
	}
	tuple = append(tuple, proof...)
	tuple = append(tuple, nul...)
	tuple = append(tuple, com...)
	tuple = append(tuple, enc...)

	sel := Selector(settleBatchSig)
	out := append([]byte{}, sel[:]...)
	out = append(out, wordU(32)...)
	return append(out, tuple...)
}
