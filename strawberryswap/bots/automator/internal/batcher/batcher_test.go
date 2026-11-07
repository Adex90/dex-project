package batcher

import (
	"math/big"
	"math/rand"
	"testing"

	"strawberryswap/automator/internal/intent"
)

var e18 = new(big.Int).Exp(big.NewInt(10), big.NewInt(18), nil)

func tok(n int64) *big.Int { return new(big.Int).Mul(big.NewInt(n), e18) }

func mkIntent(i int, dir int, value, minOut *big.Int, deadline uint64) Candidate {
	return Candidate{LeafIndex: i, Intent: &intent.Intent{
		Dir: dir, Value: value, MinOut: minOut, Deadline: deadline,
		DestPubKey: big.NewInt(int64(1000 + i)), RefundPubKey: big.NewInt(int64(2000 + i)), DestBlinding: big.NewInt(int64(3000 + i)),
	}}
}

func testPool() Pool {
	return Pool{X: tok(3_000_000), Y: tok(6_000_000), R0: tok(1_000_000), R1: tok(2_000_000)}
}

const now = 1_800_000_000

// K' >= K at the clearing price, for any batch (the invariant the on-chain K check relies on).
func TestClearingNeverDecreasesK(t *testing.T) {
	rng := rand.New(rand.NewSource(1))
	rnd := func(max int64) *big.Int {
		return new(big.Int).Mul(big.NewInt(rng.Int63n(max)+1), big.NewInt(rng.Int63n(1<<30)+1))
	}
	for it := 0; it < 5000; it++ {
		x, y := new(big.Int).Add(rnd(1<<50), big.NewInt(1_000_000)), new(big.Int).Add(rnd(1<<50), big.NewInt(1_000_000))
		var ints []*intent.Intent
		for k := 0; k < 1+rng.Intn(16); k++ {
			v := new(big.Int).Add(new(big.Int).Mod(rnd(1<<50), new(big.Int).Add(new(big.Int).Div(x, big.NewInt(4)), big.NewInt(1))), big.NewInt(1))
			ints = append(ints, &intent.Intent{Dir: rng.Intn(2), Value: v})
		}
		c := Clear(x, y, ints)
		x2 := new(big.Int).Sub(new(big.Int).Add(x, c.A0), c.O0)
		y2 := new(big.Int).Sub(new(big.Int).Add(y, c.A1), c.O1)
		if new(big.Int).Mul(x2, y2).Cmp(new(big.Int).Mul(x, y)) < 0 {
			t.Fatalf("K decreased for x=%s y=%s", x, y)
		}
	}
}

func TestEveryUserGetsTheSamePriceAndMeetsMinOut(t *testing.T) {
	cands := []Candidate{
		mkIntent(0, 0, tok(1000), tok(1900), now+100), mkIntent(1, 1, tok(800), tok(390), now+100),
		mkIntent(2, 0, tok(250), big.NewInt(0), now+100),
	}
	p, err := Build(testPool(), cands, now, Config{N: 4, MinSwaps: 1})
	if err != nil {
		t.Fatal(err)
	}
	if p.Swaps != 3 || p.Refunds != 0 {
		t.Fatalf("want 3 swaps, got %d/%d", p.Swaps, p.Refunds)
	}
	for _, s := range p.Slots {
		if s.Payout.Cmp(s.Cand.Intent.MinOut) < 0 {
			t.Fatal("minOut violated")
		}
	}
	// token0 sellers: payout == floor(value * pLo / WAD): identical price for all of them
	for _, s := range p.Slots {
		if s.Cand.Intent.Dir == 0 {
			want := new(big.Int).Div(new(big.Int).Mul(s.Cand.Intent.Value, p.PLo), e18)
			if s.Payout.Cmp(want) != 0 {
				t.Fatal("non-uniform price")
			}
		}
	}
}

func TestInfeasibleIntentIsDroppedNotTheBatch(t *testing.T) {
	cands := []Candidate{
		mkIntent(0, 0, tok(1000), tok(1900), now+100),
		mkIntent(1, 0, tok(100), tok(250), now+100), // wants price 2.5; curve gives ~2.0
		mkIntent(2, 1, tok(500), tok(240), now+100),
	}
	p, err := Build(testPool(), cands, now, Config{N: 4, MinSwaps: 1})
	if err != nil {
		t.Fatal(err)
	}
	for _, s := range p.Slots {
		if s.Cand.LeafIndex == 1 {
			t.Fatal("infeasible intent must not be included")
		}
	}
	if p.Swaps != 2 {
		t.Fatalf("want the 2 feasible swaps, got %d", p.Swaps)
	}
}

func TestAnonymityFloor(t *testing.T) {
	one := []Candidate{mkIntent(0, 0, tok(1000), big.NewInt(0), now+100)}
	if _, err := Build(testPool(), one, now, Config{N: 4, MinSwaps: 2}); err != ErrNoBatch {
		t.Fatalf("a single swap below the floor must not settle, got %v", err)
	}
	// but an expired intent can always be refunded (it reveals nothing and moves no price)
	exp := append(one, mkIntent(1, 0, tok(5), big.NewInt(0), now-1))
	p, err := Build(testPool(), exp, now, Config{N: 4, MinSwaps: 2})
	if err != nil {
		t.Fatal(err)
	}
	if p.Swaps != 0 || p.Refunds != 1 || p.NetIn0.Sign() != 0 || p.NetOut1.Sign() != 0 {
		t.Fatalf("refund-only batch expected, got swaps=%d refunds=%d", p.Swaps, p.Refunds)
	}
}

func TestPoolCannotPayMoreThanItsRealReserve(t *testing.T) {
	pool := Pool{X: tok(3_000_000), Y: tok(6_000_000), R0: tok(1_000_000), R1: tok(50)} // almost no real token1
	cands := []Candidate{mkIntent(0, 0, tok(1000), big.NewInt(0), now+100), mkIntent(1, 0, tok(10), big.NewInt(0), now+100)}
	p, err := Build(pool, cands, now, Config{N: 4, MinSwaps: 1})
	if err != nil {
		t.Fatal(err)
	}
	if p.NetOut1.Cmp(pool.R1) > 0 {
		t.Fatalf("plan pays out %s token1 but the pool only has %s", p.NetOut1, pool.R1)
	}
}

func TestRefundsAreCappedAndNeverMovePrice(t *testing.T) {
	var cands []Candidate
	for i := 0; i < 6; i++ {
		cands = append(cands, mkIntent(i, i%2, tok(10), big.NewInt(0), now-5))
	}
	cands = append(cands, mkIntent(10, 0, tok(100), big.NewInt(0), now+50), mkIntent(11, 1, tok(100), big.NewInt(0), now+50))
	p, err := Build(testPool(), cands, now, Config{N: 4, MinSwaps: 2})
	if err != nil {
		t.Fatal(err)
	}
	if p.Refunds > 2 || p.Swaps != 2 || len(p.Slots) > 4 {
		t.Fatalf("refunds=%d swaps=%d slots=%d: cap violated", p.Refunds, p.Swaps, len(p.Slots))
	}
}

func TestPublicInputsLayout(t *testing.T) {
	cands := []Candidate{mkIntent(0, 0, tok(100), big.NewInt(0), now+100), mkIntent(1, 1, tok(100), big.NewInt(0), now+100)}
	pool := testPool()
	p, _ := Build(pool, cands, now, Config{N: 4, MinSwaps: 1})
	pub := p.PublicInputs(big.NewInt(7), now, pool, 4)
	if len(pub) != 8+2*4 {
		t.Fatalf("want 16 public inputs, got %d", len(pub))
	}
	outs := p.Outputs()
	if pub[8].Cmp(outs[0].Nullifier) != 0 || pub[12].Cmp(outs[0].Commitment) != 0 || pub[10].Sign() != 0 || pub[14].Sign() != 0 {
		t.Fatal("nullifiers/commitments must be at [8..12) / [12..16) and zero-padded")
	}
}
