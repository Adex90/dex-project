package chain

import (
	"bufio"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"net"
	"strings"
	"time"
)

// VaultSigner signs through the automator vault (vaults/threshold-signer --role automator): a local process that holds
// the automator's key in AWS KMS and signs DarkPool.settleBatch only, within its caps and its nonce rule. It builds
// the transaction (the pending nonce, capped fees), has the vault sign it over the vault's Unix socket, and broadcasts
// it through the chain RPC. The Automator still holds no key, and the vault is never reached over the network.
type VaultSigner struct {
	Socket            string        // the automator vault's socket
	Timeout           time.Duration // each request to the vault
	RPC               *RPC          // nonce, fees and the broadcast
	ChainID           uint64
	MaxFeeWei         *big.Int // max fee per gas cap
	MaxPriorityFeeWei *big.Int // priority fee cap
	address           string   // the vault's address, from Connect
}

// VaultPolicy is what the vault reports it signs.
type VaultPolicy struct {
	Role         string `json:"role"`
	ChainID      uint64 `json:"chain_id"`
	To           string `json:"to"`
	MaxGas       uint64 `json:"max_gas"`
	MaxFeePerGas string `json:"max_fee_per_gas"`
}

// request sends one JSON line on a fresh connection and reads one back.
func (v *VaultSigner) request(ctx context.Context, method string, params any) (json.RawMessage, error) {
	ctx, cancel := context.WithTimeout(ctx, v.Timeout)
	defer cancel()
	var d net.Dialer
	conn, err := d.DialContext(ctx, "unix", v.Socket)
	if err != nil {
		return nil, fmt.Errorf("the vault at %s: %w", v.Socket, err)
	}
	defer conn.Close()
	if deadline, ok := ctx.Deadline(); ok {
		_ = conn.SetDeadline(deadline)
	}
	req := map[string]any{"id": 1, "method": method}
	if params != nil {
		req["params"] = params
	}
	line, _ := json.Marshal(req)
	if _, err := conn.Write(append(line, '\n')); err != nil {
		return nil, fmt.Errorf("the vault: %w", err)
	}
	reply, err := bufio.NewReaderSize(conn, 1<<16).ReadBytes('\n')
	if err != nil {
		return nil, fmt.Errorf("the vault did not answer %s: %w", method, err)
	}
	var out struct {
		ID     *uint64         `json:"id"`
		Result json.RawMessage `json:"result"`
		Error  *struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(reply, &out); err != nil {
		return nil, errors.New("the vault answered garbage")
	}
	if out.Error != nil {
		return nil, fmt.Errorf("the vault refused (%s): %s", out.Error.Code, out.Error.Message)
	}
	if out.ID == nil || *out.ID != 1 {
		return nil, errors.New("the vault answered another request")
	}
	return out.Result, nil
}

// Connect asks the vault for its address and its policy, and refuses a vault that would refuse every batch: another
// role's (its key is not the automator's), another chain, another pool, or caps below this Automator's.
func (v *VaultSigner) Connect(ctx context.Context, pool string, gasLimit uint64) error {
	raw, err := v.request(ctx, "address", nil)
	if err != nil {
		return err
	}
	var address struct {
		Address string `json:"address"`
	}
	if json.Unmarshal(raw, &address) != nil || !isAddress(address.Address) {
		return errors.New("the vault's address reply has no address")
	}
	raw, err = v.request(ctx, "policy", nil)
	if err != nil {
		return err
	}
	var p VaultPolicy
	if err := json.Unmarshal(raw, &p); err != nil {
		return errors.New("the vault's policy does not parse")
	}
	maxFee, ok := new(big.Int).SetString(p.MaxFeePerGas, 10)
	switch {
	case p.Role != "automator":
		return fmt.Errorf("the vault at %s holds the %s key: the Automator signs through the automator vault", v.Socket, p.Role)
	case p.ChainID != v.ChainID:
		return fmt.Errorf("the vault signs for chain %d, the Automator settles on chain %d", p.ChainID, v.ChainID)
	case !strings.EqualFold(p.To, pool):
		return fmt.Errorf("the vault signs calls to %s, the Automator settles on %s", p.To, pool)
	case p.MaxGas < gasLimit:
		return fmt.Errorf("the Automator's gas limit %d is above the vault's cap %d", gasLimit, p.MaxGas)
	case !ok || maxFee.Cmp(v.MaxFeeWei) < 0:
		return fmt.Errorf("the Automator's max fee %s wei is above the vault's cap %s", v.MaxFeeWei, p.MaxFeePerGas)
	}
	v.address = address.Address
	return nil
}

// Address is the vault's (the automator's) address, once connected.
func (v *VaultSigner) Address() string { return v.address }

func isAddress(s string) bool {
	b, err := hex.DecodeString(strings.TrimPrefix(s, "0x"))
	return err == nil && len(b) == 20 && strings.HasPrefix(s, "0x")
}

func hexBig(raw json.RawMessage) (*big.Int, error) {
	var s string
	if err := json.Unmarshal(raw, &s); err != nil {
		return nil, err
	}
	v, ok := new(big.Int).SetString(strings.TrimPrefix(s, "0x"), 16)
	if !ok || v.Sign() < 0 {
		return nil, fmt.Errorf("bad hex quantity %q", s)
	}
	return v, nil
}

// fees are the capped EIP-1559 fees: the node's priority fee (at most the cap), and twice the base fee plus it (at
// most the cap). A base fee at or above the cap is refused: such a transaction would wait unmined.
func (v *VaultSigner) fees(ctx context.Context) (priority, maxFee *big.Int, err error) {
	raw, err := v.RPC.call(ctx, "eth_getBlockByNumber", "latest", false)
	if err != nil {
		return nil, nil, err
	}
	var head struct {
		BaseFee json.RawMessage `json:"baseFeePerGas"`
	}
	if err := json.Unmarshal(raw, &head); err != nil || head.BaseFee == nil {
		return nil, nil, errors.New("the latest block has no base fee")
	}
	base, err := hexBig(head.BaseFee)
	if err != nil {
		return nil, nil, err
	}
	if base.Cmp(v.MaxFeeWei) >= 0 {
		return nil, nil, fmt.Errorf("base fee %s wei at or above the cap %s", base, v.MaxFeeWei)
	}
	raw, err = v.RPC.call(ctx, "eth_maxPriorityFeePerGas")
	if err != nil {
		return nil, nil, err
	}
	priority, err = hexBig(raw)
	if err != nil {
		return nil, nil, err
	}
	if priority.Cmp(v.MaxPriorityFeeWei) > 0 {
		priority = new(big.Int).Set(v.MaxPriorityFeeWei)
	}
	maxFee = new(big.Int).Add(new(big.Int).Lsh(base, 1), priority)
	if maxFee.Cmp(v.MaxFeeWei) > 0 {
		maxFee = new(big.Int).Set(v.MaxFeeWei)
	}
	if priority.Cmp(maxFee) > 0 {
		priority = new(big.Int).Set(maxFee)
	}
	return priority, maxFee, nil
}

// SignAndSend builds the transaction (the pending nonce, capped fees), has the vault sign it and broadcasts it. The
// node's hash must be the vault's.
func (v *VaultSigner) SignAndSend(ctx context.Context, to string, data []byte, gasLimit uint64) (string, error) {
	if v.address == "" {
		return "", errors.New("the vault signer is not connected")
	}
	var dest [20]byte
	toBytes, err := hex.DecodeString(strings.TrimPrefix(to, "0x"))
	if err != nil || len(toBytes) != 20 {
		return "", fmt.Errorf("bad destination %q", to)
	}
	copy(dest[:], toBytes)
	raw, err := v.RPC.call(ctx, "eth_getTransactionCount", v.address, "pending")
	if err != nil {
		return "", err
	}
	var nonceHex string
	if err := json.Unmarshal(raw, &nonceHex); err != nil {
		return "", errors.New("bad nonce")
	}
	nonce, err := hexU64(nonceHex)
	if err != nil {
		return "", err
	}
	priority, maxFee, err := v.fees(ctx)
	if err != nil {
		return "", err
	}
	tx := Tx1559{ChainID: v.ChainID, Nonce: nonce, MaxPriorityFeePerGas: priority, MaxFeePerGas: maxFee,
		GasLimit: gasLimit, To: dest, Data: data}
	raw, err = v.request(ctx, "sign_transaction", map[string]string{"tx": "0x" + hex.EncodeToString(tx.SigningPayload())})
	if err != nil {
		return "", err
	}
	var signed struct {
		SignedTx string `json:"signed_tx"`
		TxHash   string `json:"tx_hash"`
	}
	if json.Unmarshal(raw, &signed) != nil || signed.SignedTx == "" || signed.TxHash == "" {
		return "", errors.New("the vault's signature reply is incomplete")
	}
	raw, err = v.RPC.call(ctx, "eth_sendRawTransaction", signed.SignedTx)
	if err != nil {
		return "", err
	}
	var sent string
	if json.Unmarshal(raw, &sent) != nil || !strings.EqualFold(sent, signed.TxHash) {
		return "", fmt.Errorf("the node accepted %s, the vault signed %s", sent, signed.TxHash)
	}
	return signed.TxHash, nil
}
