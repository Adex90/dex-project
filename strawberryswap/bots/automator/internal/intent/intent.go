// Package intent defines the shielded swap intent, its note commitments and nullifier, and the sealed output
// blob only the intent's owner can open. Mirrors circuits/batch_swap (header) and circuits/test/batch.lib.mjs.
package intent

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"

	"strawberryswap/automator/internal/field"
)

// Nothing-up-my-sleeve constant shared with the circuit (no known Poseidon preimage).
var BatchPubKey = field.LabelToField("StrawberrySwap.batch.pubkey")

var (
	two112 = new(big.Int).Lsh(big.NewInt(1), 112)
	two64  = new(big.Int).Lsh(big.NewInt(1), 64)
)

// Intent is what a user hands the Automator: the OPENING of an intent note they already created on-chain with a
// JoinSplit. It contains no private key — the Automator can neither spend the note elsewhere nor redirect it.
type Intent struct {
	Dir          int      `json:"dir"`          // asset sold into the pool: 0 = token0, 1 = token1
	Value        *big.Int `json:"value"`        // amount sold
	DestPubKey   *big.Int `json:"destPubKey"`   // receives the payout
	RefundPubKey *big.Int `json:"refundPubKey"` // receives the refund after the deadline
	DestBlinding *big.Int `json:"destBlinding"` // blinding of the output note; also the key sealing the result
	MinOut       *big.Int `json:"minOut"`       // slippage bound
	Deadline     uint64   `json:"deadline"`     // unix seconds
}

var ErrInvalid = errors.New("invalid intent")

// Validate checks every range the circuit enforces, so a malformed intent is rejected before it can poison a batch.
func (i *Intent) Validate() error {
	if i.Dir != 0 && i.Dir != 1 {
		return fmt.Errorf("%w: dir must be 0 or 1", ErrInvalid)
	}
	for name, v := range map[string]*big.Int{"value": i.Value, "destPubKey": i.DestPubKey, "refundPubKey": i.RefundPubKey, "destBlinding": i.DestBlinding, "minOut": i.MinOut} {
		if v == nil || !field.InField(v) {
			return fmt.Errorf("%w: %s missing or not a field element", ErrInvalid, name)
		}
	}
	if i.Value.Sign() <= 0 || i.Value.Cmp(two112) >= 0 {
		return fmt.Errorf("%w: value must be in (0, 2^112)", ErrInvalid)
	}
	if i.MinOut.Cmp(two112) >= 0 {
		return fmt.Errorf("%w: minOut must be < 2^112", ErrInvalid)
	}
	if i.DestPubKey.Sign() == 0 || i.RefundPubKey.Sign() == 0 {
		return fmt.Errorf("%w: destination keys must be non-zero", ErrInvalid)
	}
	if i.Deadline == 0 || new(big.Int).SetUint64(i.Deadline).Cmp(two64) >= 0 {
		return fmt.Errorf("%w: deadline out of range", ErrInvalid)
	}
	return nil
}

// Blinding = Poseidon(destPubKey, refundPubKey, destBlinding, minOut, deadline): the intent's terms, committed
// inside the note so they cannot be changed after the user created it.
func (i *Intent) Blinding() *big.Int {
	return field.MustHash(i.DestPubKey, i.RefundPubKey, i.DestBlinding, i.MinOut, new(big.Int).SetUint64(i.Deadline))
}

// Commitment of the intent note: Poseidon(assetIn, value, BATCH_PUBKEY, blinding).
func (i *Intent) Commitment() *big.Int {
	return field.MustHash(big.NewInt(int64(i.Dir)), i.Value, BatchPubKey, i.Blinding())
}

// Nullifier of the intent note at a given leaf: Poseidon(commitment, leafIndex, destBlinding). The key is the
// intent's own secret (known to the user and to whoever holds the opening), so an observer cannot link a spent intent
// to the transaction that created it. A public key made every batch slot linkable (Phase-8 audit H-7).
func (i *Intent) Nullifier(leafIndex int) *big.Int {
	return field.MustHash(i.Commitment(), big.NewInt(int64(leafIndex)), i.DestBlinding)
}

// SwapOutputCommitment is the note the user receives on a swap: (assetOut, payout, destPubKey, destBlinding).
func (i *Intent) SwapOutputCommitment(payout *big.Int) *big.Int {
	return field.MustHash(big.NewInt(int64(1-i.Dir)), payout, i.DestPubKey, i.DestBlinding)
}

// RefundOutputCommitment is the note returned after the deadline: same asset, full value, refundPubKey.
func (i *Intent) RefundOutputCommitment() *big.Int {
	return field.MustHash(big.NewInt(int64(i.Dir)), i.Value, i.RefundPubKey, i.DestBlinding)
}

// UnmarshalJSON accepts decimal strings or 0x-hex strings for the big integers (JSON numbers are rejected: they
// silently lose precision above 2^53).
func (i *Intent) UnmarshalJSON(b []byte) error {
	var raw struct {
		Dir          int    `json:"dir"`
		Value        string `json:"value"`
		DestPubKey   string `json:"destPubKey"`
		RefundPubKey string `json:"refundPubKey"`
		DestBlinding string `json:"destBlinding"`
		MinOut       string `json:"minOut"`
		Deadline     uint64 `json:"deadline"`
	}
	dec := json.NewDecoder(bytesReader(b))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&raw); err != nil {
		return fmt.Errorf("%w: %v", ErrInvalid, err)
	}
	parse := func(s string) (*big.Int, error) {
		v, ok := new(big.Int).SetString(s, 0)
		if !ok || v.Sign() < 0 {
			return nil, fmt.Errorf("%w: bad integer %q", ErrInvalid, truncate(s))
		}
		return v, nil
	}
	var err error
	i.Dir, i.Deadline = raw.Dir, raw.Deadline
	if i.Value, err = parse(raw.Value); err != nil {
		return err
	}
	if i.DestPubKey, err = parse(raw.DestPubKey); err != nil {
		return err
	}
	if i.RefundPubKey, err = parse(raw.RefundPubKey); err != nil {
		return err
	}
	if i.DestBlinding, err = parse(raw.DestBlinding); err != nil {
		return err
	}
	if i.MinOut, err = parse(raw.MinOut); err != nil {
		return err
	}
	return nil
}

func truncate(s string) string {
	if len(s) > 12 {
		return s[:12] + "…"
	}
	return s
}

// ---------------------------------------------------------------- sealed output (stealth delivery of the result)

// Seal encrypts the output opening for the intent's owner with AES-256-GCM under a key derived from DestBlinding —
// a secret only the owner and the Automator know. It is published as the note's `encryptedNote` on-chain; the
// owner finds it by scanning NoteInserted events and trying their key. Everyone else sees random-looking bytes.
func (i *Intent) Seal(assetOut int, value *big.Int) ([]byte, error) {
	aead, err := newAEAD(i.DestBlinding)
	if err != nil {
		return nil, err
	}
	nonce := make([]byte, aead.NonceSize())
	if _, err := rand.Read(nonce); err != nil {
		return nil, err
	}
	pt := append([]byte{byte(assetOut)}, field.Bytes32Slice(value)...)
	return aead.Seal(nonce, nonce, pt, nil), nil
}

// OpenOutput is the owner-side inverse of Seal.
func OpenOutput(destBlinding *big.Int, blob []byte) (assetOut int, value *big.Int, err error) {
	aead, err := newAEAD(destBlinding)
	if err != nil {
		return 0, nil, err
	}
	if len(blob) < aead.NonceSize() {
		return 0, nil, errors.New("blob too short")
	}
	pt, err := aead.Open(nil, blob[:aead.NonceSize()], blob[aead.NonceSize():], nil)
	if err != nil || len(pt) != 33 {
		return 0, nil, errors.New("not sealed for this key")
	}
	return int(pt[0]), new(big.Int).SetBytes(pt[1:]), nil
}

func newAEAD(blinding *big.Int) (cipher.AEAD, error) {
	k := sha256.Sum256(append([]byte("StrawberrySwap.outnote.v1"), field.Bytes32Slice(blinding)...))
	blk, err := aes.NewCipher(k[:])
	if err != nil {
		return nil, err
	}
	return cipher.NewGCM(blk)
}
