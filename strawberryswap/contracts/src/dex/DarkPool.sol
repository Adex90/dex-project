// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {NoteTree} from "../privacy/NoteTree.sol";
import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {Math} from "../utils/Math.sol";
import {SafeTransfer} from "../utils/SafeTransfer.sol";
import {IERC20Min} from "../interfaces/IERC20Min.sol";
import {IZKVerifier} from "../interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../interfaces/IZKOracle.sol";
import {ITreasury} from "../interfaces/ITreasury.sol";
import {IFlashRebalancer} from "../interfaces/IFlashRebalancer.sol";

/// @title DarkPool — Module 2: Dark Pool AMM (SKELETON)
///
/// CURVE ("adjusted constant product")
///   (R0 + v0) · (R1 + v1) = K          R = real reserves, v = virtual offsets
///   Effective reserves x = R0+v0, y = R1+v1. Offsets flatten the curve around the launch price (less
///   slippage per unit of real capital) while keeping the exact x·y=k algebra. Rounding always favours the
///   pool, so K is monotonically non-decreasing: no fee is needed for the invariant to be safe.
///   The offsets scale with totalShares (v = v_ref · totalShares / refShares): the pool is a single-range
///   concentrated position, so a PROPORTIONAL deposit or withdrawal never moves the price [audit C-2].
///
/// ZERO-FEE SUSTAINABILITY
///   Retail swap fee is the constant 0. Value that a 0-fee constant-product AMM leaks to arbitrageurs
///   (loss-versus-rebalancing, LVR) is *auctioned back*: `flashRebalance` lets any searcher move the pool
///   to the zk-attested oracle price, and the pool PULLS EXTRACTION_BPS of the harvested value from the
///   initiator to the Treasury (which subsidises retail gas) in the same transaction [audit H-1].
///   A batch can never lower the pool's value at the oracle price (I5), so arbitrage TOWARD the oracle is
///   only possible through flashRebalance [audit C-1]. Residual: when the market moves before the oracle
///   updates, a batch trading WITH that move is still free; MAX_ORACLE_AGE bounds that window.
///
/// PRIVACY
///   Swaps are never executed one-by-one on-chain. Users hold *notes* (ZK-ERC1155: assetId 0/1) in this
///   contract's own `NoteTree`. An Automator/aggregator collects signed intents off-chain and submits ONE
///   proof per batch (`settleBatch`). The chain sees only the aggregate flows (in0,in1,out0,out1) plus
///   opaque nullifiers/commitments. Intent nullifiers are keyed by a secret of the intent [audit H-7], so a
///   spent intent cannot be linked to the transaction that created it.
///   LIMITATIONS: small batches leak by arithmetic (the anonymity floor is enforced by the Automator, not
///   on-chain); the transaction that CREATES an intent note is public, so it must be relayed, not sent from
///   the user's own address; whoever holds an intent's opening can order batches around it within its
///   minOut [audit H-4].
///
/// INVARIANTS (checked on-chain every time)
///   I1  K' >= K for settleBatch and flashRebalance.
///   I2  token.balanceOf(pool) >= reserve + noteCustody for both tokens, after every public entrypoint.
///   I3  flashRebalance strictly reduces |spot - oraclePrice|, and the Treasury is paid exactly 80% of the
///       harvest, pulled from the initiator.
///   I4  Σ note value of an asset == noteCustody[asset] (guaranteed by the join-split/batch circuits).
///   I5  settleBatch: (in0 − out0)·P + (in1 − out1) >= 0 at the fresh oracle price P (refund-only batches,
///       which move no value, are exempt and never need the oracle).
///
/// REENTRANCY MODEL: every state-mutating entrypoint is `nonReentrant`. `flashRebalance` makes an
/// external callback before it writes reserves, which is safe ONLY because no other mutating function or
/// price-reading path can run during the callback.
///
/// Internal adversarial review: docs/AUDIT_PHASE8.md. NOT externally audited.
/// Token assumptions: 18 decimals, no fee-on-transfer, no rebasing.
contract DarkPool is NoteTree, ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    // ------------------------------------------------------------------ parameters
    /// @notice Retail trading fee in bps. Axiom: ALWAYS zero. Present as a constant for auditability.
    uint256 public constant RETAIL_FEE_BPS = 0;
    /// @notice Share of the harvested rebalancing value that must go to the Treasury.
    uint256 public constant EXTRACTION_BPS = 8_000;
    uint256 internal constant BPS = 10_000;
    uint256 internal constant WAD = 1e18;
    uint256 public constant MAX_RESERVE = type(uint104).max; // keeps every intermediate product < 2^256
    uint256 public constant MAX_PRICE = 1e24;
    uint256 public constant MAX_ORACLE_AGE = 60; // seconds
    uint256 public constant MINIMUM_LIQUIDITY = 1_000;
    /// @notice Hard ceiling on the circuit's slot count (public inputs = 8 + 2·batchSize).
    uint256 public constant MAX_BATCH_SIZE = 64;
    /// @notice A batch proof carries a coarse prover-supplied clock `tNow` (deadline checks happen in-circuit).
    ///         It must satisfy  tNow <= block.timestamp <= tNow + BATCH_CLOCK_WINDOW.
    uint256 public constant BATCH_CLOCK_WINDOW = 300;

    IERC20Min public immutable token0;
    IERC20Min public immutable token1; // quote asset: Treasury is paid in token1
    IZKVerifier public immutable batchVerifier;
    /// @notice Slot count N of the deployed `batch_swap` circuit (verifier key). Padding slots carry zeros.
    uint256 public immutable batchSize;
    IZKOracle public immutable oracle;
    ITreasury public immutable treasury;
    uint256 internal immutable _v0; // [audit C-2]: virtual reserves at `refShares`; they scale with totalShares
    uint256 internal immutable _v1;

    // ------------------------------------------------------------------ storage
    uint128 public reserve0; // LP-owned reserves (price-setting)
    uint128 public reserve1;
    uint128 public custody0; // value backing users' shielded notes (NOT part of the curve)
    uint128 public custody1;
    uint256 public totalShares;
    mapping(address => uint256) public shares;
    uint256 public refShares; // [audit C-2]: totalShares right after the first deposit
    // [audit H-5]: reserve states a batch proof may still be bound to (hash(X, Y) => block it stopped being current)
    uint256 public constant STATE_WINDOW_BLOCKS = 64;
    bytes32 public currentState;
    mapping(bytes32 => uint256) public stateEndedAt;

    // ------------------------------------------------------------------ types
    /// @dev All flow fields are NET per asset (the circuit keeps gross volumes private): `inK` is value the pool
    ///      RECEIVES of asset K, `outK` value it PAYS; for each asset at most one of the two is non-zero.
    struct Batch {
        bytes proof;
        bytes32 root;
        uint64 tNow; // prover's clock, see BATCH_CLOCK_WINDOW
        bytes32[] nullifiers; // spent intent notes, active slots first, <= batchSize
        bytes32[] commitments; // new output notes (one per intent), same length as nullifiers
        bytes[] encryptedNotes; // one ciphertext per commitment (stealth-addressed)
        uint256 in0;
        uint256 in1;
        uint256 out0;
        uint256 out1;
        uint256 X; // [audit H-5]: the (recent) effective reserves the proof was generated against
        uint256 Y;
    }

    // ------------------------------------------------------------------ events / errors
    event Sync(uint256 reserve0, uint256 reserve1);
    event BatchSettled(uint256 in0, uint256 in1, uint256 out0, uint256 out1, uint32 firstLeaf);
    event LiquidityAdded(address indexed to, uint256 amount0, uint256 amount1, uint256 shares);
    event LiquidityRemoved(address indexed from, address indexed to, uint256 amount0, uint256 amount1);
    event NoteDeposited(uint256 indexed assetId, uint256 amount);
    event NoteWithdrawn(uint256 indexed assetId, address indexed to, uint256 amount);
    event Rebalanced(address indexed searcher, uint256 harvest, uint256 treasuryPaid, uint256 priceBefore);

    error BadTokenOrder();
    error BadDecimals();
    error BadBatch();
    error InvariantBroken();
    error ReserveOverflow();
    error InsufficientLiquidity();
    error InsufficientInput();
    error StaleOracle();
    error BadPrice();
    error DeviationNotReduced();
    error BalanceMismatch();
    error BadAsset();
    error StaleBatchClock();
    error ValueLeak(); // [audit C-1]
    error StaleState(); // [audit H-5]

    constructor(
        IERC20Min token0_,
        IERC20Min token1_,
        uint256 virtual0_,
        uint256 virtual1_,
        IZKVerifier joinSplitVerifier_,
        IZKVerifier batchVerifier_,
        IPoseidonHasher hasher_,
        IZKOracle oracle_,
        ITreasury treasury_,
        uint256 batchSize_
    ) NoteTree(joinSplitVerifier_, hasher_) {
        if (batchSize_ == 0 || batchSize_ > MAX_BATCH_SIZE) revert BadBatch();
        if (address(token0_) >= address(token1_)) revert BadTokenOrder();
        if (token0_.decimals() != 18 || token1_.decimals() != 18) revert BadDecimals();
        if (virtual0_ == 0 || virtual1_ == 0 || virtual0_ > MAX_RESERVE || virtual1_ > MAX_RESERVE) {
            revert ReserveOverflow();
        }
        if (address(batchVerifier_) == address(0) || address(oracle_) == address(0) || address(treasury_) == address(0)) {
            revert ZeroAddress();
        }
        token0 = token0_;
        token1 = token1_;
        _v0 = virtual0_;
        _v1 = virtual1_;
        batchVerifier = batchVerifier_;
        batchSize = batchSize_;
        oracle = oracle_;
        treasury = treasury_;
    }

    // ================================================================== curve maths (views)
    /// @notice Effective reserves (real + virtual).
    function virtual0() public view returns (uint256) {
        uint256 rs = refShares;
        return rs == 0 ? _v0 : _v0 * totalShares / rs;
    }

    function virtual1() public view returns (uint256) {
        uint256 rs = refShares;
        return rs == 0 ? _v1 : _v1 * totalShares / rs;
    }

    function effectiveReserves() public view returns (uint256 x, uint256 y) {
        x = uint256(reserve0) + virtual0();
        y = uint256(reserve1) + virtual1();
    }

    /// @dev [audit H-5]: remember the state that stops being current so in-flight batch proofs stay valid for a window.
    function _markState() private {
        (uint256 x, uint256 y) = effectiveReserves();
        bytes32 h = keccak256(abi.encode(x, y));
        bytes32 old = currentState;
        if (h == old) return;
        if (old != bytes32(0)) stateEndedAt[old] = block.number;
        currentState = h;
    }

    function invariantK() public view returns (uint256) {
        (uint256 x, uint256 y) = effectiveReserves();
        return x * y;
    }

    /// @notice Spot price of token0 in token1 (1e18 fixed point).
    function spotPrice() public view returns (uint256) {
        (uint256 x, uint256 y) = effectiveReserves();
        return y * WAD / x;
    }

    /// @notice Exact-in quote with ZERO fee, rounded down (in the pool's favour). Used by aggregators to
    ///         price intents; the circuit re-derives the same number.
    function getAmountOut(uint256 amountIn, bool zeroForOne) public view returns (uint256) {
        (uint256 x, uint256 y) = effectiveReserves();
        (uint256 rin, uint256 rout) = zeroForOne ? (x, y) : (y, x);
        // out = rout * in / (rin + in)  =>  (rin+in)(rout-out) >= rin*rout
        return rout * amountIn / (rin + amountIn);
    }

    // ================================================================== liquidity (public domain)
    function addLiquidity(uint256 amount0, uint256 amount1, address to)
        external
        nonReentrant
        returns (uint256 minted)
    {
        if (to == address(0)) revert ZeroAddress();
        uint256 r0 = reserve0;
        uint256 r1 = reserve1;
        uint256 ts = totalShares;

        if (ts == 0) {
            minted = Math.sqrt(amount0 * amount1);
            if (minted <= MINIMUM_LIQUIDITY) revert InsufficientInput();
            minted -= MINIMUM_LIQUIDITY;
            totalShares = MINIMUM_LIQUIDITY; // permanently locked: pool can never be fully drained
            shares[address(0)] = MINIMUM_LIQUIDITY;
            ts = MINIMUM_LIQUIDITY;
        } else {
            minted = Math.min(amount0 * ts / r0, amount1 * ts / r1);
            if (minted == 0) revert InsufficientInput();
        }

        // effects
        if (r0 + amount0 > MAX_RESERVE || r1 + amount1 > MAX_RESERVE) revert ReserveOverflow();
        totalShares = ts + minted;
        shares[to] += minted;
        if (refShares == 0) refShares = ts + minted; // [audit C-2]
        reserve0 = uint128(r0 + amount0);
        reserve1 = uint128(r1 + amount1);
        _markState();

        // interactions
        token0.safeTransferFrom(msg.sender, address(this), amount0);
        token1.safeTransferFrom(msg.sender, address(this), amount1);
        _checkBalances();
        emit LiquidityAdded(to, amount0, amount1, minted);
        emit Sync(reserve0, reserve1);
    }

    function removeLiquidity(uint256 shareAmount, address to)
        external
        nonReentrant
        returns (uint256 a0, uint256 a1)
    {
        if (to == address(0)) revert ZeroAddress();
        uint256 ts = totalShares;
        a0 = uint256(reserve0) * shareAmount / ts;
        a1 = uint256(reserve1) * shareAmount / ts;
        if (a0 == 0 && a1 == 0) revert InsufficientInput();

        shares[msg.sender] -= shareAmount; // reverts on underflow
        totalShares = ts - shareAmount;
        reserve0 -= uint128(a0);
        reserve1 -= uint128(a1);
        _markState();

        token0.safeTransfer(to, a0);
        token1.safeTransfer(to, a1);
        _checkBalances();
        emit LiquidityRemoved(msg.sender, to, a0, a1);
        emit Sync(reserve0, reserve1);
    }

    // ================================================================== shielded note ledger (ZK-ERC1155)
    /// @notice public -> note. Revealed amount; creates private balance of `assetId` (0 or 1).
    function depositNote(uint256 assetId, uint256 amount, JoinSplit calldata js) external nonReentrant {
        if (assetId > 1) revert BadAsset();
        if (amount == 0) revert InsufficientInput();
        bytes32 ext = _toField(abi.encode(msg.sender, js.encryptedNotes[0], js.encryptedNotes[1]));
        _applyJoinSplit(js, amount, 0, ext, assetId);

        if (assetId == 0) custody0 += uint128(amount);
        else custody1 += uint128(amount);

        (assetId == 0 ? token0 : token1).safeTransferFrom(msg.sender, address(this), amount);
        _checkBalances();
        emit NoteDeposited(assetId, amount);
    }

    /// @notice note -> public. `recipient` is bound into the proof.
    function withdrawNote(uint256 assetId, uint256 amount, address recipient, JoinSplit calldata js)
        external
        nonReentrant
    {
        if (assetId > 1) revert BadAsset();
        if (amount == 0 || recipient == address(0)) revert InsufficientInput();
        bytes32 ext = _toField(abi.encode(recipient, js.encryptedNotes[0], js.encryptedNotes[1]));
        _applyJoinSplit(js, 0, amount, ext, assetId);

        if (assetId == 0) custody0 -= uint128(amount); // underflow => revert: custody back-stop (I4)
        else custody1 -= uint128(amount);

        (assetId == 0 ? token0 : token1).safeTransfer(recipient, amount);
        _checkBalances();
        emit NoteWithdrawn(assetId, recipient, amount);
    }

    /// @notice Private shielded transfer inside the pool (no public leg).
    function transferNote(uint256 assetId, JoinSplit calldata js) external nonReentrant {
        if (assetId > 1) revert BadAsset();
        _applyJoinSplit(js, 0, 0, _toField(abi.encode(js.encryptedNotes[0], js.encryptedNotes[1])), assetId);
    }

    // ================================================================== private batch settlement
    /// @notice Settles an off-chain batch of shielded swap intents with ONE proof (`circuits/batch_swap`).
    /// @dev    Public inputs, in the order the circuit declares them (8 + 2·batchSize):
    ///           [root, tNow, X, Y, in0, in1, out0, out1, nullifier[0..N), commitment[0..N)]
    ///         with (X, Y) = the effective reserves the proof was generated against, accepted if they are the current
    ///         state or stopped being current at most STATE_WINDOW_BLOCKS ago [audit H-5], and nullifier/commitment
    ///         arrays zero-padded to N. NO hashing of the arrays: putting them straight into the public inputs costs ~7k
    ///         verifier gas each instead of ~100k SNARK constraints each.
    ///         The proof establishes (see the circuit header): every intent note exists under `root`; each user is paid
    ///         ⌊value·price⌋ at the ONE uniform curve price implied by (X, Y, gross flows), at least their own `minOut`,
    ///         to the destination they committed to (or refunded after their deadline); and the NET flow equals
    ///         gross-in minus gross-out. This contract independently enforces, against the CURRENT state, the pool's
    ///         economics (I1: K' >= K; I5: no value leaves the pool at the oracle price) and full backing (I2). An
    ///         aggregator cannot redirect or keep funds; one that knows intent openings can still choose WHEN and in
    ///         which order to settle them, within each user's minOut [audit H-4].
    function settleBatch(Batch calldata b) external nonReentrant {
        _validateBatch(b);
        uint256 r0 = reserve0;
        uint256 r1 = reserve1;
        uint256 kBefore = (r0 + virtual0()) * (r1 + virtual1());

        // ---- checks: proof bound to a RECENT state (P-8); the pool's own protections apply to the CURRENT state
        bytes32 h = keccak256(abi.encode(b.X, b.Y));
        if (h != currentState) {
            uint256 ended = stateEndedAt[h]; // 0 = never was a state of this pool
            if (ended == 0 || block.number > ended + STATE_WINDOW_BLOCKS) revert StaleState();
        }
        if (!batchVerifier.verifyProof(b.proof, _batchInputs(b))) revert InvalidProof();

        // ---- effects: nullifiers first (replay protection), then the tree, then reserves
        uint32 firstLeaf = _consumeBatch(b);
        _applyFlows(b, r0, r1, kBefore);
        // [audit C-1]: a batch may never make the pool lose value at the zk-oracle price. Arbitrage (moving the pool
        // TOWARD the oracle) is therefore only possible through flashRebalance, which pays the Treasury. Pure refund
        // batches move no value and never need the oracle.
        if ((b.in0 | b.in1 | b.out0 | b.out1) != 0) {
            uint256 price = _freshPrice();
            if (b.in0 * price + b.in1 * WAD < b.out0 * price + b.out1 * WAD) revert ValueLeak();
        }

        _checkBalances(); // I2 (no tokens move in a batch: pure re-labelling custody <-> reserve)
        emit BatchSettled(b.in0, b.in1, b.out0, b.out1, firstLeaf);
        emit Sync(reserve0, reserve1);
        // TODO(Phase 3): treasury.reimburseRelayer(msg.sender, gasUsed) — gas subsidy paid from extraction.
    }

    function _validateBatch(Batch calldata b) private view {
        uint256 n = b.nullifiers.length;
        if (n == 0 || n > batchSize || b.commitments.length != n || b.encryptedNotes.length != n) revert BadBatch();
        if (!_knownRoot(b.root)) revert UnknownRoot();
        if (b.tNow > block.timestamp || block.timestamp - b.tNow > BATCH_CLOCK_WINDOW) revert StaleBatchClock();
    }

    function _batchInputs(Batch calldata b) private view returns (uint256[] memory pub) {
        uint256 size = batchSize;
        pub = new uint256[](8 + 2 * size);
        pub[0] = uint256(b.root);
        pub[1] = b.tNow;
        pub[2] = b.X;
        pub[3] = b.Y;
        pub[4] = b.in0;
        pub[5] = b.in1;
        pub[6] = b.out0;
        pub[7] = b.out1;
        uint256 n = b.nullifiers.length;
        for (uint256 i; i < n; ++i) {
            pub[8 + i] = uint256(b.nullifiers[i]);
            pub[8 + size + i] = uint256(b.commitments[i]);
        } // remaining entries stay 0 = padding slots
    }

    function _consumeBatch(Batch calldata b) private returns (uint32 firstLeaf) {
        uint256 n = b.nullifiers.length;
        for (uint256 i; i < n; ++i) {
            if (uint256(b.nullifiers[i]) >= FIELD) revert NotInField();
            if (b.nullifiers[i] == bytes32(0) || b.commitments[i] == bytes32(0)) revert BadBatch(); // [audit L-1]
            _spend(b.nullifiers[i]); // reverts on duplicate, also within the same batch
        }
        firstLeaf = nextLeafIndex;
        bytes32 root;
        for (uint256 i; i < n; ++i) {
            root = _append(b.commitments[i], b.encryptedNotes[i]);
        }
        _pushRoot(root); // [audit H-2]
    }

    /// @dev Moves NET value between note custody and curve reserves. Checked arithmetic = no free lunch.
    function _applyFlows(Batch calldata b, uint256 r0, uint256 r1, uint256 kBefore) private {
        custody0 = uint128(uint256(custody0) - b.in0 + b.out0);
        custody1 = uint128(uint256(custody1) - b.in1 + b.out1);
        uint256 n0 = r0 + b.in0 - b.out0;
        uint256 n1 = r1 + b.in1 - b.out1;
        if (n0 > MAX_RESERVE || n1 > MAX_RESERVE) revert ReserveOverflow();
        // I1: aggregate economics. The circuit's rounding favours the pool => K never decreases.
        if ((n0 + virtual0()) * (n1 + virtual1()) < kBefore) revert InvariantBroken();
        reserve0 = uint128(n0);
        reserve1 = uint128(n1);
        _markState();
    }

    // ================================================================== MEV internalisation
    /// @notice Searchers rebalance the pool towards the zk-attested oracle price and pay the Treasury.
    ///
    /// FLOW (Uniswap-V2-style optimistic transfer):
    ///   1. snapshot state, read zk-attested price P, enforce freshness
    ///   2. optimistically send `amount0Out`/`amount1Out` to `to`
    ///   3. callback `onFlashRebalance` — the searcher repays the pool
    ///   4. recompute reserves from real balances and enforce:
    ///        I1  K' >= K                                (pool never loses curve value)
    ///        I3a |spot' - P| < |spot - P|               (must genuinely rebalance)
    ///   5. PULL exactly 80% · harvest of token1 from msg.sender to the Treasury (harvest = pool value lost at P).
    ///      The initiator must have approved the pool; the allowance doubles as its maximum-payment guard.
    ///      Pulling (rather than measuring the Treasury's balance) is what makes the payment per-pool: nested
    ///      rebalances of pools that share a Treasury each pay in full [audit H-1].
    ///   Harvest = V(before) - V(after), V(R) = x·P + y measured in token1 at the ORACLE price. Because K
    ///   cannot fall, this is exactly the searcher's gross arbitrage profit; the Treasury takes 80% and the
    ///   searcher keeps 20% + inventory risk compensation. Pre-trade quote: `quoteMinPayment`.
    function flashRebalance(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data)
        external
        nonReentrant
    {
        if (to == address(0) || to == address(token0) || to == address(token1)) revert ZeroAddress();
        if (amount0Out == 0 && amount1Out == 0) revert InsufficientInput();

        uint256 r0 = reserve0;
        uint256 r1 = reserve1;
        if (amount0Out >= r0 || amount1Out >= r1) revert InsufficientLiquidity();

        uint256 price = _freshPrice();
        uint256 v0 = virtual0();
        uint256 v1 = virtual1();
        uint256 kBefore = (r0 + v0) * (r1 + v1);
        uint256 devBefore = _deviation((r0 + v0), (r1 + v1), price);
        uint256 valueBefore = _value(r0 + v0, r1 + v1, price);

        // ---- interactions (optimistic)
        if (amount0Out > 0) token0.safeTransfer(to, amount0Out);
        if (amount1Out > 0) token1.safeTransfer(to, amount1Out);
        IFlashRebalancer(to).onFlashRebalance(msg.sender, amount0Out, amount1Out, data);

        // ---- re-derive reserves from balances (never trust the callee)
        uint256 n0 = token0.balanceOf(address(this)) - custody0;
        uint256 n1 = token1.balanceOf(address(this)) - custody1;
        if (n0 > MAX_RESERVE || n1 > MAX_RESERVE || n0 == 0 || n1 == 0) revert ReserveOverflow();

        // I1
        if ((n0 + v0) * (n1 + v1) < kBefore) revert InvariantBroken();
        // I3a
        if (_deviation(n0 + v0, n1 + v1, price) >= devBefore) revert DeviationNotReduced();
        // I3b
        uint256 valueAfter = _value(n0 + v0, n1 + v1, price);
        uint256 harvest = valueBefore > valueAfter ? valueBefore - valueAfter : 0;
        uint256 required = harvest * EXTRACTION_BPS / BPS;

        // ---- effects (guarded by nonReentrant for the whole callback window)
        reserve0 = uint128(n0);
        reserve1 = uint128(n1);
        _markState();
        emit Sync(n0, n1);
        emit Rebalanced(msg.sender, harvest, required, price);
        // [audit H-1]: PULL exactly `required` from the initiator. No shared-balance measurement => no double counting
        // across pools sharing the Treasury, and the searcher no longer has to over-pay a safety margin.
        if (required > 0) token1.safeTransferFrom(msg.sender, address(treasury), required);
        treasury.recordExtraction(address(token1), required, msg.sender); // trusted immutable, informational
    }

    /// @notice Off-chain helper for searchers: minimum Treasury payment (token1) for ending at (n0, n1).
    function quoteMinPayment(uint256 n0, uint256 n1) external view returns (uint256) {
        uint256 price = _freshPrice();
        uint256 vb = _value(uint256(reserve0) + virtual0(), uint256(reserve1) + virtual1(), price);
        uint256 va = _value(n0 + virtual0(), n1 + virtual1(), price);
        return vb > va ? (vb - va) * EXTRACTION_BPS / BPS : 0;
    }

    /// @notice Upper bound of extractable value right now (LVR in token1): xP + y − 2√(x·y·P).
    ///         Bots poll this to decide whether a rebalance is worth the gas.
    function extractableValue() external view returns (uint256) {
        uint256 price = _freshPrice();
        (uint256 x, uint256 y) = effectiveReserves();
        uint256 v = _value(x, y, price);
        uint256 vStar = 2 * Math.sqrt(x * (y * price / WAD));
        return v > vStar ? v - vStar : 0;
    }

    // ================================================================== internals
    function _freshPrice() private view returns (uint256 price) {
        uint64 ts;
        (price, ts) = oracle.latestVerifiedPrice();
        if (ts > block.timestamp || block.timestamp - ts > MAX_ORACLE_AGE) revert StaleOracle();
        if (price == 0 || price > MAX_PRICE) revert BadPrice();
    }

    /// @dev Pool value in token1 at oracle price P: x·P + y. x,y < 2^105, P <= 1e24 => no overflow.
    function _value(uint256 x, uint256 y, uint256 price) private pure returns (uint256) {
        return x * price / WAD + y;
    }

    /// @dev |spot - P| / P in bps, spot = y/x.
    function _deviation(uint256 x, uint256 y, uint256 price) private pure returns (uint256) {
        return Math.absDiff(y * WAD / x, price) * BPS / price;
    }

    /// @dev I2: real token balances must COVER curve reserves + note custody. `>=` (not `==`) so a
    ///      donation cannot brick the pool; surplus is absorbed into reserves at the next flashRebalance.
    function _checkBalances() private view {
        if (
            token0.balanceOf(address(this)) < uint256(reserve0) + custody0
                || token1.balanceOf(address(this)) < uint256(reserve1) + custody1
        ) revert BalanceMismatch();
    }
}
