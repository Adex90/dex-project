// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {NoteTree} from "../privacy/NoteTree.sol";
import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {IZKVerifier} from "../interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../interfaces/IPoseidonHasher.sol";

/// @title Berrycoin (BERRY) — Module 1: Hard Money
/// @notice ZK-ERC20. Fixed supply, minted once in the constructor (genesis block), with NO mint path.
///
/// AXIOMS ENFORCED BY CONSTRUCTION
///  A1. Zero inflation: `MAX_SUPPLY` is a compile-time constant and is credited exactly once, in the
///      constructor. There is no `mint`, no `burn`, no owner, no admin, no proxy/upgrade hook, no
///      `selfdestruct`/`delegatecall`. "Destroying mint()" here means the function does not exist in the
///      deployed bytecode; it cannot be re-introduced because the code is immutable.
///  A2. Conservation invariant (checked on every state change that touches it):
///         Σ publicBalance[a]  +  shieldedSupply  ==  MAX_SUPPLY
///      `shieldedSupply` is the value currently locked inside the note tree.
///  A3. Soundness back-stop: even if a circuit/trusted-setup bug let an attacker forge a proof, `unshield`
///      can never release more than `shieldedSupply`. A forged proof can steal shielded funds but can NOT
///      inflate the total supply above MAX_SUPPLY.
///
/// TWO BALANCE DOMAINS
///  * public  : classic ERC-20 (`transfer`, `approve`, `transferFrom`) — exchange/L1-bridge compatible.
///  * shielded: UTXO notes in the Merkle tree of `NoteTree`. `transferZK` moves value between notes
///              without revealing sender, receiver or amount.
///  `shield` / `unshield` are the only bridges between domains and necessarily reveal their amount.
///
/// NOT AUDITED. Skeleton for Phase 1: circuits, verifier keys and the Poseidon contract are external.
contract Berrycoin is NoteTree, ReentrancyGuard {
    // ------------------------------------------------------------------ ERC-20 metadata
    string public constant name = "Berrycoin";
    string public constant symbol = "BERRY";
    uint8 public constant decimals = 18;

    /// @notice Hard cap AND total supply. Placeholder value: 21,000,000 BERRY (governance of the founders
    ///         must fix the final number BEFORE deployment; it cannot change afterwards).
    uint256 public constant MAX_SUPPLY = 21_000_000 * 1e18;
    uint256 public constant totalSupply = MAX_SUPPLY;

    /// @notice Block in which the supply was minted. Immutable proof that genesis happened exactly once.
    uint256 public immutable genesisBlock;

    /// @notice ZK-ERC1155 asset id used inside join-split proofs (single asset => 0).
    uint256 internal constant ASSET_ID = 0;

    // ------------------------------------------------------------------ storage
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    /// @notice Value currently held inside shielded notes (sum of unspent notes). Public by necessity.
    uint256 public shieldedSupply;

    // ------------------------------------------------------------------ events / errors
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event Shielded(address indexed from, uint256 amount);
    event Unshielded(address indexed to, uint256 amount);
    /// @dev No amount, no addresses: only opaque commitments/nullifiers are emitted (see NoteTree events).
    event TransferZK();

    error InsufficientBalance();
    error InsufficientAllowance();
    error InvalidRecipient();
    error AmountZero();
    error ShieldedSupplyExceeded();

    /// @param genesisHolder receives the entire supply (should be a vesting/distribution contract, MPC/TSS
    ///        multisig or the shielded-genesis helper — never an EOA hot key).
    constructor(address genesisHolder, IZKVerifier verifier_, IPoseidonHasher hasher_)
        NoteTree(verifier_, hasher_)
    {
        if (genesisHolder == address(0)) revert InvalidRecipient();
        genesisBlock = block.number;
        balanceOf[genesisHolder] = MAX_SUPPLY; // the one and only issuance, forever
        emit Transfer(address(0), genesisHolder, MAX_SUPPLY);
    }

    // ================================================================== public ERC-20
    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed != type(uint256).max) {
            if (allowed < amount) revert InsufficientAllowance();
            unchecked {
                allowance[from][msg.sender] = allowed - amount;
            }
        }
        _transfer(from, to, amount);
        return true;
    }

    function _transfer(address from, address to, uint256 amount) private {
        if (to == address(0) || to == address(this)) revert InvalidRecipient();
        uint256 bal = balanceOf[from];
        if (bal < amount) revert InsufficientBalance();
        unchecked {
            balanceOf[from] = bal - amount;
            balanceOf[to] += amount; // cannot overflow: Σ balances <= MAX_SUPPLY (A2)
        }
        emit Transfer(from, to, amount);
    }

    // ================================================================== shielded domain
    /// @notice public -> shielded. Debits `msg.sender`, creates two notes (one may be dummy-sized 0).
    /// @dev    Circuit instance: publicIn = amount, publicOut = 0. `extDataHash` binds `msg.sender` and
    ///         the ciphertexts, so the proof cannot be replayed by another account.
    function shield(uint256 amount, JoinSplit calldata js) external nonReentrant {
        if (amount == 0) revert AmountZero();
        bytes32 ext = _extHash(msg.sender, js);

        // checks + effects on the tree (reverts on bad proof / reused nullifier)
        _applyJoinSplit(js, amount, 0, ext, ASSET_ID);

        // effects on balances
        uint256 bal = balanceOf[msg.sender];
        if (bal < amount) revert InsufficientBalance();
        unchecked {
            balanceOf[msg.sender] = bal - amount;
        }
        shieldedSupply += amount;
        emit Shielded(msg.sender, amount);
        emit Transfer(msg.sender, address(this), amount); // keeps explorers' supply accounting coherent
    }

    /// @notice shielded -> shielded. THE privacy primitive: reveals no sender, receiver or amount.
    /// @dev    Circuit instance: publicIn = publicOut = 0. Anyone (relayer) may submit it; with a 0-fee
    ///         model the relayer is gas-subsidised by the Treasury, so no fee note is needed.
    function transferZK(JoinSplit calldata js) external nonReentrant {
        bytes32 ext = _extHash(address(0), js); // not bound to a caller: relayers are interchangeable
        _applyJoinSplit(js, 0, 0, ext, ASSET_ID);
        emit TransferZK();
    }

    /// @notice shielded -> public. Credits `recipient` with `amount` (revealed). Recipient is bound into
    ///         the proof, so a relayer/mempool observer cannot redirect it.
    function unshield(uint256 amount, address recipient, JoinSplit calldata js) external nonReentrant {
        if (amount == 0) revert AmountZero();
        if (recipient == address(0) || recipient == address(this)) revert InvalidRecipient();
        bytes32 ext = _toField(abi.encode(recipient, js.encryptedNotes[0], js.encryptedNotes[1]));

        // A3: hard ceiling independent of circuit soundness (checked BEFORE trusting the proof result).
        if (amount > shieldedSupply) revert ShieldedSupplyExceeded();

        _applyJoinSplit(js, 0, amount, ext, ASSET_ID);

        shieldedSupply -= amount;
        unchecked {
            balanceOf[recipient] += amount; // <= MAX_SUPPLY by A2/A3
        }
        emit Unshielded(recipient, amount);
        emit Transfer(address(this), recipient, amount);
    }

    function _extHash(address caller, JoinSplit calldata js) private pure returns (bytes32) {
        return _toField(abi.encode(caller, js.encryptedNotes[0], js.encryptedNotes[1]));
    }
}
