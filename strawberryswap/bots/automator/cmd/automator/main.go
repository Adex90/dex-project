// Command automator is the StrawberrySwap batch relayer.
//
//	automator serve          run the node (intake API on loopback/unix socket, epoch loop, Tor egress)
//	automator batch-witness  build a batch + circuit witness from a state file (used by CI and fixtures)
//	automator prove          prove a batch-witness output with the configured prover (same code path as `serve`)
//	automator version
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"math/big"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"strawberryswap/automator/internal/batcher"
	"strawberryswap/automator/internal/chain"
	"strawberryswap/automator/internal/intent"
	"strawberryswap/automator/internal/notetree"
	"strawberryswap/automator/internal/privacy"
	"strawberryswap/automator/internal/prover"
	"strawberryswap/automator/internal/server"
	"strawberryswap/automator/internal/service"
)

const version = "0.1.0"

func main() {
	if len(os.Args) < 2 {
		usage()
	}
	var err error
	switch os.Args[1] {
	case "serve":
		err = serve(os.Args[2:])
	case "batch-witness":
		err = batchWitness(os.Args[2:])
	case "prove":
		err = proveCmd(os.Args[2:])
	case "version":
		fmt.Println("automator", version)
	default:
		usage()
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", privacy.Scrub(err.Error()))
		os.Exit(1)
	}
}

func usage() {
	fmt.Fprintln(os.Stderr, "usage: automator serve|batch-witness|version  (use -h on a subcommand)")
	os.Exit(2)
}

// ---------------------------------------------------------------------------------------------- serve

type fileConfig struct {
	Listen      string `json:"listen"`      // unix:/run/automator/intents.sock  or 127.0.0.1:8080
	TorSocks    string `json:"torSocks"`    // 127.0.0.1:9050
	RPCURL      string `json:"rpcUrl"`      // chain JSON-RPC (reached through Tor)
	SignerURL   string `json:"signerUrl"`   // MPC/TSS gateway (reached through Tor)
	SignerToken string `json:"signerToken"` // prefer env AUTOMATOR_SIGNER_TOKEN
	ChainID     uint64 `json:"chainId"`
	Pool        string `json:"pool"`
	DeployBlock uint64 `json:"deployBlock"`
	N           int    `json:"batchSize"`
	MinSwaps    int    `json:"minSwaps"`
	IntervalSec int    `json:"intervalSeconds"`
	JitterSec   int    `json:"jitterSeconds"`
	GasLimit    uint64 `json:"gasLimit"`
	Snarkjs     string `json:"snarkjs"`
	Wasm        string `json:"wasm"`
	Zkey        string `json:"zkey"`
	ProverTmp   string `json:"proverTmpDir"`
	// valueMarginBps: plan batches to survive this much oracle movement while proving (default 0; every batch is
	// re-checked against the live oracle before submission anyway).
	ValueMarginBps int64 `json:"valueMarginBps"`
	// Development escape hatches: each one is a deliberate privacy downgrade and is named accordingly.
	DevAllowClearnetListen bool `json:"devAllowClearnetListen"`
	DevAllowDirectEgress   bool `json:"devAllowDirectEgress"`
}

func serve(args []string) error {
	fs := flag.NewFlagSet("serve", flag.ExitOnError)
	cfgPath := fs.String("config", "", "path to config JSON")
	_ = fs.Parse(args)
	raw, err := os.ReadFile(*cfgPath)
	if err != nil {
		return fmt.Errorf("read config: %w", err)
	}
	var c fileConfig
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&c); err != nil {
		return fmt.Errorf("config: %w", err)
	}
	if t := os.Getenv("AUTOMATOR_SIGNER_TOKEN"); t != "" {
		c.SignerToken = t
	}

	log := privacy.NewLogger(os.Stderr, slog.LevelInfo)
	if err := privacy.Harden(); err != nil {
		log.Warn("process hardening unavailable", "err", err.Error())
	}

	mode := privacy.Onion
	if c.DevAllowClearnetListen {
		mode = privacy.Clearnet
		log.Warn("DEV MODE: clearnet listener allowed — client IPs are exposed to the host and its network")
	}
	ln, err := privacy.Listen(c.Listen, mode) // refuses non-loopback in onion mode
	if err != nil {
		return err
	}

	// Egress: Tor only. There is no code path that dials the chain RPC or signer directly unless the operator
	// sets the explicit development flag.
	var rpcHTTP, signerHTTP *http.Client
	if c.DevAllowDirectEgress {
		log.Warn("DEV MODE: direct egress allowed — chain RPC and signer will see this host's address")
		rpcHTTP, signerHTTP = &http.Client{Timeout: 60 * time.Second}, &http.Client{Timeout: 60 * time.Second}
	} else {
		if c.TorSocks == "" {
			return errors.New("torSocks is required (set devAllowDirectEgress only for local development)")
		}
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		err := privacy.CheckSOCKS(ctx, c.TorSocks)
		cancel()
		if err != nil {
			return fmt.Errorf("tor SOCKS proxy not reachable: %w", err)
		}
		t1, _ := privacy.TorTransport(c.TorSocks, "automator-rpc")
		t2, _ := privacy.TorTransport(c.TorSocks, "automator-signer")
		rpcHTTP, signerHTTP = &http.Client{Transport: t1, Timeout: 120 * time.Second}, &http.Client{Transport: t2, Timeout: 120 * time.Second}
	}

	rpc := &chain.RPC{URL: c.RPCURL, Pool_: c.Pool, HTTP: rpcHTTP, GasLimit: c.GasLimit,
		Signer: &chain.HTTPSigner{URL: c.SignerURL, Token: c.SignerToken, ChainID: c.ChainID, HTTP: signerHTTP}}
	prv := &prover.SnarkJS{Bin: c.Snarkjs, Wasm: c.Wasm, Zkey: c.Zkey, TmpDir: orDefault(c.ProverTmp, "/dev/shm"), Timeout: 10 * time.Minute}
	svc := service.New(service.Config{
		N: c.N, MinSwaps: c.MinSwaps, Interval: time.Duration(c.IntervalSec) * time.Second, Jitter: time.Duration(c.JitterSec) * time.Second,
		DeployBlock: c.DeployBlock, ValueMarginBps: c.ValueMarginBps,
	}, rpc, prv, log)

	srv := privacy.NewServer(server.New(svc, 64), log)
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	go svc.Run(ctx)
	go func() {
		<-ctx.Done()
		sc, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = srv.Shutdown(sc)
	}()
	log.Info("automator started", "batchSize", c.N, "minSwaps", c.MinSwaps)
	if err := srv.Serve(ln); err != nil && !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil // intents live in memory only and vanish here by design
}

func orDefault(s, d string) string {
	if s == "" {
		return d
	}
	return s
}

// ---------------------------------------------------------------------------------------------- batch-witness

type stateFile struct {
	Leaves   []string          `json:"leaves"`
	Pool     map[string]string `json:"pool"` // X, Y, R0, R1, and the fresh oracle "price" (absent = stale: refunds only)
	TNow     uint64            `json:"tNow"`
	N        int               `json:"batchSize"`
	MinSwaps int               `json:"minSwaps"`
	Intents  []intent.Intent   `json:"intents"`
}

func batchWitness(args []string) error {
	fs := flag.NewFlagSet("batch-witness", flag.ExitOnError)
	in := fs.String("state", "", "input state JSON")
	out := fs.String("out", "", "output JSON (witness, public inputs, per-slot outputs)")
	_ = fs.Parse(args)
	raw, err := os.ReadFile(*in)
	if err != nil {
		return err
	}
	var st stateFile
	if err := json.Unmarshal(raw, &st); err != nil {
		return err
	}
	tree := notetree.New(notetree.Depth)
	for _, l := range st.Leaves {
		v, ok := new(big.Int).SetString(l, 10)
		if !ok {
			return errors.New("bad leaf")
		}
		if _, err := tree.Insert(v); err != nil {
			return err
		}
	}
	pb := func(k string) *big.Int { v, _ := new(big.Int).SetString(st.Pool[k], 10); return v }
	pool := batcher.Pool{X: pb("X"), Y: pb("Y"), R0: pb("R0"), R1: pb("R1")}
	if _, ok := st.Pool["price"]; ok {
		pool.Price = pb("price")
	}
	var cands []batcher.Candidate
	for i := range st.Intents {
		it := &st.Intents[i]
		if err := it.Validate(); err != nil {
			return err
		}
		idx, ok := tree.IndexOf(it.Commitment())
		if !ok {
			return fmt.Errorf("intent %d: note is not among the leaves", i)
		}
		cands = append(cands, batcher.Candidate{Intent: it, LeafIndex: idx})
	}
	plan, err := batcher.Build(pool, cands, st.TNow, batcher.Config{N: st.N, MinSwaps: st.MinSwaps})
	if err != nil {
		return err
	}
	w, err := plan.Witness(tree, st.TNow, pool, st.N)
	if err != nil {
		return err
	}
	pub := plan.PublicInputs(tree.Root(), st.TNow, pool, st.N)
	type slotOut struct {
		Mode       string `json:"mode"`
		Index      int    `json:"leafIndex"`
		Nullifier  string `json:"nullifier"`
		Commitment string `json:"commitment"`
		AssetOut   int    `json:"assetOut"`
		ValueOut   string `json:"valueOut"`
		PubKey     string `json:"destPubKey"`
		Blinding   string `json:"destBlinding"`
	}
	var slots []slotOut
	for i, o := range plan.Outputs() {
		s := plan.Slots[i]
		mode, pk := "swap", s.Cand.Intent.DestPubKey
		if s.Mode == batcher.Refund {
			mode, pk = "refund", s.Cand.Intent.RefundPubKey
		}
		slots = append(slots, slotOut{mode, s.Cand.LeafIndex, o.Nullifier.String(), o.Commitment.String(), o.AssetOut, o.ValueOut.String(), pk.String(), s.Cand.Intent.DestBlinding.String()})
	}
	pubStr := make([]string, len(pub))
	for i, p := range pub {
		pubStr[i] = p.String()
	}
	res, _ := json.MarshalIndent(map[string]any{"witness": w, "public": pubStr, "slots": slots, "netIn0": plan.NetIn0.String(), "netIn1": plan.NetIn1.String(), "netOut0": plan.NetOut0.String(), "netOut1": plan.NetOut1.String(), "price": plan.PLo.String()}, "", " ")
	return os.WriteFile(*out, res, 0o600)
}

// ---------------------------------------------------------------------------------------------- prove

// proveCmd runs the production prover wrapper over a batch-witness output and writes {proof, public}. CI uses it to
// produce fixtures with exactly the code path the node uses, so the proof encoding is tested against the real verifier.
func proveCmd(args []string) error {
	fs := flag.NewFlagSet("prove", flag.ExitOnError)
	wf := fs.String("witness", "", "batch-witness output JSON")
	out := fs.String("out", "", "output JSON")
	sj := fs.String("snarkjs", "snarkjs", "snarkjs executable")
	wasm := fs.String("wasm", "", "circuit wasm")
	zkey := fs.String("zkey", "", "proving key")
	_ = fs.Parse(args)
	raw, err := os.ReadFile(*wf)
	if err != nil {
		return err
	}
	var in struct {
		Witness map[string]any `json:"witness"`
		Public  []string       `json:"public"`
	}
	if err := json.Unmarshal(raw, &in); err != nil {
		return err
	}
	exp := make([]*big.Int, len(in.Public))
	for i, s := range in.Public {
		exp[i], _ = new(big.Int).SetString(s, 10)
	}
	p := &prover.SnarkJS{Bin: *sj, Wasm: *wasm, Zkey: *zkey, TmpDir: os.TempDir(), Timeout: 15 * time.Minute}
	proof, err := p.Prove(context.Background(), in.Witness, exp)
	if err != nil {
		return err
	}
	res, _ := json.Marshal(map[string]any{"proof": fmt.Sprintf("0x%x", proof.Bytes), "public": in.Public})
	return os.WriteFile(*out, res, 0o600)
}
