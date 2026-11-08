// Package service is the Automator's brain: it holds pending intents in MEMORY ONLY, mirrors the note tree from
// chain events, and each epoch builds, proves and submits one batch.
package service

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"math/big"
	"sync"
	"time"

	"strawberryswap/automator/internal/batcher"
	"strawberryswap/automator/internal/chain"
	"strawberryswap/automator/internal/field"
	"strawberryswap/automator/internal/intent"
	"strawberryswap/automator/internal/notetree"
	"strawberryswap/automator/internal/prover"
)

var (
	ErrBusy        = errors.New("automator is at capacity")
	ErrUnknownNote = errors.New("intent note is not in the note tree: create it on-chain first")
	ErrTreeBehind  = errors.New("local note tree is not in sync with the chain")
	ErrStaleOracle = errors.New("oracle price went stale while proving; batch held back")
	ErrValueLeak   = errors.New("oracle moved while proving: the batch would now lower pool value; held back")
)

type State string

const (
	Queued   State = "queued"
	Inflight State = "inflight"
	Settled  State = "settled"
	Refunded State = "refunded"
	Dropped  State = "dropped" // consumed by another aggregator (its nullifier is spent)
)

type Config struct {
	N            int           // circuit slots (must equal DarkPool.batchSize)
	MinSwaps     int           // anonymity floor
	Interval     time.Duration // epoch length
	Jitter       time.Duration // ± random offset so batch times don't track request arrivals
	MaxPending   int           // memory / DoS bound
	DeployBlock  uint64        // first block to scan for NoteInserted
	LogChunk     uint64        // blocks per eth_getLogs
	RetainEpochs int           // how long finished intents stay queryable (in epochs, not wall time)
	// MaxOracleAge mirrors DarkPool.MAX_ORACLE_AGE: an older price cannot back a value-moving batch.
	MaxOracleAge uint64
	// ValueMarginBps (default 0): plan batches so they still pass DarkPool's value guard if the oracle moves this
	// far. Every batch is re-checked against the oracle right before submission either way, so the margin only
	// trades fewer wasted proofs for fewer settled intents.
	ValueMarginBps int64
	// TreeDepth of the local note-tree mirror; must equal NoteTree.DEPTH (tests use smaller trees).
	TreeDepth int
}

func (c *Config) defaults() {
	if c.LogChunk == 0 {
		c.LogChunk = 2000
	}
	if c.MaxOracleAge == 0 {
		c.MaxOracleAge = 60
	}
	if c.TreeDepth == 0 {
		c.TreeDepth = notetree.Depth
	}
	if c.MaxPending == 0 {
		c.MaxPending = 4096
	}
	if c.RetainEpochs == 0 {
		c.RetainEpochs = 100
	}
}

type entry struct {
	in        *intent.Intent
	state     State
	tx        string
	outCommit *big.Int // expected output note, to detect settlement from chain events
	isRefund  bool
	doneEpoch int
}

type Status struct {
	State State  `json:"state"`
	Tx    string `json:"txHash,omitempty"`
}

type Service struct {
	cfg Config
	ch  chain.Chain
	prv prover.Prover
	log *slog.Logger

	mu       sync.Mutex
	tree     *notetree.Tree    // the current on-chain tree
	trees    []*notetree.Tree  // completed (rolled-over) trees, oldest first; their roots stay valid on-chain forever
	scanned  uint64            // last block fully scanned
	entries  map[string]*entry // by intent commitment (decimal)
	receipts map[string]string // receipt -> commitment
	epoch    int
	lastSync time.Time // rate-limits on-demand resyncs triggered by unknown-note submissions
}

// minResync bounds how often an unknown note may trigger a chain sync: otherwise submitting junk would be an
// RPC-amplification vector against the operator's node.
const minResync = 15 * time.Second

func New(cfg Config, ch chain.Chain, prv prover.Prover, log *slog.Logger) *Service {
	cfg.defaults()
	return &Service{cfg: cfg, ch: ch, prv: prv, log: log, tree: notetree.New(cfg.TreeDepth),
		scanned: cfg.DeployBlock - boolU(cfg.DeployBlock > 0), entries: map[string]*entry{}, receipts: map[string]string{}}
}

func boolU(b bool) uint64 {
	if b {
		return 1
	}
	return 0
}

// Info is public, non-identifying configuration clients need to build intent notes.
type Info struct {
	BatchSize    int    `json:"batchSize"`
	MinSwaps     int    `json:"minSwaps"`
	EpochSeconds int    `json:"epochSeconds"`
	BatchPubKey  string `json:"batchPubKey"`
}

func (s *Service) Info() Info {
	return Info{BatchSize: s.cfg.N, MinSwaps: s.cfg.MinSwaps, EpochSeconds: int(s.cfg.Interval.Seconds()), BatchPubKey: intent.BatchPubKey.String()}
}

// Submit validates an intent and queues it. It is idempotent: re-submitting the same intent returns the same
// receipt. The note must already exist on-chain — which doubles as the anti-spam rule (you cannot flood the node
// without first creating funded notes), since per-IP rate limiting is impossible by design.
func (s *Service) Submit(in *intent.Intent) (string, error) {
	if err := in.Validate(); err != nil {
		return "", err
	}
	key := in.Commitment().String()
	s.mu.Lock()
	defer s.mu.Unlock()
	if e, ok := s.entries[key]; ok {
		for r, c := range s.receipts {
			if c == key {
				_ = e
				return r, nil
			}
		}
	}
	if len(s.entries) >= s.cfg.MaxPending {
		return "", ErrBusy
	}
	if _, _, ok := s.locateLocked(in.Commitment()); !ok {
		// the note may have been created after our last scan (a user submits right after their transferNote)
		if time.Since(s.lastSync) > minResync {
			ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
			_ = s.syncLocked(ctx)
			cancel()
		}
		if _, _, ok := s.locateLocked(in.Commitment()); !ok {
			return "", ErrUnknownNote
		}
	}
	var rb [16]byte
	if _, err := rand.Read(rb[:]); err != nil {
		return "", err
	}
	receipt := hex.EncodeToString(rb[:])
	s.entries[key] = &entry{in: in, state: Queued}
	s.receipts[receipt] = key
	return receipt, nil
}

func (s *Service) Status(receipt string) (Status, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	key, ok := s.receipts[receipt]
	if !ok {
		return Status{}, false
	}
	e := s.entries[key]
	return Status{State: e.state, Tx: e.tx}, true
}

// SyncTree scans NoteInserted events and appends them to the local mirror. Leaves MUST arrive in index order.
func (s *Service) SyncTree(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.syncLocked(ctx)
}

func (s *Service) syncLocked(ctx context.Context) error {
	s.lastSync = time.Now()
	head, err := s.ch.Head(ctx)
	if err != nil {
		return err
	}
	for from := s.scanned + 1; from <= head.Number; from += s.cfg.LogChunk {
		to := from + s.cfg.LogChunk - 1
		if to > head.Number {
			to = head.Number
		}
		leaves, err := s.ch.Leaves(ctx, from, to)
		if err != nil {
			return err
		}
		for _, l := range leaves {
			if s.tree.Len() == 1<<s.cfg.TreeDepth && l.Index == 0 {
				// NoteTree rolled over: the full tree is final (its root stays valid), indices restart at 0
				s.trees = append(s.trees, s.tree)
				s.tree = notetree.New(s.cfg.TreeDepth)
			}
			switch {
			case l.Index < uint64(s.tree.Len()):
				continue // already have it (overlapping scan)
			case l.Index > uint64(s.tree.Len()):
				return fmt.Errorf("%w: gap at leaf %d", ErrTreeBehind, s.tree.Len())
			}
			if _, err := s.tree.Insert(l.Commitment); err != nil {
				return err
			}
		}
		s.scanned = to
	}
	return nil
}

// Tick runs one epoch: sync → select → prove → submit. It never blocks intent intake for longer than the
// bookkeeping sections (proving happens without the lock).
func (s *Service) Tick(ctx context.Context) error {
	s.mu.Lock()
	s.epoch++
	epoch := s.epoch
	if err := s.syncLocked(ctx); err != nil {
		s.mu.Unlock()
		return err
	}
	pool, err := s.ch.Pool(ctx)
	if err != nil {
		s.mu.Unlock()
		return err
	}
	if int(pool.BatchSize) != s.cfg.N {
		s.mu.Unlock()
		return fmt.Errorf("configured batch size %d differs from the pool's %d", s.cfg.N, pool.BatchSize)
	}
	if pool.TreeID != uint64(len(s.trees)) || pool.Root.Cmp(s.tree.Root()) != 0 {
		s.mu.Unlock()
		return ErrTreeBehind // the chain moved between our scan and the pool read; retry next epoch
	}
	s.reconcileLocked(ctx)
	head, err := s.ch.Head(ctx)
	if err != nil {
		s.mu.Unlock()
		return err
	}
	// One batch proves membership under ONE root: settle the oldest tree that has pending intents first (a
	// completed tree's final root stays valid on-chain forever).
	byTree := map[int][]batcher.Candidate{}
	first := -1
	for _, e := range s.entries {
		if e.state != Queued {
			continue
		}
		t, idx, ok := s.locateLocked(e.in.Commitment())
		if !ok {
			continue
		}
		byTree[t] = append(byTree[t], batcher.Candidate{Intent: e.in, LeafIndex: idx})
		if first < 0 || t < first {
			first = t
		}
	}
	if first < 0 {
		s.mu.Unlock()
		return nil
	}
	cands, tree := byTree[first], s.treeAtLocked(first)
	s.mu.Unlock()

	orc, err := s.ch.Oracle(ctx)
	if err != nil {
		return err
	}
	bp := batcher.Pool{X: pool.X(), Y: pool.Y(), R0: pool.R0, R1: pool.R1, Price: s.freshPrice(orc, head.Timestamp)}
	if bp.Price == nil {
		s.log.Warn("oracle stale: refunds only this epoch")
	}
	plan, err := batcher.Build(bp, cands, head.Timestamp, batcher.Config{N: s.cfg.N, MinSwaps: s.cfg.MinSwaps, ValueMarginBps: s.cfg.ValueMarginBps})
	if errors.Is(err, batcher.ErrNoBatch) {
		return nil
	}
	if err != nil {
		return err
	}
	witness, err := plan.Witness(tree, head.Timestamp, bp, s.cfg.N)
	if err != nil {
		return err
	}
	pub := plan.PublicInputs(tree.Root(), head.Timestamp, bp, s.cfg.N)
	proof, err := s.prv.Prove(ctx, witness, pub)
	if err != nil {
		return err
	}

	// Proving took a while: re-check DarkPool's value guard against the oracle as it is NOW. A batch that would
	// revert is not worth its gas; its intents stay queued for the next epoch.
	if plan.Swaps > 0 {
		now, err := s.ch.Head(ctx)
		if err != nil {
			return err
		}
		o2, err := s.ch.Oracle(ctx)
		if err != nil {
			return err
		}
		p2 := s.freshPrice(o2, now.Timestamp)
		if p2 == nil {
			return ErrStaleOracle
		}
		if leak, _ := batcher.LeaksValue(plan.NetIn0, plan.NetIn1, plan.NetOut0, plan.NetOut1, p2); leak {
			return ErrValueLeak
		}
	}

	outs := plan.Outputs()
	b := &chain.Batch{Proof: proof.Bytes, TNow: head.Timestamp, In0: plan.NetIn0, In1: plan.NetIn1, Out0: plan.NetOut0, Out1: plan.NetOut1,
		X: bp.X, Y: bp.Y}
	b.Root = field.Bytes32(tree.Root())
	for i, o := range outs {
		sealed, err := plan.Slots[i].Cand.Intent.Seal(o.AssetOut, o.ValueOut)
		if err != nil {
			return err
		}
		b.Nullifiers = append(b.Nullifiers, field.Bytes32(o.Nullifier))
		b.Commitments = append(b.Commitments, field.Bytes32(o.Commitment))
		b.EncryptedNotes = append(b.EncryptedNotes, sealed)
	}
	tx, err := s.ch.Submit(ctx, chain.EncodeSettleBatch(b))
	if err != nil {
		return err // intents stay queued; nothing was lost
	}
	s.mu.Lock()
	for i, sl := range plan.Slots {
		if e := s.entries[sl.Cand.Intent.Commitment().String()]; e != nil {
			e.state, e.tx, e.outCommit, e.isRefund = Inflight, tx, outs[i].Commitment, sl.Mode == batcher.Refund
		}
	}
	s.mu.Unlock()
	// counters only: no sizes, no identifiers (privacy.bannedKeys would drop them anyway)
	s.log.Info("batch submitted", "epoch", epoch, "swaps", plan.Swaps, "refunds", plan.Refunds)
	return nil
}

// reconcileLocked moves inflight intents to settled/refunded once their output note appears in the tree, marks
// intents consumed by someone else as dropped, and prunes old finished entries.
func (s *Service) reconcileLocked(ctx context.Context) {
	for k, e := range s.entries {
		switch e.state {
		case Inflight:
			if _, _, ok := s.locateLocked(e.outCommit); ok {
				e.state, e.doneEpoch = Settled, s.epoch
				if e.isRefund {
					e.state = Refunded
				}
			}
		case Queued:
			if _, idx, ok := s.locateLocked(e.in.Commitment()); ok {
				if spent, err := s.ch.Spent(ctx, e.in.Nullifier(idx)); err == nil && spent {
					e.state, e.doneEpoch = Dropped, s.epoch
				}
			}
		case Settled, Refunded, Dropped:
			if s.epoch-e.doneEpoch > s.cfg.RetainEpochs {
				delete(s.entries, k)
				for r, c := range s.receipts {
					if c == k {
						delete(s.receipts, r)
					}
				}
			}
		}
	}
}

// locateLocked finds a commitment in the completed trees (oldest first) or the current one: (tree ordinal, leaf, ok).
func (s *Service) locateLocked(c *big.Int) (int, int, bool) {
	for t, tr := range s.trees {
		if i, ok := tr.IndexOf(c); ok {
			return t, i, true
		}
	}
	if i, ok := s.tree.IndexOf(c); ok {
		return len(s.trees), i, true
	}
	return 0, 0, false
}

func (s *Service) treeAtLocked(t int) *notetree.Tree {
	if t < len(s.trees) {
		return s.trees[t]
	}
	return s.tree
}

// freshPrice returns the oracle price if DarkPool would accept it at time now, else nil.
func (s *Service) freshPrice(o chain.OraclePrice, now uint64) *big.Int {
	if o.Price == nil || o.Price.Sign() == 0 || o.UpdatedAt > now || now-o.UpdatedAt > s.cfg.MaxOracleAge {
		return nil
	}
	return o.Price
}

// Run is the epoch loop. Epoch boundaries are jittered with crypto/rand and are independent of request arrival.
func (s *Service) Run(ctx context.Context) {
	for {
		d := s.cfg.Interval
		if s.cfg.Jitter > 0 {
			var b [8]byte
			if _, err := rand.Read(b[:]); err == nil {
				j := int64(binary.BigEndian.Uint64(b[:]) % uint64(2*s.cfg.Jitter))
				d += time.Duration(j) - s.cfg.Jitter
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(d):
		}
		if err := s.Tick(ctx); err != nil {
			s.log.Warn("epoch failed", "err", err.Error())
		}
	}
}
