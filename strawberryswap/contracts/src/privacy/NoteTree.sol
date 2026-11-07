// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IZKVerifier} from "../interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../interfaces/IPoseidonHasher.sol";

/// @title NoteTree
/// @notice Shared shielded-pool engine for ZK-ERC20 (Berrycoin) and ZK-ERC1155 (DarkPool note ledger).
///
/// MODEL (UTXO "notes", Zcash-Sapling / Tornado-Nova family)
///   note       = (assetId, value, ownerPubKey, blinding)
///   commitment = Poseidon(assetId, value, ownerPubKey, blinding)          -> leaf of the Merkle tree
///   nullifier  = Poseidon(commitment, leafIndex, ownerPrivKey)            -> published when spent
///
/// JOIN-SPLIT CIRCUIT (one circuit; shield / transfer / unshield differ only in public amounts)
///   private: 2 input notes (value 0 allowed => dummy, membership check skipped), 2 output notes
///   public : [root, nullifier0, nullifier1, commitment0, commitment1, publicIn, publicOut, extDataHash, assetId]
///   proves : * every non-dummy input note is a leaf under `root` and the prover knows its private key
///            * nullifiers are correctly derived and nullifier0 != nullifier1
///            * sum(inputs) + publicIn == sum(outputs) + publicOut         (value conservation)
///            * all values are range-checked to 128 bits                    (no field wrap-around)
///            * every note carries `assetId`
///   `extDataHash` binds recipient / relayer / ciphertexts to the proof so a mempool observer cannot
///   re-target a valid proof (front-running defence).
///
/// STEALTH ADDRESSES: `encryptedNote` is an ECIES ciphertext under the recipient's *stealth* viewing key,
///   prefixed by the sender's ephemeral public key (ERC-5564 style). Wallets scan `NoteInserted` events
///   with their viewing key; no on-chain link between sender, recipient and receiving address exists.
///
/// PRIVACY BOUNDARY (honest statement): amounts and addresses are hidden *inside* the pool. Entering
///   (publicIn > 0) and leaving (publicOut > 0) reveal the public amount by definition. Origin/destination
///   hiding for those two edges relies on anonymity-set size, relayers and stealth addresses.
abstract contract NoteTree {
    // ---------------------------------------------------------------- constants
    /// @dev BN254 scalar field order.
    uint256 internal constant FIELD =
        21888242871839275222246405745257275088548364400416034343698204186575808495617;
    uint8 public constant DEPTH = 20; // 2^20 ≈ 1.05M notes per pool
    uint32 public constant ROOT_HISTORY = 128;

    // ---------------------------------------------------------------- immutables
    IZKVerifier public immutable joinSplitVerifier;
    IPoseidonHasher internal immutable hasher;

    // ---------------------------------------------------------------- storage
    uint32 public nextLeafIndex;
    uint32 private _rootCursor;
    bytes32[DEPTH] private _filled; // right-most filled node per level
    bytes32[DEPTH] private _zeros; // empty-subtree hashes per level
    bytes32[ROOT_HISTORY] private _rootRing;
    mapping(bytes32 => bool) public isKnownRoot;
    mapping(bytes32 => bool) public nullifierSpent;

    // ---------------------------------------------------------------- types
    struct JoinSplit {
        bytes proof;
        bytes32 root;
        bytes32[2] nullifiers;
        bytes32[2] commitments;
        bytes[2] encryptedNotes;
    }

    // ---------------------------------------------------------------- events / errors
    event NoteInserted(uint32 indexed leafIndex, bytes32 commitment, bytes encryptedNote);
    event Nullified(bytes32 indexed nullifier);

    error UnknownRoot();
    error NullifierReuse();
    error NotInField();
    error InvalidProof();
    error TreeFull();
    error ZeroAddress();

    constructor(IZKVerifier verifier_, IPoseidonHasher hasher_) {
        if (address(verifier_) == address(0) || address(hasher_) == address(0)) revert ZeroAddress();
        joinSplitVerifier = verifier_;
        hasher = hasher_;

        bytes32 z = bytes32(uint256(keccak256("StrawberrySwap.NoteTree.zero")) % FIELD);
        for (uint256 i; i < DEPTH; ++i) {
            _zeros[i] = z;
            _filled[i] = z;
            z = hasher_.poseidon(z, z);
        }
        _pushRoot(z);
    }

    // ---------------------------------------------------------------- core: join-split
    /// @dev Validates and applies one join-split. Pure state transition + a `view` verifier call, no token
    ///      movement, so callers do their own accounting AFTER this returns (checks-effects-interactions).
    function _applyJoinSplit(
        JoinSplit calldata js,
        uint256 publicIn,
        uint256 publicOut,
        bytes32 extDataHash,
        uint256 assetId
    ) internal {
        // ---- checks
        if (!isKnownRoot[js.root]) revert UnknownRoot();
        bytes32 n0 = js.nullifiers[0];
        bytes32 n1 = js.nullifiers[1];
        if (n0 == n1 || nullifierSpent[n0] || nullifierSpent[n1]) revert NullifierReuse();
        if (
            uint256(n0) >= FIELD || uint256(n1) >= FIELD || uint256(js.commitments[0]) >= FIELD
                || uint256(js.commitments[1]) >= FIELD || uint256(extDataHash) >= FIELD || publicIn >= FIELD
                || publicOut >= FIELD
        ) revert NotInField();

        uint256[] memory pub = new uint256[](9);
        pub[0] = uint256(js.root);
        pub[1] = uint256(n0);
        pub[2] = uint256(n1);
        pub[3] = uint256(js.commitments[0]);
        pub[4] = uint256(js.commitments[1]);
        pub[5] = publicIn;
        pub[6] = publicOut;
        pub[7] = uint256(extDataHash);
        pub[8] = assetId;
        if (!joinSplitVerifier.verifyProof(js.proof, pub)) revert InvalidProof();

        // ---- effects
        _spend(n0);
        _spend(n1);
        _insert(js.commitments[0], js.encryptedNotes[0]);
        _insert(js.commitments[1], js.encryptedNotes[1]);
    }

    function _spend(bytes32 nullifier) internal {
        if (nullifierSpent[nullifier]) revert NullifierReuse();
        nullifierSpent[nullifier] = true;
        emit Nullified(nullifier);
    }

    /// @dev Reduces arbitrary calldata into a field element for `extDataHash`.
    function _toField(bytes memory data) internal pure returns (bytes32) {
        return bytes32(uint256(keccak256(data)) % FIELD);
    }

    // ---------------------------------------------------------------- incremental Merkle tree
    function _insert(bytes32 leaf, bytes calldata encryptedNote) internal {
        uint32 index = nextLeafIndex;
        if (index >= uint32(1) << DEPTH) revert TreeFull();
        if (uint256(leaf) >= FIELD) revert NotInField();

        uint32 cur = index;
        bytes32 node = leaf;
        for (uint256 i; i < DEPTH; ++i) {
            if (cur & 1 == 0) {
                _filled[i] = node;
                node = hasher.poseidon(node, _zeros[i]);
            } else {
                node = hasher.poseidon(_filled[i], node);
            }
            cur >>= 1;
        }
        nextLeafIndex = index + 1;
        _pushRoot(node);
        emit NoteInserted(index, leaf, encryptedNote);
    }

    function _pushRoot(bytes32 root) private {
        uint32 c = (_rootCursor + 1) % ROOT_HISTORY;
        bytes32 evicted = _rootRing[c];
        if (evicted != bytes32(0)) isKnownRoot[evicted] = false;
        _rootRing[c] = root;
        isKnownRoot[root] = true;
        _rootCursor = c;
    }

    function currentRoot() public view returns (bytes32) {
        return _rootRing[_rootCursor];
    }
}
