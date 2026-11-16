// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IZKOracle} from "../interfaces/IZKOracle.sol";
import {IZKDecoOracle} from "../interfaces/IZKDecoOracle.sol";

/// @title ZkPriceFeed
/// @notice The DarkPool's price feed (`IZKOracle`): the newest zk-DECO-attested price of ONE instrument from ONE data
///         provider. Anyone holding a valid attestation may push it (`submit`): the feed trusts the proof, never the
///         relayer. Proofs are checked by `ZkDecoOracle` (Groth16 proof of `circuits/zkdeco_oracle` + the provider's
///         registered notary signature).
///
/// WHAT `submit` ENFORCES (cheap checks first, the proof last)
///   1. Provider and instrument: `a.providerId == providerId` and `a.queryCommitment == queryCommitment`. A valid
///      proof for another provider or another instrument (XAUUSD pushed to an ETHUSD feed) is refused.
///   2. Order: `a.observedAt` is strictly newer than the stored observation. Replays and out-of-order pushes are
///      refused, and the stored time never goes back.
///   3. Time window: `a.observedAt <= block.timestamp`, because DarkPool rejects future-stamped prices and storing one
///      would stop rebalances and batches until the chain caught up; and `block.timestamp - a.observedAt <=
///      maxSubmissionDelay`, so a relayer cannot hold attestations back and pick the price it prefers from a long
///      window.
///   4. Bounds: `minPrice <= price <= maxPrice <= 1e24` (DarkPool's MAX_PRICE). A sanity range against unit and
///      parsing errors, not a substitute for the notary's honesty.
///   5. Proof: `verifier.verifyAttestation(a, proof)`, a STATICCALL that cannot re-enter or change state.
///
/// UNITS. The circuit attests the decimal quote of one whole token0 in token1, scaled to 1e18 (ETHUSD 3456.789012 ->
///   3456.789012e18). DarkPool prices raw units, P = y·1e18/x, and only accepts 18-decimal tokens (`BadDecimals`), so
///   the attested value IS the pool's price; the feed stores it unchanged.
///
/// FRESHNESS. `latestVerifiedPrice` returns the OBSERVATION time, not the block in which it was pushed, so DarkPool's
///   60 s `MAX_ORACLE_AGE` measures how old the data really is. Before the first push it returns (0, 0), which every
///   consumer must treat as stale (DarkPool does: `StaleOracle`). Liveness is the relayers' job: the testnet relayer
///   pushes every ~15 s, and anyone may run another one.
///
/// ⚠ TRUST MODEL (honest statement). A price is as honest as (a) the data provider, (b) the provider's notary
///   registered in `ZkDecoOracle`, until in-circuit TLS verification removes it, and (c) the circuit and its trusted
///   setup. TESTNET IS A TRUSTED BOOTSTRAP: Rothbard Tecnologías Financieras operates both the price endpoint and the
///   notary, and the verifier uses single-party DEV keys, so testnet prices are only as trustworthy as the operator.
///   Mainnet requires independent providers, a decentralized notary (or in-circuit TLS), a multi-party setup ceremony
///   and an external audit. See docs/ORACLE.md.
contract ZkPriceFeed is IZKOracle {
    /// @notice DarkPool's MAX_PRICE: the largest price the pool accepts.
    uint256 public constant MAX_POOL_PRICE = 1e24;
    /// @notice DarkPool's MAX_ORACLE_AGE: an observation older than this is of no use to the pool.
    uint64 public constant MAX_SUBMISSION_DELAY_CAP = 60;

    struct Config {
        IZKDecoOracle verifier; // ZkDecoOracle
        bytes32 providerId; // the one data provider accepted
        bytes32 queryCommitment; // the one instrument accepted (its hidden query)
        uint64 maxSubmissionDelay; // seconds from observation to push, 1..60
        uint256 minPrice; // 1e18-scaled, > 0
        uint256 maxPrice; // 1e18-scaled, <= MAX_POOL_PRICE
    }

    IZKDecoOracle public immutable verifier;
    bytes32 public immutable providerId;
    bytes32 public immutable queryCommitment;
    uint64 public immutable maxSubmissionDelay;
    uint256 public immutable minPrice;
    uint256 public immutable maxPrice;

    /// @dev One slot (192 + 64 bits): DarkPool's hot paths read the feed with a single SLOAD.
    uint192 private _price;
    uint64 private _observedAt;

    event PriceUpdated(uint256 price, uint64 observedAt, address indexed relayer);

    error BadConfig();
    error WrongProvider(bytes32 providerId);
    error WrongInstrument(bytes32 queryCommitment);
    error NotNewer(uint64 observedAt, uint64 latest);
    error FromTheFuture(uint64 observedAt, uint256 blockTime);
    error TooOld(uint64 observedAt, uint256 blockTime);
    error PriceOutOfBounds(uint256 price);
    error InvalidAttestation();

    constructor(Config memory c) {
        if (
            address(c.verifier) == address(0) || c.providerId == bytes32(0) || c.queryCommitment == bytes32(0)
                || c.maxSubmissionDelay == 0 || c.maxSubmissionDelay > MAX_SUBMISSION_DELAY_CAP || c.minPrice == 0
                || c.minPrice > c.maxPrice || c.maxPrice > MAX_POOL_PRICE
        ) {
            revert BadConfig();
        }
        verifier = c.verifier;
        providerId = c.providerId;
        queryCommitment = c.queryCommitment;
        maxSubmissionDelay = c.maxSubmissionDelay;
        minPrice = c.minPrice;
        maxPrice = c.maxPrice;
    }

    /// @notice Pushes an attested price. Permissionless: the relayer pays the gas and is trusted for nothing.
    /// @param a     the attestation's public statement (provider, instrument commitment, price, observation time)
    /// @param proof the `ZkDecoOracle` wire format: Groth16 proof ‖ bodyCommitment ‖ notary signature (353 bytes)
    function submit(IZKDecoOracle.Attestation calldata a, bytes calldata proof) external {
        if (a.providerId != providerId) revert WrongProvider(a.providerId);
        if (a.queryCommitment != queryCommitment) revert WrongInstrument(a.queryCommitment);
        uint64 latest = _observedAt;
        if (a.observedAt <= latest) revert NotNewer(a.observedAt, latest);
        if (a.observedAt > block.timestamp) revert FromTheFuture(a.observedAt, block.timestamp);
        if (block.timestamp - a.observedAt > maxSubmissionDelay) revert TooOld(a.observedAt, block.timestamp);
        if (a.price < minPrice || a.price > maxPrice) revert PriceOutOfBounds(a.price);
        if (!verifier.verifyAttestation(a, proof)) revert InvalidAttestation();

        _price = uint192(a.price); // <= MAX_POOL_PRICE < 2^192
        _observedAt = a.observedAt;
        emit PriceUpdated(a.price, a.observedAt, msg.sender);
    }

    /// @inheritdoc IZKOracle
    /// @dev `updatedAt` is the attested observation time; (0, 0) until the first push.
    function latestVerifiedPrice() external view returns (uint256 price1e18, uint64 updatedAt) {
        return (_price, _observedAt);
    }
}
