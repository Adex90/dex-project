// Package notetree mirrors contracts/src/privacy/NoteTree.sol: an incremental binary Merkle tree over BN254
// Poseidon(2), depth 20, with zero value keccak256("StrawberrySwap.NoteTree.zero") mod p. The Automator keeps a
// local mirror (rebuilt from NoteInserted events) so it can produce Merkle paths for the batch circuit.
package notetree

import (
	"errors"
	"fmt"
	"math/big"
	"sync"

	"strawberryswap/automator/internal/field"
)

const Depth = 20

var ErrFull = errors.New("note tree is full")

type Tree struct {
	mu    sync.RWMutex
	depth int
	zeros []*big.Int   // zeros[l] = root of an empty subtree of height l
	nodes [][]*big.Int // nodes[l][i] = node i at height l (height 0 = leaves); only populated prefixes
	index map[string]int
}

// New returns an empty tree of the given depth (use Depth in production; smaller depths are for tests).
func New(depth int) *Tree {
	t := &Tree{depth: depth, zeros: make([]*big.Int, depth+1), nodes: make([][]*big.Int, depth+1), index: map[string]int{}}
	t.zeros[0] = field.LabelToField("StrawberrySwap.NoteTree.zero")
	for l := 1; l <= depth; l++ {
		t.zeros[l] = field.MustHash(t.zeros[l-1], t.zeros[l-1])
	}
	return t
}

func (t *Tree) node(l, i int) *big.Int {
	if i < len(t.nodes[l]) {
		return t.nodes[l][i]
	}
	return t.zeros[l]
}

// Insert appends a leaf and returns its index. Duplicate commitments are allowed (as on-chain) but IndexOf
// then reports the FIRST occurrence.
func (t *Tree) Insert(leaf *big.Int) (int, error) {
	if !field.InField(leaf) {
		return 0, field.ErrNotInField
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	idx := len(t.nodes[0])
	if idx >= 1<<t.depth {
		return 0, ErrFull
	}
	t.nodes[0] = append(t.nodes[0], new(big.Int).Set(leaf))
	if _, ok := t.index[leaf.String()]; !ok {
		t.index[leaf.String()] = idx
	}
	cur := idx
	for l := 0; l < t.depth; l++ {
		left, right := cur&^1, cur|1
		h := field.MustHash(t.node(l, left), t.node(l, right))
		parent := cur >> 1
		if parent < len(t.nodes[l+1]) {
			t.nodes[l+1][parent] = h
		} else {
			t.nodes[l+1] = append(t.nodes[l+1], h)
		}
		cur = parent
	}
	return idx, nil
}

func (t *Tree) Root() *big.Int {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return new(big.Int).Set(t.node(t.depth, 0))
}

func (t *Tree) Len() int {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return len(t.nodes[0])
}

// IndexOf returns the leaf index of a commitment.
func (t *Tree) IndexOf(commitment *big.Int) (int, bool) {
	t.mu.RLock()
	defer t.mu.RUnlock()
	i, ok := t.index[commitment.String()]
	return i, ok
}

// Path returns the sibling hashes and the index bits (bit l = 1 ⇒ the running node is a RIGHT child at level l),
// exactly the witness the circuits' MerkleInclusion template expects.
func (t *Tree) Path(index int) (elements []*big.Int, bits []int, err error) {
	t.mu.RLock()
	defer t.mu.RUnlock()
	if index < 0 || index >= len(t.nodes[0]) {
		return nil, nil, fmt.Errorf("leaf index %d out of range", index)
	}
	cur := index
	for l := 0; l < t.depth; l++ {
		elements = append(elements, new(big.Int).Set(t.node(l, cur^1)))
		bits = append(bits, cur&1)
		cur >>= 1
	}
	return elements, bits, nil
}
