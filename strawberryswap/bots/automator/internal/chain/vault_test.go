package chain

import (
	"bufio"
	"context"
	"encoding/hex"
	"encoding/json"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

const pool = "0x9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0"

// The signing payload of testdata/eip1559.vault.json, as alloy encodes it (the vault's tests check the same bytes).
func TestSigningPayloadIsAlloys(t *testing.T) {
	raw, err := os.ReadFile("../../testdata/eip1559.vault.json")
	if err != nil {
		t.Fatal(err)
	}
	var v struct {
		ChainID              uint64 `json:"chainId"`
		Nonce                uint64 `json:"nonce"`
		MaxPriorityFeePerGas string `json:"maxPriorityFeePerGas"`
		MaxFeePerGas         string `json:"maxFeePerGas"`
		GasLimit             uint64 `json:"gasLimit"`
		To                   string `json:"to"`
		Payload              string `json:"payload"`
	}
	if err := json.Unmarshal(raw, &v); err != nil {
		t.Fatal(err)
	}
	calldata, err := os.ReadFile("../../testdata/settleBatch.calldata.hex")
	if err != nil {
		t.Fatal(err)
	}
	data, _ := hex.DecodeString(strings.TrimPrefix(strings.TrimSpace(string(calldata)), "0x"))
	prio, _ := new(big.Int).SetString(v.MaxPriorityFeePerGas, 10)
	maxFee, _ := new(big.Int).SetString(v.MaxFeePerGas, 10)
	tx := Tx1559{ChainID: v.ChainID, Nonce: v.Nonce, MaxPriorityFeePerGas: prio, MaxFeePerGas: maxFee, GasLimit: v.GasLimit, Data: data}
	to, _ := hex.DecodeString(strings.TrimPrefix(v.To, "0x"))
	copy(tx.To[:], to)
	if got := "0x" + hex.EncodeToString(tx.SigningPayload()); got != v.Payload {
		t.Fatalf("payload differs from alloy's:\n got %s\nwant %s", got[:120], v.Payload[:120])
	}
}

func TestRLPIsCanonical(t *testing.T) {
	cases := []struct {
		got  []byte
		want string
	}{
		{rlpUint(big.NewInt(0)), "80"},
		{rlpUint(big.NewInt(1)), "01"},
		{rlpUint(big.NewInt(127)), "7f"},
		{rlpUint(big.NewInt(128)), "8180"},
		{rlpUint(big.NewInt(1024)), "820400"},
		{rlpString(nil), "80"},
		{rlpString(make([]byte, 55)), "b7" + strings.Repeat("00", 55)},
		{rlpString(make([]byte, 56)), "b838" + strings.Repeat("00", 56)},
		{rlpString(make([]byte, 1024)), "b90400" + strings.Repeat("00", 1024)},
		{rlpList(), "c0"},
		{rlpList(rlpUint(big.NewInt(1)), rlpUint(big.NewInt(2))), "c20102"},
	}
	for i, c := range cases {
		if got := hex.EncodeToString(c.got); got != c.want {
			t.Errorf("case %d: %s, want %s", i, got, c.want)
		}
	}
}

// A vault on a Unix socket: address, policy and sign_transaction, recording the payloads it is asked to sign.
type fakeVault struct {
	socket string
	policy map[string]any
	mu     sync.Mutex
	asked  []string
}

func startVault(t *testing.T, policy map[string]any) *fakeVault {
	t.Helper()
	dir, err := os.MkdirTemp("", "vault")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	v := &fakeVault{socket: filepath.Join(dir, "automator.sock"), policy: policy}
	ln, err := net.Listen("unix", v.socket)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer conn.Close()
				line, err := bufio.NewReader(conn).ReadBytes('\n')
				if err != nil {
					return
				}
				var req struct {
					ID     uint64            `json:"id"`
					Method string            `json:"method"`
					Params map[string]string `json:"params"`
				}
				_ = json.Unmarshal(line, &req)
				var result any
				switch req.Method {
				case "address":
					result = map[string]string{"address": "0x00000000000000000000000000000000000a0702"}
				case "policy":
					result = v.policy
				default:
					v.mu.Lock()
					v.asked = append(v.asked, req.Params["tx"])
					v.mu.Unlock()
					result = map[string]string{"signed_tx": "0x02c0ffee", "tx_hash": "0x" + strings.Repeat("ab", 32)}
				}
				reply, _ := json.Marshal(map[string]any{"id": req.ID, "result": result})
				_, _ = conn.Write(append(reply, '\n'))
			}()
		}
	}()
	return v
}

// A node answering the reads the signer makes, and recording what it broadcasts.
func startNode(t *testing.T, baseFee string) (*RPC, *[]string) {
	t.Helper()
	var sent []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var req struct {
			ID     uint64 `json:"id"`
			Method string `json:"method"`
			Params []any  `json:"params"`
		}
		_ = json.NewDecoder(r.Body).Decode(&req)
		var result any
		switch req.Method {
		case "eth_getTransactionCount":
			result = "0x7"
		case "eth_getBlockByNumber":
			result = map[string]string{"baseFeePerGas": baseFee}
		case "eth_maxPriorityFeePerGas":
			result = "0x3b9aca00" // 1 gwei: above the 0.1 gwei cap
		case "eth_sendRawTransaction":
			sent = append(sent, req.Params[0].(string))
			result = "0x" + strings.Repeat("AB", 32)
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"jsonrpc": "2.0", "id": req.ID, "result": result})
	}))
	t.Cleanup(srv.Close)
	return &RPC{URL: srv.URL, Pool_: pool, HTTP: srv.Client()}, &sent
}

func automatorPolicy() map[string]any {
	return map[string]any{"role": "automator", "chain_id": 421614, "to": strings.ToLower(pool), "max_gas": 5000000,
		"max_fee_per_gas": "500000000000"}
}

func signer(socket string, rpc *RPC) *VaultSigner {
	return &VaultSigner{Socket: socket, Timeout: 2 * time.Second, RPC: rpc, ChainID: 421614,
		MaxFeeWei: big.NewInt(2_000_000_000), MaxPriorityFeeWei: big.NewInt(100_000_000)}
}

func TestTheVaultSignsAndTheAutomatorBroadcasts(t *testing.T) {
	vault := startVault(t, automatorPolicy())
	rpc, sent := startNode(t, "0x2faf080") // 50,000,000 wei
	s := signer(vault.socket, rpc)
	ctx := context.Background()
	if _, err := s.SignAndSend(ctx, pool, []byte{0x25, 0xa5, 0xb9, 0x41}, 5_000_000); err == nil {
		t.Fatal("signed before connecting")
	}
	if err := s.Connect(ctx, pool, 5_000_000); err != nil {
		t.Fatal(err)
	}
	hash, err := s.SignAndSend(ctx, pool, []byte{0x25, 0xa5, 0xb9, 0x41}, 5_000_000)
	if err != nil {
		t.Fatal(err)
	}
	if hash != "0x"+strings.Repeat("ab", 32) || len(*sent) != 1 || (*sent)[0] != "0x02c0ffee" {
		t.Fatalf("hash %s, sent %v", hash, *sent)
	}
	// The vault was asked to sign the pending nonce, the capped priority fee and 2 × base + priority.
	want := Tx1559{ChainID: 421614, Nonce: 7, MaxPriorityFeePerGas: big.NewInt(100_000_000),
		MaxFeePerGas: big.NewInt(2*50_000_000 + 100_000_000), GasLimit: 5_000_000, Data: []byte{0x25, 0xa5, 0xb9, 0x41}}
	to, _ := hex.DecodeString(strings.TrimPrefix(pool, "0x"))
	copy(want.To[:], to)
	if len(vault.asked) != 1 || vault.asked[0] != "0x"+hex.EncodeToString(want.SigningPayload()) {
		t.Fatalf("the vault was asked %v", vault.asked)
	}
}

func TestFeesAboveTheCapAreNotSigned(t *testing.T) {
	vault := startVault(t, automatorPolicy())
	rpc, sent := startNode(t, "0x77359400") // 2 gwei: the cap itself
	s := signer(vault.socket, rpc)
	if err := s.Connect(context.Background(), pool, 5_000_000); err != nil {
		t.Fatal(err)
	}
	_, err := s.SignAndSend(context.Background(), pool, []byte{1}, 5_000_000)
	if err == nil || !strings.Contains(err.Error(), "above the cap") || len(vault.asked) != 0 || len(*sent) != 0 {
		t.Fatalf("err %v, asked %v, sent %v", err, vault.asked, *sent)
	}
}

func TestAVaultThatWouldRefuseEveryBatchIsRefused(t *testing.T) {
	cases := []struct {
		change func(map[string]any)
		why    string
	}{
		{func(p map[string]any) { p["role"] = "operator" }, "holds the operator key"},
		{func(p map[string]any) { p["chain_id"] = 1 }, "chain 1"},
		{func(p map[string]any) { p["to"] = "0x0000000000000000000000000000000000000001" }, "signs calls to"},
		{func(p map[string]any) { p["max_gas"] = 1000 }, "gas limit"},
		{func(p map[string]any) { p["max_fee_per_gas"] = "1" }, "max fee"},
	}
	for _, c := range cases {
		policy := automatorPolicy()
		c.change(policy)
		vault := startVault(t, policy)
		rpc, _ := startNode(t, "0x1")
		err := signer(vault.socket, rpc).Connect(context.Background(), pool, 5_000_000)
		if err == nil || !strings.Contains(err.Error(), c.why) {
			t.Errorf("%s: %v", c.why, err)
		}
	}
	err := signer(filepath.Join(os.TempDir(), "no-such-vault.sock"), nil).Connect(context.Background(), pool, 1)
	if err == nil || !strings.Contains(err.Error(), "the vault at") {
		t.Errorf("missing vault: %v", err)
	}
}
