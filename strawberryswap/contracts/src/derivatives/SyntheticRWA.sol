// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {SafeTransfer} from "../utils/SafeTransfer.sol";
import {IERC20Min} from "../interfaces/IERC20Min.sol";

// =====================================================================================================
//                                      ZK ORACLE INTERFACE (zk-DECO)
// =====================================================================================================

/// @title IZKDecoOracle
/// @notice Verifier of zero-knowledge proofs that a number was served over TLS by a specific, registered
///         data provider (DECO / zk-DECO / TLSNotary family). The consumer never trusts a signer or a
///         relayer: it trusts only (a) the proof system and (b) the provider registry.
///
/// HOW A PRICE REACHES THE CHAIN
///   1. A *prover* (any relayer, no permission needed) opens a normal HTTPS session to the provider's
///      public API. In a 3-party handshake the key material is secret-shared with a *verifier* so the
///      prover cannot forge the transcript; no cooperation from the provider is required or possible.
///   2. The prover builds a SNARK proving, WITHOUT revealing the transcript or credentials:
///        * the session's server certificate chains to the key registered under `providerId`;
///        * the request followed the template committed in `queryCommitment`
///            = Poseidon(ticker/endpoint template, salt);
///        * the authenticated response body contained `price` and `observedAt` at the claimed offsets.
///      Private witnesses: API key, full request, full response, TLS keys, commitment salt.
///   3. The proof + the public tuple below is submitted on-chain; `verifyAttestation` checks it.
///
/// WHAT IS (AND IS NOT) HIDDEN — precision matters:
///   * Hidden from the CHAIN / public observers: the query (ticker, endpoint, parameters), the API key
///     and the rest of the response. Only an opaque `queryCommitment` appears on-chain, so nobody can tell
///     which instrument a position tracks from contract data alone.
///   * Hidden from the PROVIDER: that the data is being used on-chain, who consumes it, and which
///     contract/address it feeds. The provider sees an ordinary client request.
///   * NOT hideable from the provider: the request itself. A server that answers a query necessarily sees
///     it. To limit metadata (IP, timing, query patterns) provers should use rotating relays/Tor and a
///     shared provider account; that is an off-chain operational requirement of the Automator network,
///     not something a smart contract can enforce.
interface IZKDecoOracle {
    /// @dev Public statement proven by the SNARK. `price` is 18-decimal fixed point (index units).
    struct Attestation {
        bytes32 providerId; // registry key: hash of (domain, TLS pubkey root) of the data provider
        bytes32 queryCommitment; // Poseidon commitment to the hidden request template
        uint256 price; // value extracted from the authenticated response, 1e18 scale
        uint64 observedAt; // timestamp the provider's response asserts (from the signed body / Date)
    }

    /// @return ok true iff `proof` is valid for `a` AND `a.providerId` is an active registered provider.
    /// MUST be `view` (no state change, no callbacks): consumers call it under a reentrancy guard and
    /// rely on it being a STATICCALL.
    function verifyAttestation(Attestation calldata a, bytes calldata proof) external view returns (bool ok);
}

// =====================================================================================================
//                                          SYNTHETIC RWA
// =====================================================================================================

/// @title SyntheticRWA — Module 4: synthetic derivatives on physical indices and yield flows
///
/// DESIGN: FULLY-FUNDED LONG/SHORT PAIRS ON A CAPPED INDEX
///   A 1:1 "gold token" backed only by stablecoin cannot be 100 % collateralised, because the index has no
///   upper bound. The only construction that keeps the 100 %-reserve axiom is to bound the payoff:
///       per contract, collateral C is locked.  At settlement, with index level S and cap K:
///           LONG  receives  L = C · min(S, K) / K
///           SHORT receives  C − L
///       L + (C − L) == C  EXACTLY, for every S.  The contract can never owe more than it holds.
///   Choose K well above the plausible range (e.g. 2× spot) and the long behaves as a linear synthetic
///   of the index while remaining fully backed; the short is the inverse position. This works for
///   commodity prices (gold, WTI, wheat) and for yield flows (e.g. a T-bill rate or cumulative yield
///   index: S = rate, K = max rate), where long = receive-variable and short = receive-fixed-complement.
///
/// AXIOMS
///   * No naked exposure, no inflation: positions are created ONLY by `mintPair`, which pulls
///     qty × C collateral first (verified by balance delta). Pairs can be destroyed only via
///     `redeemPair` (returns qty × C) or settlement. Total collateral held == Σ outstanding pairs × C.
///   * Solvency (I-SOLV): collateral.balanceOf(this) >= totalCollateral[collateral] after every call.
///   * Payout per contract is computed ONCE from the settlement price → identical for every holder, so
///     rounding can never make Σ payouts exceed Σ collateral.
///
/// LIFECYCLE
///   mintPair ──► (trade long/short freely) ──► expiry ──► settle(attestation) during SETTLE_WINDOW ──►
///   finalised at expiry + SETTLE_WINDOW ──► redeemLong / redeemShort.
///   Fallback: if no valid attestation arrives, after FALLBACK_DELAY both sides redeem C/2 (neutral
///   refund) so funds can never be locked by a dead oracle.
///
/// ORACLE SAFETY RULES (on top of the proof)
///   * providerId and queryCommitment must equal the series' immutable choices;
///   * observedAt ∈ [expiry, expiry + SETTLE_WINDOW] and <= block.timestamp;
///   * EARLIEST-OBSERVATION-WINS: until the window closes, a valid attestation with a strictly earlier
///     `observedAt` replaces the stored one. This removes the incentive to wait and cherry-pick a
///     favourable price: the canonical price is the earliest provable one after expiry.
///
/// NOT AUDITED. Collateral: plain 18-decimal ERC-20. Position ledger is internal (Phase 3: wrap in
/// ZK-ERC1155 using NoteTree, as OptionsMarket does).
contract SyntheticRWA is ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    // ------------------------------------------------------------------ parameters
    uint64 public constant SETTLE_WINDOW = 10 minutes;
    uint64 public constant FALLBACK_DELAY = 7 days;
    uint256 internal constant WAD = 1e18;

    IZKDecoOracle public immutable oracle;

    // ------------------------------------------------------------------ types
    struct SeriesParams {
        IERC20Min collateral;
        uint128 collateralPerContract; // C, base units locked per long+short pair
        uint256 cap; // K, index level (1e18) at/above which the long takes everything
        uint64 expiry;
        bytes32 providerId; // which data provider is authoritative
        bytes32 queryCommitment; // hidden instrument/endpoint (e.g. XAU/USD spot)
    }

    struct Series {
        SeriesParams p;
        bool exists;
        bool settled; // at least one valid attestation recorded
        uint256 settlementPrice; // 1e18
        uint64 settlementObservedAt;
    }

    // ------------------------------------------------------------------ storage
    mapping(bytes32 => Series) public series;
    mapping(bytes32 => uint256) public pairsOutstanding; // contracts not yet redeemed (long side count)
    mapping(bytes32 => mapping(address => uint256)) public longBalance;
    mapping(bytes32 => mapping(address => uint256)) public shortBalance;
    mapping(bytes32 => uint256) public longSupply;
    mapping(bytes32 => uint256) public shortSupply;
    mapping(address => uint256) public totalCollateral; // token => Σ owed

    // ------------------------------------------------------------------ events / errors
    event SeriesCreated(bytes32 indexed id, address collateral, uint256 c, uint256 cap, uint64 expiry, bytes32 providerId, bytes32 queryCommitment);
    event PairMinted(bytes32 indexed id, address indexed minter, address longTo, address shortTo, uint256 qty);
    event PairRedeemed(bytes32 indexed id, address indexed holder, uint256 qty);
    event Settled(bytes32 indexed id, uint256 price, uint64 observedAt);
    event Redeemed(bytes32 indexed id, address indexed holder, bool isLong, uint256 qty, uint256 payout);
    event PositionTransfer(bytes32 indexed id, bool isLong, address indexed from, address indexed to, uint256 qty);

    error BadSeries();
    error UnknownSeries();
    error Expired();
    error NotFinal();
    error SettlementWindowClosed();
    error SettlementNotYet();
    error AttestationMismatch();
    error AttestationTooLate();
    error NotEarlier();
    error InvalidProof();
    error AmountZero();
    error InsufficientBalance();
    error InvalidRecipient();
    error CollateralShortfall();
    error Insolvent();

    constructor(IZKDecoOracle oracle_) {
        if (address(oracle_) == address(0)) revert InvalidRecipient();
        oracle = oracle_;
    }

    // ================================================================== series
    function seriesId(SeriesParams memory p) public pure returns (bytes32) {
        return keccak256(abi.encode(p));
    }

    function createSeries(SeriesParams calldata p) external returns (bytes32 id) {
        id = seriesId(p);
        if (series[id].exists) return id;
        if (
            address(p.collateral) == address(0) || p.collateralPerContract == 0 || p.cap == 0
                || p.expiry <= block.timestamp || p.providerId == bytes32(0) || p.queryCommitment == bytes32(0)
                || p.collateral.decimals() != 18
        ) revert BadSeries();
        series[id].p = p;
        series[id].exists = true;
        emit SeriesCreated(id, address(p.collateral), p.collateralPerContract, p.cap, p.expiry, p.providerId, p.queryCommitment);
    }

    // ================================================================== mint / redeem pairs (100 % funded)
    /// @notice Locks qty × C collateral and mints `qty` LONG to `longTo` and `qty` SHORT to `shortTo`.
    function mintPair(bytes32 id, uint256 qty, address longTo, address shortTo) external nonReentrant {
        Series storage s = _s(id);
        if (qty == 0) revert AmountZero();
        if (longTo == address(0) || shortTo == address(0)) revert InvalidRecipient();
        if (block.timestamp >= s.p.expiry) revert Expired();

        IERC20Min c = s.p.collateral;
        uint256 required = qty * s.p.collateralPerContract;

        // ---- effects
        pairsOutstanding[id] += qty;
        longSupply[id] += qty;
        shortSupply[id] += qty;
        longBalance[id][longTo] += qty;
        shortBalance[id][shortTo] += qty;
        totalCollateral[address(c)] += required;

        // ---- interaction + coverage gate
        uint256 before = c.balanceOf(address(this));
        c.safeTransferFrom(msg.sender, address(this), required);
        if (c.balanceOf(address(this)) - before != required) revert CollateralShortfall();
        _requireSolvent(c);

        emit PairMinted(id, msg.sender, longTo, shortTo, qty);
    }

    /// @notice Burn equal long + short held by the caller and withdraw qty × C. Only before expiry
    ///         (afterwards use settlement), so early exit cannot front-run the oracle.
    function redeemPair(bytes32 id, uint256 qty, address to) external nonReentrant {
        Series storage s = _s(id);
        if (qty == 0) revert AmountZero();
        if (to == address(0)) revert InvalidRecipient();
        if (block.timestamp >= s.p.expiry) revert Expired();
        if (longBalance[id][msg.sender] < qty || shortBalance[id][msg.sender] < qty) revert InsufficientBalance();

        IERC20Min c = s.p.collateral;
        uint256 out = qty * s.p.collateralPerContract;

        longBalance[id][msg.sender] -= qty;
        shortBalance[id][msg.sender] -= qty;
        longSupply[id] -= qty;
        shortSupply[id] -= qty;
        pairsOutstanding[id] -= qty;
        totalCollateral[address(c)] -= out;

        c.safeTransfer(to, out);
        _requireSolvent(c);
        emit PairRedeemed(id, msg.sender, qty);
    }

    // ================================================================== settlement via zk-DECO
    /// @notice Submit a zk-DECO attested index level. Permissionless. See "ORACLE SAFETY RULES".
    /// @dev    `oracle.verifyAttestation` is a STATICCALL (view); it cannot re-enter or mutate state.
    function settle(bytes32 id, IZKDecoOracle.Attestation calldata a, bytes calldata proof) external nonReentrant {
        Series storage s = _s(id);
        uint64 exp = s.p.expiry;
        if (block.timestamp < exp) revert SettlementNotYet();
        if (block.timestamp >= exp + SETTLE_WINDOW) revert SettlementWindowClosed();

        // ---- bind the attestation to THIS series (provider + hidden query) and to the time window
        if (a.providerId != s.p.providerId || a.queryCommitment != s.p.queryCommitment) revert AttestationMismatch();
        if (a.observedAt < exp || a.observedAt > exp + SETTLE_WINDOW || a.observedAt > block.timestamp) {
            revert AttestationTooLate();
        }
        if (a.price == 0) revert AttestationMismatch();
        if (s.settled && a.observedAt >= s.settlementObservedAt) revert NotEarlier();

        // ---- the cryptographic check (proof + provider registry)
        if (!oracle.verifyAttestation(a, proof)) revert InvalidProof();

        s.settled = true;
        s.settlementPrice = a.price;
        s.settlementObservedAt = a.observedAt;
        emit Settled(id, a.price, a.observedAt);
    }

    // ================================================================== redemption after finality
    /// @notice Redeem `qty` LONG after finality for qty × L, where L = C · min(S,K) / K.
    function redeemLong(bytes32 id, uint256 qty, address to) external nonReentrant {
        Series storage s = _s(id);
        _beforeRedeem(qty, to, longBalance[id][msg.sender]);
        uint256 payout = qty * _longPayoutPerContract(s);
        longBalance[id][msg.sender] -= qty;
        longSupply[id] -= qty;
        _payOut(id, s, qty, payout, to, true);
    }

    /// @notice Redeem `qty` SHORT after finality for qty × (C − L).
    function redeemShort(bytes32 id, uint256 qty, address to) external nonReentrant {
        Series storage s = _s(id);
        _beforeRedeem(qty, to, shortBalance[id][msg.sender]);
        uint256 payout = qty * (s.p.collateralPerContract - _longPayoutPerContract(s));
        shortBalance[id][msg.sender] -= qty;
        shortSupply[id] -= qty;
        _payOut(id, s, qty, payout, to, false);
    }

    /// @notice Payout per LONG contract once final (reverts before finality). View for UIs/bots.
    function longPayoutPerContract(bytes32 id) external view returns (uint256) {
        return _longPayoutPerContract(_s(id));
    }

    // ================================================================== position transfers
    function transferPosition(bytes32 id, bool isLong, address to, uint256 qty) external {
        _s(id);
        if (to == address(0) || to == address(this)) revert InvalidRecipient();
        mapping(address => uint256) storage bal = isLong ? longBalance[id] : shortBalance[id];
        if (bal[msg.sender] < qty) revert InsufficientBalance();
        bal[msg.sender] -= qty;
        bal[to] += qty;
        emit PositionTransfer(id, isLong, msg.sender, to, qty);
    }

    // ================================================================== internals
    function _beforeRedeem(uint256 qty, address to, uint256 bal) private pure {
        if (qty == 0) revert AmountZero();
        if (to == address(0)) revert InvalidRecipient();
        if (bal < qty) revert InsufficientBalance();
    }

    function _payOut(bytes32 id, Series storage s, uint256 qty, uint256 payout, address to, bool isLong) private {
        IERC20Min c = s.p.collateral;
        totalCollateral[address(c)] -= payout;
        if (payout > 0) c.safeTransfer(to, payout);
        _requireSolvent(c);
        emit Redeemed(id, msg.sender, isLong, qty, payout);
    }

    /// @dev Single source of truth for the payoff. Identical for all holders ⇒ Σ payouts == Σ collateral.
    function _longPayoutPerContract(Series storage s) private view returns (uint256) {
        uint64 exp = s.p.expiry;
        uint256 c = s.p.collateralPerContract;
        if (s.settled && block.timestamp >= exp + SETTLE_WINDOW) {
            uint256 clamped = s.settlementPrice < s.p.cap ? s.settlementPrice : s.p.cap;
            return c * clamped / s.p.cap; // floor; short gets the exact complement
        }
        // Oracle never delivered: neutral 50/50 refund after the fallback delay.
        if (!s.settled && block.timestamp >= exp + SETTLE_WINDOW + FALLBACK_DELAY) {
            return c / 2;
        }
        revert NotFinal();
    }

    function _s(bytes32 id) private view returns (Series storage s) {
        s = series[id];
        if (!s.exists) revert UnknownSeries();
    }

    function _requireSolvent(IERC20Min t) private view {
        if (t.balanceOf(address(this)) < totalCollateral[address(t)]) revert Insolvent();
    }
}
