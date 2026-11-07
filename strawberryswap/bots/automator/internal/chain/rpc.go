package chain

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"strings"
	"sync/atomic"

	"strawberryswap/automator/internal/field"
)

// PoolState is what the Automator needs from DarkPool to build a batch.
type PoolState struct {
	R0, R1, V0, V1 *big.Int // real and virtual reserves
	Root           *big.Int // NoteTree.currentRoot()
	NextLeaf       uint64
	BatchSize      uint64
}

// X, Y are the effective reserves (the circuit's public inputs).
func (p PoolState) X() *big.Int { return new(big.Int).Add(p.R0, p.V0) }
func (p PoolState) Y() *big.Int { return new(big.Int).Add(p.R1, p.V1) }

type Leaf struct {
	Index      uint64
	Commitment *big.Int
	Block      uint64
}

type Head struct {
	Number    uint64
	Timestamp uint64
}

// Chain is the Automator's whole view of the blockchain. Everything is behind this interface so the service can be
// tested deterministically and the transport (Tor SOCKS, private relay) is a deployment concern.
type Chain interface {
	Head(ctx context.Context) (Head, error)
	Pool(ctx context.Context) (PoolState, error)
	Leaves(ctx context.Context, from, to uint64) ([]Leaf, error)
	Spent(ctx context.Context, nullifier *big.Int) (bool, error)
	Submit(ctx context.Context, calldata []byte) (txHash string, err error)
}

// Signer signs and broadcasts a transaction. The Automator NEVER holds a key: production points this at an
// MPC/TSS signing gateway (ideally one that broadcasts through a private relay to avoid the public mempool).
type Signer interface {
	SignAndSend(ctx context.Context, to string, data []byte, gasLimit uint64) (txHash string, err error)
}

// RPC implements Chain over plain JSON-RPC.
type RPC struct {
	URL      string
	Pool_    string // DarkPool address (0x…)
	HTTP     *http.Client
	Signer   Signer
	GasLimit uint64
	id       atomic.Uint64
}

func (c *RPC) call(ctx context.Context, method string, params ...any) (json.RawMessage, error) {
	body, _ := json.Marshal(map[string]any{"jsonrpc": "2.0", "id": c.id.Add(1), "method": method, "params": params})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.URL, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("content-type", "application/json")
	resp, err := c.HTTP.Do(req)
	if err != nil {
		return nil, fmt.Errorf("rpc %s: %w", method, scrubErr(err))
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 32<<20))
	if err != nil {
		return nil, err
	}
	var out struct {
		Result json.RawMessage `json:"result"`
		Error  *struct {
			Code    int    `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, fmt.Errorf("rpc %s: bad response", method)
	}
	if out.Error != nil {
		return nil, fmt.Errorf("rpc %s: %d %s", method, out.Error.Code, out.Error.Message)
	}
	return out.Result, nil
}

// scrubErr drops the URL (and therefore any host/credentials) that net/http embeds in transport errors.
func scrubErr(err error) error {
	var ue interface{ Unwrap() error }
	if errors.As(err, &ue) && ue.Unwrap() != nil {
		return ue.Unwrap()
	}
	return err
}

func hexU64(s string) (uint64, error) {
	v, ok := new(big.Int).SetString(strings.TrimPrefix(s, "0x"), 16)
	if !ok || !v.IsUint64() {
		return 0, fmt.Errorf("bad hex quantity %q", s)
	}
	return v.Uint64(), nil
}

func (c *RPC) ethCall(ctx context.Context, data []byte) ([]byte, error) {
	res, err := c.call(ctx, "eth_call", map[string]string{"to": c.Pool_, "data": "0x" + hex.EncodeToString(data)}, "latest")
	if err != nil {
		return nil, err
	}
	var s string
	if err := json.Unmarshal(res, &s); err != nil {
		return nil, err
	}
	return hex.DecodeString(strings.TrimPrefix(s, "0x"))
}

func (c *RPC) callUint(ctx context.Context, sig string) (*big.Int, error) {
	sel := Selector(sig)
	out, err := c.ethCall(ctx, sel[:])
	if err != nil || len(out) < 32 {
		return nil, fmt.Errorf("call %s failed", sig)
	}
	return new(big.Int).SetBytes(out[:32]), nil
}

func (c *RPC) Head(ctx context.Context) (Head, error) {
	res, err := c.call(ctx, "eth_getBlockByNumber", "latest", false)
	if err != nil {
		return Head{}, err
	}
	var b struct{ Number, Timestamp string }
	if err := json.Unmarshal(res, &b); err != nil {
		return Head{}, err
	}
	n, err := hexU64(b.Number)
	if err != nil {
		return Head{}, err
	}
	ts, err := hexU64(b.Timestamp)
	return Head{Number: n, Timestamp: ts}, err
}

func (c *RPC) Pool(ctx context.Context) (PoolState, error) {
	var p PoolState
	var err error
	for _, f := range []struct {
		sig string
		dst **big.Int
	}{{"reserve0()", &p.R0}, {"reserve1()", &p.R1}, {"virtual0()", &p.V0}, {"virtual1()", &p.V1}, {"currentRoot()", &p.Root}} {
		if *f.dst, err = c.callUint(ctx, f.sig); err != nil {
			return p, err
		}
	}
	nl, err := c.callUint(ctx, "nextLeafIndex()")
	if err != nil {
		return p, err
	}
	bs, err := c.callUint(ctx, "batchSize()")
	if err != nil {
		return p, err
	}
	p.NextLeaf, p.BatchSize = nl.Uint64(), bs.Uint64()
	return p, nil
}

func (c *RPC) Spent(ctx context.Context, nullifier *big.Int) (bool, error) {
	sel := Selector("nullifierSpent(bytes32)")
	arg := make([]byte, 32)
	nullifier.FillBytes(arg)
	out, err := c.ethCall(ctx, append(sel[:], arg...))
	if err != nil || len(out) < 32 {
		return false, errors.New("nullifierSpent call failed")
	}
	return out[31] == 1, nil
}

// Leaves returns NoteInserted(uint32 indexed leafIndex, bytes32 commitment, bytes encryptedNote) events in [from, to].
func (c *RPC) Leaves(ctx context.Context, from, to uint64) ([]Leaf, error) {
	t0 := keccakHex("NoteInserted(uint32,bytes32,bytes)")
	res, err := c.call(ctx, "eth_getLogs", map[string]any{
		"address": c.Pool_, "fromBlock": fmt.Sprintf("0x%x", from), "toBlock": fmt.Sprintf("0x%x", to), "topics": []string{t0},
	})
	if err != nil {
		return nil, err
	}
	var logs []struct {
		Topics      []string `json:"topics"`
		Data        string   `json:"data"`
		BlockNumber string   `json:"blockNumber"`
	}
	if err := json.Unmarshal(res, &logs); err != nil {
		return nil, err
	}
	out := make([]Leaf, 0, len(logs))
	for _, l := range logs {
		if len(l.Topics) < 2 {
			return nil, errors.New("malformed NoteInserted log")
		}
		idx, err := hexU64(l.Topics[1])
		if err != nil {
			return nil, err
		}
		data, err := hex.DecodeString(strings.TrimPrefix(l.Data, "0x"))
		if err != nil || len(data) < 32 {
			return nil, errors.New("malformed NoteInserted data")
		}
		blk, err := hexU64(l.BlockNumber)
		if err != nil {
			return nil, err
		}
		out = append(out, Leaf{Index: idx, Commitment: new(big.Int).SetBytes(data[:32]), Block: blk})
	}
	return out, nil
}

func (c *RPC) Submit(ctx context.Context, calldata []byte) (string, error) {
	if c.Signer == nil {
		return "", errors.New("no signer configured")
	}
	return c.Signer.SignAndSend(ctx, c.Pool_, calldata, c.GasLimit)
}

func keccakHex(sig string) string {
	k := field.Keccak256([]byte(sig))
	return "0x" + hex.EncodeToString(k[:])
}

// HTTPSigner posts {to,data,gasLimit} to a signing gateway and returns the broadcast tx hash.
type HTTPSigner struct {
	URL     string
	Token   string
	ChainID uint64
	HTTP    *http.Client
}

func (s *HTTPSigner) SignAndSend(ctx context.Context, to string, data []byte, gasLimit uint64) (string, error) {
	body, _ := json.Marshal(map[string]any{"chainId": s.ChainID, "to": to, "data": "0x" + hex.EncodeToString(data), "gasLimit": gasLimit})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.URL, bytes.NewReader(body))
	if err != nil {
		return "", err
	}
	req.Header.Set("content-type", "application/json")
	if s.Token != "" {
		req.Header.Set("authorization", "Bearer "+s.Token)
	}
	resp, err := s.HTTP.Do(req)
	if err != nil {
		return "", fmt.Errorf("signer: %w", scrubErr(err))
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return "", fmt.Errorf("signer: status %d", resp.StatusCode)
	}
	var out struct {
		TxHash string `json:"txHash"`
	}
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<16)).Decode(&out); err != nil || out.TxHash == "" {
		return "", errors.New("signer: bad response")
	}
	return out.TxHash, nil
}
