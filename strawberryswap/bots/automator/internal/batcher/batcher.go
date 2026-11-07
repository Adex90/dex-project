// Package batcher turns a pool of pending intents into ONE batch the circuit can prove: it chooses which intents
// to include, computes the single uniform clearing price, every user's payout and the NET flow, and emits the
// circuit witness. The arithmetic is a line-for-line mirror of circuits/batch_swap and circuits/test/batch.lib.mjs
// (cross-checked by golden vectors in testdata/).
package batcher

import (
	"errors"
	"fmt"
	"math/big"
	"sort"

	"strawberryswap/automator/internal/field"
	"strawberryswap/automator/internal/intent"
	"strawberryswap/automator/internal/notetree"
)

type Mode int

const (
	Swap   Mode = 1
	Refund Mode = 2
)

var (
	ErrNoBatch = errors.New("nothing to settle")
	big0       = big.NewInt(0)
	two112     = new(big.Int).Lsh(big.NewInt(1), 112)
	// DarkPool.MAX_RESERVE = 2^104 - 1: post-batch reserves must stay below it or settleBatch reverts.
	maxReserve = new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1), 104), big.NewInt(1))
)

// Pool is the on-chain AMM state the proof is bound to. X, Y are EFFECTIVE reserves (real + virtual) — the public
// inputs; R0, R1 are the REAL reserves, which bound what the pool can actually pay out.
type Pool struct{ X, Y, R0, R1 *big.Int }

// Candidate is a pending intent whose note is already in the tree.
type Candidate struct {
	Intent    *intent.Intent
	LeafIndex int
}

type Slot struct {
	Cand   Candidate
	Mode   Mode
	Payout *big.Int // swap: output value; refund: the full value
	Rem    *big.Int
}

type Plan struct {
	Slots                            []Slot // active slots only, swaps first then refunds
	PLo, PRem                        *big.Int
	A0, A1, O0, O1                   *big.Int // gross (private)
	NetIn0, NetIn1, NetOut0, NetOut1 *big.Int // net (public)
	Swaps, Refunds                   int
}

type Config struct {
	N        int // circuit slot count
	MinSwaps int // minimum swaps per batch (anonymity floor); refunds are exempt
}

// Cleared is the result of pricing a set of swaps.
type Cleared struct {
	PLo, PRem, PHi *big.Int
	Payouts, Rems  []*big.Int
	A0, A1, O0, O1 *big.Int
}

// Clear prices a set of swaps at the closed-form uniform price pLo = ⌊WAD·(Y+A1)/(X+A0)⌋ (token0 sellers) and
// pHi = pLo+1 (token1 sellers); both round against the user so K' >= K holds (proved in the circuit header).
func Clear(x, y *big.Int, swaps []*intent.Intent) Cleared {
	a0, a1 := new(big.Int), new(big.Int)
	for _, s := range swaps {
		if s.Dir == 0 {
			a0.Add(a0, s.Value)
		} else {
			a1.Add(a1, s.Value)
		}
	}
	d := new(big.Int).Add(x, a0)
	nn := new(big.Int).Add(y, a1)
	num := new(big.Int).Mul(field.WAD, nn)
	pLo, pRem := new(big.Int).DivMod(num, d, new(big.Int))
	pHi := new(big.Int).Add(pLo, big.NewInt(1))
	c := Cleared{PLo: pLo, PRem: pRem, PHi: pHi, A0: a0, A1: a1, O0: new(big.Int), O1: new(big.Int)}
	for _, s := range swaps {
		n, den := pLo, field.WAD
		if s.Dir == 1 {
			n, den = field.WAD, pHi
		}
		o, r := new(big.Int).DivMod(new(big.Int).Mul(s.Value, n), den, new(big.Int))
		c.Payouts = append(c.Payouts, o)
		c.Rems = append(c.Rems, r)
		if s.Dir == 0 {
			c.O1.Add(c.O1, o)
		} else {
			c.O0.Add(c.O0, o)
		}
	}
	return c
}

func split(d *big.Int) (in, out *big.Int) {
	switch d.Sign() {
	case 1:
		return new(big.Int).Set(d), new(big.Int)
	case -1:
		return new(big.Int), new(big.Int).Neg(d)
	}
	return new(big.Int), new(big.Int)
}

// Build selects the largest feasible batch from the candidates and prices it.
func Build(pool Pool, cands []Candidate, tNow uint64, cfg Config) (*Plan, error) {
	if cfg.N < 1 {
		return nil, errors.New("batcher: N must be >= 1")
	}
	var swaps, refunds []Candidate
	for _, c := range cands {
		if c.Intent.Deadline < tNow {
			refunds = append(refunds, c) // expired: the funds can only come back to the owner
		} else {
			swaps = append(swaps, c)
		}
	}
	order := func(s []Candidate) {
		sort.SliceStable(s, func(i, j int) bool { // earliest deadline first; commitment as a deterministic tie-break
			if s[i].Intent.Deadline != s[j].Intent.Deadline {
				return s[i].Intent.Deadline < s[j].Intent.Deadline
			}
			return s[i].Intent.Commitment().Cmp(s[j].Intent.Commitment()) < 0
		})
	}
	order(swaps)
	order(refunds)
	maxRefunds := cfg.N / 2 // refunds never move the price; cap them so they cannot crowd out swaps
	if maxRefunds < 1 {
		maxRefunds = 1
	}
	if len(refunds) > maxRefunds {
		refunds = refunds[:maxRefunds]
	}
	if room := cfg.N - len(refunds); len(swaps) > room {
		swaps = swaps[:room]
	}

	// Shrink until every included swap is satisfiable and the pool can actually settle it.
	for len(swaps) > 0 {
		ints := make([]*intent.Intent, len(swaps))
		for i, c := range swaps {
			ints[i] = c.Intent
		}
		cl := Clear(pool.X, pool.Y, ints)
		if k := violator(swaps, cl); k >= 0 {
			swaps = append(swaps[:k:k], swaps[k+1:]...)
			continue
		}
		in0, out0 := split(new(big.Int).Sub(cl.A0, cl.O0))
		in1, out1 := split(new(big.Int).Sub(cl.A1, cl.O1))
		// the pool pays only from REAL reserves, and must stay under the contract's reserve cap
		if out1.Cmp(pool.R1) > 0 || new(big.Int).Sub(new(big.Int).Add(pool.R1, in1), out1).Cmp(maxReserve) > 0 {
			if k := largest(swaps, 0); k >= 0 { // token0 sellers draw token1 out
				swaps = append(swaps[:k:k], swaps[k+1:]...)
				continue
			}
			if k := largest(swaps, 1); k >= 0 {
				swaps = append(swaps[:k:k], swaps[k+1:]...)
				continue
			}
		}
		if out0.Cmp(pool.R0) > 0 || new(big.Int).Sub(new(big.Int).Add(pool.R0, in0), out0).Cmp(maxReserve) > 0 {
			if k := largest(swaps, 1); k >= 0 {
				swaps = append(swaps[:k:k], swaps[k+1:]...)
				continue
			}
			if k := largest(swaps, 0); k >= 0 {
				swaps = append(swaps[:k:k], swaps[k+1:]...)
				continue
			}
		}
		break
	}
	if len(swaps) < cfg.MinSwaps { // anonymity floor: a thin batch would leak individual sizes by arithmetic
		swaps = nil
	}
	if len(swaps) == 0 && len(refunds) == 0 {
		return nil, ErrNoBatch
	}

	ints := make([]*intent.Intent, len(swaps))
	for i, c := range swaps {
		ints[i] = c.Intent
	}
	cl := Clear(pool.X, pool.Y, ints)
	p := &Plan{PLo: cl.PLo, PRem: cl.PRem, A0: cl.A0, A1: cl.A1, O0: cl.O0, O1: cl.O1, Swaps: len(swaps), Refunds: len(refunds)}
	for i, c := range swaps {
		p.Slots = append(p.Slots, Slot{Cand: c, Mode: Swap, Payout: cl.Payouts[i], Rem: cl.Rems[i]})
	}
	for _, c := range refunds {
		p.Slots = append(p.Slots, Slot{Cand: c, Mode: Refund, Payout: new(big.Int).Set(c.Intent.Value), Rem: new(big.Int)})
	}
	p.NetIn0, p.NetOut0 = split(new(big.Int).Sub(cl.A0, cl.O0))
	p.NetIn1, p.NetOut1 = split(new(big.Int).Sub(cl.A1, cl.O1))

	// Defence in depth: re-verify K' >= K in exact integers. A failure here is a bug, never a market condition.
	x2 := new(big.Int).Sub(new(big.Int).Add(pool.X, p.NetIn0), p.NetOut0)
	y2 := new(big.Int).Sub(new(big.Int).Add(pool.Y, p.NetIn1), p.NetOut1)
	if new(big.Int).Mul(x2, y2).Cmp(new(big.Int).Mul(pool.X, pool.Y)) < 0 {
		return nil, fmt.Errorf("batcher: internal error: K would decrease (X'=%s Y'=%s)", x2, y2)
	}
	return p, nil
}

// violator returns the index of the swap whose limit is violated worst, or -1.
func violator(swaps []Candidate, cl Cleared) int {
	worst, wn, wd := -1, (*big.Int)(nil), (*big.Int)(nil) // worst shortfall ratio = (minOut-o)/minOut
	for i, c := range swaps {
		o, m := cl.Payouts[i], c.Intent.MinOut
		bad := o.Cmp(m) < 0 || o.Cmp(two112) >= 0 || o.Sign() == 0
		if !bad {
			continue
		}
		short := new(big.Int).Sub(m, o)
		den := m
		if m.Sign() == 0 { // payout invalid for other reasons (zero or overflow): remove first
			short, den = big.NewInt(1), big.NewInt(1)
		}
		if worst < 0 || new(big.Int).Mul(short, wd).Cmp(new(big.Int).Mul(wn, den)) > 0 {
			worst, wn, wd = i, short, den
		}
	}
	return worst
}

func largest(swaps []Candidate, dir int) int {
	best := -1
	for i, c := range swaps {
		if c.Intent.Dir == dir && (best < 0 || c.Intent.Value.Cmp(swaps[best].Intent.Value) > 0) {
			best = i
		}
	}
	return best
}

// ---------------------------------------------------------------- outputs

// Output is the per-slot data that goes on-chain (opaque) plus what the owner needs to read it.
type Output struct {
	Nullifier, Commitment *big.Int
	AssetOut              int
	ValueOut              *big.Int
}

func (p *Plan) Outputs() []Output {
	outs := make([]Output, len(p.Slots))
	for i, s := range p.Slots {
		in := s.Cand.Intent
		o := Output{Nullifier: in.Nullifier(s.Cand.LeafIndex)}
		if s.Mode == Swap {
			o.AssetOut, o.ValueOut, o.Commitment = 1-in.Dir, s.Payout, in.SwapOutputCommitment(s.Payout)
		} else {
			o.AssetOut, o.ValueOut, o.Commitment = in.Dir, in.Value, in.RefundOutputCommitment()
		}
		outs[i] = o
	}
	return outs
}

// PublicInputs is the exact vector DarkPool.settleBatch passes to the batch verifier:
// [root, tNow, X, Y, in0, in1, out0, out1, nullifier[0..N), commitment[0..N)] with zero padding.
func (p *Plan) PublicInputs(root *big.Int, tNow uint64, pool Pool, n int) []*big.Int {
	pub := []*big.Int{root, new(big.Int).SetUint64(tNow), pool.X, pool.Y, p.NetIn0, p.NetIn1, p.NetOut0, p.NetOut1}
	nul := make([]*big.Int, n)
	com := make([]*big.Int, n)
	for i := range nul {
		nul[i], com[i] = new(big.Int), new(big.Int)
	}
	for i, o := range p.Outputs() {
		nul[i], com[i] = o.Nullifier, o.Commitment
	}
	return append(append(pub, nul...), com...)
}

// Witness builds the snarkjs input for circuits/batch_swap (decimal strings, as snarkjs expects).
func (p *Plan) Witness(tree *notetree.Tree, tNow uint64, pool Pool, n int) (map[string]any, error) {
	pub := p.PublicInputs(tree.Root(), tNow, pool, n)
	w := map[string]any{
		"root": field.Dec(pub[0]), "tNow": field.Dec(pub[1]), "X": field.Dec(pub[2]), "Y": field.Dec(pub[3]),
		"netIn0": field.Dec(pub[4]), "netIn1": field.Dec(pub[5]), "netOut0": field.Dec(pub[6]), "netOut1": field.Dec(pub[7]),
		"pLo": field.Dec(p.PLo), "pRem": field.Dec(p.PRem),
	}
	strs := func(v []*big.Int) []string {
		o := make([]string, len(v))
		for i := range v {
			o[i] = field.Dec(v[i])
		}
		return o
	}
	w["nullifier"], w["commitment"] = strs(pub[8:8+n]), strs(pub[8+n:8+2*n])

	cols := map[string][]string{}
	add := func(k string, v *big.Int) { cols[k] = append(cols[k], field.Dec(v)) }
	pathE := make([][]string, 0, n)
	pathI := make([][]string, 0, n)
	zeroPath := make([]string, notetree.Depth)
	for i := range zeroPath {
		zeroPath[i] = "0"
	}
	for i := 0; i < n; i++ {
		if i >= len(p.Slots) {
			for _, k := range []string{"isSwap", "isRefund", "dir", "value", "outValue", "rem", "destPubKey", "refundPubKey", "destBlinding", "minOut", "deadline"} {
				add(k, big0)
			}
			pathE, pathI = append(pathE, zeroPath), append(pathI, zeroPath)
			continue
		}
		s := p.Slots[i]
		in := s.Cand.Intent
		b := func(c bool) *big.Int {
			if c {
				return big.NewInt(1)
			}
			return big.NewInt(0)
		}
		add("isSwap", b(s.Mode == Swap))
		add("isRefund", b(s.Mode == Refund))
		add("dir", big.NewInt(int64(in.Dir)))
		add("value", in.Value)
		if s.Mode == Swap {
			add("outValue", s.Payout)
		} else {
			add("outValue", big0)
		}
		add("rem", s.Rem)
		add("destPubKey", in.DestPubKey)
		add("refundPubKey", in.RefundPubKey)
		add("destBlinding", in.DestBlinding)
		add("minOut", in.MinOut)
		add("deadline", new(big.Int).SetUint64(in.Deadline))
		el, bits, err := tree.Path(s.Cand.LeafIndex)
		if err != nil {
			return nil, err
		}
		pathE = append(pathE, strs(el))
		bs := make([]string, len(bits))
		for j, bit := range bits {
			bs[j] = fmt.Sprint(bit)
		}
		pathI = append(pathI, bs)
	}
	for k, v := range cols {
		w[k] = v
	}
	w["pathElements"], w["pathIndices"] = pathE, pathI
	return w, nil
}
