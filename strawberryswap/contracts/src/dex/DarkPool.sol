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
///   (R0 + v0) · (R1 + v1) = K          R = real reserves, v = immutable virtual offsets
///   Effective reserves x = R0+v0, y = R1+v1. Offsets flatten the curve around the launch price (less
///   slippage per unit of real capital) while keeping the exact x·y=k algebra. Rounding always favours the
///   pool, so K is monotonically non-decreasing: no fee is needed for the invariant to be safe.
///
/// ZERO-FEE SUSTAINABILITY
///   Retail swap fee is the constant 0. Value that a 0-fee constant-product AMM leaks to arbitrageurs
///   (loss-versus-rebalancing, LVR) is *auctioned back*: `flashRebalance` lets any searcher move the pool
///   to the zk-attested oracle price, provided they pay EXTRACTION_BPS of the value they harvest to the
///   Treasury (which subsidises retail gas) within the same transaction.
///
/// PRIVACY
///   Swaps are never executed one-by-one on-chain. Users hold *notes* (ZK-ERC1155: assetId 0/1) in this
///   contract's own `NoteTree`. An Automator/aggregator collects signed intents off-chain and submits ONE
///   proof per batch (`settleBatch`). The chain sees only the aggregate flows (in0,in1,out0,out1) plus
///   opaque nullifiers/commitments — never who traded, individual sizes, or who received what.
///   LIMITATION (Phase 2): small batches leak by arithmetic; a min-anonymity-set rule and
///   Berrycoin<->DarkPool direct shielded transfers (no public unshield hop) are not yet implemented.
///
/// INVARIANTS (checked on-chain every time)
///   I1  K' >= K for settleBatch and flashRebalance.
///   I2  token.balanceOf(pool) >= reserve + noteCustody for both tokens, after every public entrypoint.
///   I3  flashRebalance strictly reduces |spot - oraclePrice|, and Treasury receives >= 80% of harvest.
///   I4  Σ note value of an asset == noteCustody[asset] (guaranteed by the join-split/batch circuits).
///
/// REENTRANCY MODEL: every state-mutating entrypoint is `nonReentrant`. `flashRebalance` makes an
/// external callback before it writes reserves, which is safe ONLY because no other mutating function or
/// price-reading path can run during the callback.
///
/// NOT AUDITED. Fee token assumptions: 18 decimals, no fee-on-transfer, no rebasing.
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
    uint256 public constant MAX_BATCH = 16;

    IERC20Min public immutable token0;
    IERC20Min public immutable token1; // quote asset: Treasury is paid in token1
    IZKVerifier public immutable batchVerifier;
    IZKOracle public immutable oracle;
    ITreasury public immutable treasury;
    uint256 public immutable virtual0;
    uint256 public immutable virtual1;

    // ------------------------------------------------------------------ storage
    uint128 public reserve0; // LP-owned reserves (price-setting)
    uint128 public reserve1;
    uint128 public custody0; // value backing users' shielded notes (NOT part of the curve)
    uint128 public custody1;
    uint256 public totalShares;
    mapping(address => uint256) public shares;

    // ------------------------------------------------------------------ types
    struct Batch {
        bytes proof;
        bytes32 root;
        bytes32[] nullifiers; // spent input notes, <= MAX_BATCH
        bytes32[] commitments; // new output notes, <= MAX_BATCH
        bytes[] encryptedNotes; // one ciphertext per commitment (stealth-addressed)
        uint256 in0; // aggregate asset0 moving custody -> pool
        uint256 in1;
        uint256 out0; // aggregate asset0 moving pool -> custody
        uint256 out1;
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
    error TreasuryUnderpaid(uint256 required, uint256 paid);
    error BalanceMismatch();
    error BadAsset();

    constructor(
        IERC20Min token0_,
        IERC20Min token1_,
        uint256 virtual0_,
        uint256 virtual1_,
        IZKVerifier joinSplitVerifier_,
        IZKVerifier batchVerifier_,
        IPoseidonHasher hasher_,
        IZKOracle oracle_,
        ITreasury treasury_
    ) NoteTree(joinSplitVerifier_, hasher_) {
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
        virtual0 = virtual0_;
        virtual1 = virtual1_;
        batchVerifier = batchVerifier_;
        oracle = oracle_;
        treasury = treasury_;
    }

    // ================================================================== curve maths (views)
    /// @notice Effective reserves (real + virtual).
    function effectiveReserves() public view returns (uint256 x, uint256 y) {
        x = uint256(reserve0) + virtual0;
        y = uint256(reserve1) + virtual1;
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
        reserve0 = uint128(r0 + amount0);
        reserve1 = uint128(r1 + amount1);

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
    /// @notice Settles an off-chain batch of encrypted swap intents with ONE proof.
    /// @dev    Circuit statement (`batchVerifier`), public inputs:
    ///           [root, reserve0, reserve1, in0, in1, out0, out1, H(nullifiers), H(commitments)]
    ///         proving: every nullifier is a valid unspent input note under `root`; Σ inputs per asset ==
    ///         (in0,in1); Σ outputs per asset == (out0,out1); every user's output satisfies their own
    ///         signed `minOut` and the uniform clearing price implied by (reserve0, reserve1) — so the
    ///         aggregator cannot skim individual users. The CONTRACT independently enforces the pool's
    ///         economics (I1), so a malicious aggregator can at worst fail to include someone.
    ///         H(x) = SHA-256(abi.encode(x)) >> 8  (248-bit, cheap in-circuit, fits the field).
    function settleBatch(Batch calldata b) external nonReentrant {
        uint256 n = b.nullifiers.length;
        uint256 m = b.commitments.length;
        if (n == 0 || n > MAX_BATCH || m > MAX_BATCH || b.encryptedNotes.length != m) revert BadBatch();
        if (!isKnownRoot[b.root]) revert UnknownRoot();

        uint256 r0 = reserve0;
        uint256 r1 = reserve1;
        uint256 kBefore = (r0 + virtual0) * (r1 + virtual1);

        // ---- checks: proof
        uint256[] memory pub = new uint256[](9);
        pub[0] = uint256(b.root);
        pub[1] = r0; // binds the proof to the CURRENT state: stale/griefing proofs fail
        pub[2] = r1;
        pub[3] = b.in0;
        pub[4] = b.in1;
        pub[5] = b.out0;
        pub[6] = b.out1;
        pub[7] = uint256(sha256(abi.encode(b.nullifiers))) >> 8;
        pub[8] = uint256(sha256(abi.encode(b.commitments))) >> 8;
        if (!batchVerifier.verifyProof(b.proof, pub)) revert InvalidProof();

        // ---- effects: nullifiers first (replay protection), then tree, then reserves
        for (uint256 i; i < n; ++i) {
            if (uint256(b.nullifiers[i]) >= FIELD) revert NotInField();
            _spend(b.nullifiers[i]); // reverts on duplicate, also within the same batch
        }
        uint32 firstLeaf = nextLeafIndex;
        for (uint256 i; i < m; ++i) {
            _insert(b.commitments[i], b.encryptedNotes[i]);
        }

        // value moves between note custody and curve reserves (checked arithmetic = no free lunch)
        custody0 = uint128(uint256(custody0) - b.in0 + b.out0);
        custody1 = uint128(uint256(custody1) - b.in1 + b.out1);
        uint256 n0 = r0 + b.in0 - b.out0;
        uint256 n1 = r1 + b.in1 - b.out1;
        if (n0 > MAX_RESERVE || n1 > MAX_RESERVE) revert ReserveOverflow();
        // I1: aggregate economics. Rounding in the circuit favours the pool => K never decreases.
        if ((n0 + virtual0) * (n1 + virtual1) < kBefore) revert InvariantBroken();
        reserve0 = uint128(n0);
        reserve1 = uint128(n1);

        _checkBalances(); // I2 (no tokens move in a batch: pure re-labelling custody <-> reserve)
        emit BatchSettled(b.in0, b.in1, b.out0, b.out1, firstLeaf);
        emit Sync(n0, n1);
        // TODO(Phase 3): treasury.reimburseRelayer(msg.sender, gasUsed) — gas subsidy paid from extraction.
    }

    // ================================================================== MEV internalisation
    /// @notice Searchers rebalance the pool towards the zk-attested oracle price and pay the Treasury.
    ///
    /// FLOW (Uniswap-V2-style optimistic transfer):
    ///   1. snapshot state, read zk-attested price P, enforce freshness
    ///   2. optimistically send `amount0Out`/`amount1Out` to `to`
    ///   3. callback `onFlashRebalance` — searcher trades elsewhere, repays the pool AND pays the Treasury
    ///   4. recompute reserves from real balances and enforce:
    ///        I1  K' >= K                                (pool never loses curve value)
    ///        I3a |spot' - P| < |spot - P|               (must genuinely rebalance)
    ///        I3b ΔTreasury(token1) >= 80% · harvest     (harvest = pool value lost at price P)
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
        uint256 kBefore = (r0 + virtual0) * (r1 + virtual1);
        uint256 devBefore = _deviation((r0 + virtual0), (r1 + virtual1), price);
        uint256 valueBefore = _value(r0 + virtual0, r1 + virtual1, price);
        uint256 tBefore = token1.balanceOf(address(treasury));

        // ---- interactions (optimistic)
        if (amount0Out > 0) token0.safeTransfer(to, amount0Out);
        if (amount1Out > 0) token1.safeTransfer(to, amount1Out);
        IFlashRebalancer(to).onFlashRebalance(msg.sender, amount0Out, amount1Out, data);

        // ---- re-derive reserves from balances (never trust the callee)
        uint256 n0 = token0.balanceOf(address(this)) - custody0;
        uint256 n1 = token1.balanceOf(address(this)) - custody1;
        if (n0 > MAX_RESERVE || n1 > MAX_RESERVE || n0 == 0 || n1 == 0) revert ReserveOverflow();

        // I1
        if ((n0 + virtual0) * (n1 + virtual1) < kBefore) revert InvariantBroken();
        // I3a
        if (_deviation(n0 + virtual0, n1 + virtual1, price) >= devBefore) revert DeviationNotReduced();
        // I3b
        uint256 valueAfter = _value(n0 + virtual0, n1 + virtual1, price);
        uint256 harvest = valueBefore > valueAfter ? valueBefore - valueAfter : 0;
        uint256 required = harvest * EXTRACTION_BPS / BPS;
        uint256 paid = token1.balanceOf(address(treasury)) - tBefore;
        if (paid < required) revert TreasuryUnderpaid(required, paid);

        // ---- effects (guarded by nonReentrant for the whole callback window)
        reserve0 = uint128(n0);
        reserve1 = uint128(n1);
        emit Sync(n0, n1);
        emit Rebalanced(msg.sender, harvest, paid, price);

        treasury.recordExtraction(address(token1), paid, msg.sender); // trusted immutable, informational
    }

    /// @notice Off-chain helper for searchers: minimum Treasury payment (token1) for ending at (n0, n1).
    function quoteMinPayment(uint256 n0, uint256 n1) external view returns (uint256) {
        uint256 price = _freshPrice();
        uint256 vb = _value(uint256(reserve0) + virtual0, uint256(reserve1) + virtual1, price);
        uint256 va = _value(n0 + virtual0, n1 + virtual1, price);
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
