package chain

import (
	"encoding/binary"
	"math/big"
)

// Tx1559 is an EIP-1559 transaction with no value and no access list: all the Automator sends (DarkPool.settleBatch).
// The Automator has no Ethereum library, so the signing payload is RLP-encoded here, canonically: the vault decodes
// it, re-encodes it and refuses any byte that differs. testdata/eip1559.vault.json pins one payload, which the vault's
// own tests check against alloy's encoding.
type Tx1559 struct {
	ChainID              uint64
	Nonce                uint64
	MaxPriorityFeePerGas *big.Int
	MaxFeePerGas         *big.Int
	GasLimit             uint64
	To                   [20]byte
	Data                 []byte
}

// SigningPayload is 0x02 ‖ rlp([chainId, nonce, maxPriorityFeePerGas, maxFeePerGas, gasLimit, to, value, data,
// accessList]), with value 0 and an empty access list: what the vault signs (its keccak256 is the signing hash).
func (t *Tx1559) SigningPayload() []byte {
	u := func(v uint64) []byte { return rlpUint(new(big.Int).SetUint64(v)) }
	list := rlpList(
		u(t.ChainID),
		u(t.Nonce),
		rlpUint(t.MaxPriorityFeePerGas),
		rlpUint(t.MaxFeePerGas),
		u(t.GasLimit),
		rlpString(t.To[:]),
		rlpUint(new(big.Int)),
		rlpString(t.Data),
		rlpList(),
	)
	return append([]byte{0x02}, list...)
}

// rlpHeader is the prefix of a string (short = 0x80) or a list (short = 0xc0) of n bytes.
func rlpHeader(n int, short byte) []byte {
	if n < 56 {
		return []byte{short + byte(n)}
	}
	var be [8]byte
	binary.BigEndian.PutUint64(be[:], uint64(n))
	i := 0
	for i < 7 && be[i] == 0 {
		i++
	}
	return append([]byte{short + 55 + byte(8-i)}, be[i:]...)
}

// rlpString encodes a byte string: a single byte below 0x80 is itself.
func rlpString(b []byte) []byte {
	if len(b) == 1 && b[0] < 0x80 {
		return []byte{b[0]}
	}
	return append(rlpHeader(len(b), 0x80), b...)
}

// rlpUint encodes a non-negative integer as its minimal big-endian bytes (zero is the empty string, 0x80).
func rlpUint(v *big.Int) []byte {
	return rlpString(v.Bytes())
}

func rlpList(items ...[]byte) []byte {
	n := 0
	for _, item := range items {
		n += len(item)
	}
	out := rlpHeader(n, 0xc0)
	for _, item := range items {
		out = append(out, item...)
	}
	return out
}
