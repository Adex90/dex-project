package service

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"math/big"
	"testing"
	"time"

	"strawberryswap/automator/internal/chain"
	"strawberryswap/automator/internal/field"
	"strawberryswap/automator/internal/intent"
	"strawberryswap/automator/internal/notetree"
	"strawberryswap/automator/internal/prover"
)

var e18 = new(big.Int).Exp(big.NewInt(10), big.NewInt(18), nil)

func tok(n int64) *big.Int { return new(big.Int).Mul(big.NewInt(n), e18) }

// fakeChain is a deterministic stand-in for DarkPool: a note tree fed by "events", pool reserves, a spent set.
type fakeChain struct {
	tree      *notetree.Tree
	leaves    []chain.Leaf
	block     uint64
	ts        uint64
	pool      chain.PoolState
	spent     map[string]bool
	submitted [][]byte
	failNext  bool
}

func newFake() *fakeChain {
	return &fakeChain{tree: notetree.New(notetree.Depth), block: 1, ts: 1_800_000_000, spent: map[string]bool{},
		pool: chain.PoolState{R0: tok(1_000_000), R1: tok(2_000_000), V0: tok(2_000_000), V1: tok(4_000_000), BatchSize: 4}}
}

func (f *fakeChain) addLeaf(c *big.Int) {
	idx, _ := f.tree.Insert(c)
	f.block++
	f.leaves = append(f.leaves, chain.Leaf{Index: uint64(idx), Commitment: c, Block: f.block})
}
func (f *fakeChain) Head(context.Context) (chain.Head, error) {
	return chain.Head{Number: f.block, Timestamp: f.ts}, nil
}
func (f *fakeChain) Pool(context.Context) (chain.PoolState, error) {
	p := f.pool
	p.Root = f.tree.Root()
	p.NextLeaf = uint64(f.tree.Len())
	return p, nil
}
func (f *fakeChain) Leaves(_ context.Context, from, to uint64) ([]chain.Leaf, error) {
	var out []chain.Leaf
	for _, l := range f.leaves {
		if l.Block >= from && l.Block <= to {
			out = append(out, l)
		}
	}
	return out, nil
}
func (f *fakeChain) Spent(_ context.Context, n *big.Int) (bool, error) {
	return f.spent[n.String()], nil
}
func (f *fakeChain) Submit(_ context.Context, cd []byte) (string, error) {
	if f.failNext {
		f.failNext = false
		return "", errors.New("signer unavailable")
	}
	f.submitted = append(f.submitted, cd)
	return "0xabc", nil
}

func mkIntent(i int, dir int, value *big.Int, minOut int64) *intent.Intent {
	return &intent.Intent{Dir: dir, Value: value, MinOut: tok(minOut), Deadline: 1_800_003_600,
		DestPubKey: big.NewInt(int64(100 + i)), RefundPubKey: big.NewInt(int64(200 + i)), DestBlinding: big.NewInt(int64(300 + i))}
}

func quiet() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

// captureProver records the witness so the test can "apply" the batch on the fake chain.
type captureProver struct{ w map[string]any }

func (c *captureProver) Prove(_ context.Context, w map[string]any, exp []*big.Int) (*prover.Proof, error) {
	c.w = w
	return &prover.Proof{Bytes: make([]byte, 256), Public: exp}, nil
}

func setup(t *testing.T, minSwaps int) (*Service, *fakeChain, *captureProver, []*intent.Intent) {
	t.Helper()
	fc := newFake()
	ints := []*intent.Intent{mkIntent(0, 0, tok(1000), 1900), mkIntent(1, 1, tok(800), 390), mkIntent(2, 0, tok(250), 0)}
	for _, in := range ints {
		fc.addLeaf(in.Commitment())
	}
	cp := &captureProver{}
	svc := New(Config{N: 4, MinSwaps: minSwaps, Interval: time.Second, MaxPending: 8}, fc, cp, quiet())
	if err := svc.SyncTree(context.Background()); err != nil {
		t.Fatal(err)
	}
	return svc, fc, cp, ints
}

func TestEndToEndEpoch(t *testing.T) {
	svc, fc, cp, ints := setup(t, 2)
	var receipts []string
	for _, in := range ints {
		r, err := svc.Submit(in)
		if err != nil {
			t.Fatal(err)
		}
		receipts = append(receipts, r)
	}
	if err := svc.Tick(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(fc.submitted) != 1 {
		t.Fatalf("want 1 batch submitted, got %d", len(fc.submitted))
	}
	cd := fc.submitted[0]
	if sel := chain.Selector("settleBatch((bytes,bytes32,uint64,bytes32[],bytes32[],bytes[],uint256,uint256,uint256,uint256))"); !bytes.Equal(cd[:4], sel[:]) {
		t.Fatal("wrong function selector")
	}
	tuple := cd[4+32:]
	if !bytes.Equal(tuple[32:64], field.Bytes32Slice(fc.tree.Root())) {
		t.Fatal("batch is not bound to the current tree root")
	}
	for _, r := range receipts {
		if st, _ := svc.Status(r); st.State != Inflight {
			t.Fatalf("want inflight, got %s", st.State)
		}
	}
	// the chain mines it: output commitments appear as new leaves
	for _, c := range cp.w["commitment"].([]string) {
		v, _ := new(big.Int).SetString(c, 10)
		if v.Sign() != 0 {
			fc.addLeaf(v)
		}
	}
	if err := svc.Tick(context.Background()); err != nil {
		t.Fatal(err)
	}
	for _, r := range receipts {
		if st, _ := svc.Status(r); st.State != Settled || st.Tx != "0xabc" {
			t.Fatalf("want settled, got %+v", st)
		}
	}
	if len(fc.submitted) != 1 {
		t.Fatal("settled intents must not be re-batched")
	}
}

func TestSubmitRules(t *testing.T) {
	svc, _, _, ints := setup(t, 1)
	r1, err := svc.Submit(ints[0])
	if err != nil {
		t.Fatal(err)
	}
	if r2, err := svc.Submit(ints[0]); err != nil || r2 != r1 {
		t.Fatal("re-submission must be idempotent and return the same receipt")
	}
	if _, err := svc.Submit(mkIntent(99, 0, tok(5), 0)); !errors.Is(err, ErrUnknownNote) {
		t.Fatalf("a note that is not in the tree must be refused, got %v", err)
	}
	bad := mkIntent(0, 0, tok(1000), 1900)
	bad.Dir = 7
	if _, err := svc.Submit(bad); !errors.Is(err, intent.ErrInvalid) {
		t.Fatal("invalid intent accepted")
	}
	if _, ok := svc.Status("deadbeef"); ok {
		t.Fatal("unknown receipt must not exist")
	}
}

func TestBelowAnonymityFloorNothingIsSubmitted(t *testing.T) {
	svc, fc, _, ints := setup(t, 2)
	if _, err := svc.Submit(ints[0]); err != nil {
		t.Fatal(err)
	}
	if err := svc.Tick(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(fc.submitted) != 0 {
		t.Fatal("a single swap must not be settled alone (it would leak its size)")
	}
}

func TestSubmitFailureKeepsIntentsQueued(t *testing.T) {
	svc, fc, _, ints := setup(t, 2)
	var rs []string
	for _, in := range ints {
		r, _ := svc.Submit(in)
		rs = append(rs, r)
	}
	fc.failNext = true
	if err := svc.Tick(context.Background()); err == nil {
		t.Fatal("expected the signer error")
	}
	if st, _ := svc.Status(rs[0]); st.State != Queued {
		t.Fatal("intents must stay queued after a failed submission")
	}
	if err := svc.Tick(context.Background()); err != nil || len(fc.submitted) != 1 {
		t.Fatalf("retry failed: %v", err)
	}
}

func TestIntentConsumedByAnotherAggregatorIsDropped(t *testing.T) {
	svc, fc, _, ints := setup(t, 1)
	r, _ := svc.Submit(ints[0])
	idx, _ := svc.tree.IndexOf(ints[0].Commitment())
	fc.spent[ints[0].Nullifier(idx).String()] = true
	_ = svc.Tick(context.Background())
	if st, _ := svc.Status(r); st.State != Dropped {
		t.Fatalf("want dropped, got %s", st.State)
	}
}

func TestSafetyChecksBeforeProving(t *testing.T) {
	svc, fc, cp, ints := setup(t, 1)
	svc.Submit(ints[0])
	fc.pool.BatchSize = 8 // circuit/pool mismatch
	if err := svc.Tick(context.Background()); err == nil || cp.w != nil {
		t.Fatal("must refuse to prove against a pool with a different batch size")
	}
	fc.pool.BatchSize = 4
	svc.tree = notetree.New(notetree.Depth) // simulate a desynchronised mirror
	svc.scanned = fc.block
	if err := svc.Tick(context.Background()); !errors.Is(err, ErrTreeBehind) {
		t.Fatalf("a desynchronised tree must abort the epoch, got %v", err)
	}
}

func TestCapacityBound(t *testing.T) {
	fc := newFake()
	svc := New(Config{N: 4, MinSwaps: 1, MaxPending: 2}, fc, &prover.Mock{}, quiet())
	var all []*intent.Intent
	for i := 0; i < 3; i++ {
		in := mkIntent(i, 0, tok(10), 0)
		fc.addLeaf(in.Commitment())
		all = append(all, in)
	}
	svc.SyncTree(context.Background())
	svc.Submit(all[0])
	svc.Submit(all[1])
	if _, err := svc.Submit(all[2]); !errors.Is(err, ErrBusy) {
		t.Fatalf("want ErrBusy, got %v", err)
	}
}

func TestSubmitResyncsForFreshNotesButIsRateLimited(t *testing.T) {
	svc, fc, _, _ := setup(t, 1)
	svc.lastSync = time.Now().Add(-time.Minute) // last scan is old enough to allow a resync
	fresh := mkIntent(50, 0, tok(5), 0)
	fc.addLeaf(fresh.Commitment()) // created on-chain AFTER the service's last scan
	if _, err := svc.Submit(fresh); err != nil {
		t.Fatalf("a freshly created note must be found by an on-demand resync: %v", err)
	}
	// a second unknown note immediately afterwards must NOT trigger another chain sync (amplification guard)
	before := fc.block
	junk := mkIntent(51, 0, tok(5), 0)
	fc.addLeaf(junk.Commitment())
	if _, err := svc.Submit(junk); !errors.Is(err, ErrUnknownNote) {
		t.Fatalf("resync must be rate-limited, got %v", err)
	}
	_ = before
}
