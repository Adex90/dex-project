// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {ZkPriceFeed} from "../src/oracle/ZkPriceFeed.sol";
import {IZKDecoOracle} from "../src/interfaces/IZKDecoOracle.sol";

/// @dev A verifier whose answer the test sets; it counts nothing (it is called with STATICCALL).
contract SwitchVerifier is IZKDecoOracle {
    bool public answer = true;

    function set(bool answer_) external {
        answer = answer_;
    }

    function verifyAttestation(Attestation calldata, bytes calldata) external view returns (bool) {
        return answer;
    }
}

/// @dev Breaks IZKDecoOracle's `view` promise by writing storage while verifying.
contract WritingVerifier {
    uint256 public calls;

    function verifyAttestation(IZKDecoOracle.Attestation calldata, bytes calldata) external returns (bool) {
        ++calls;
        return true;
    }
}

/// @notice ZkPriceFeed's rules, one by one, against a verifier the test controls. The real proof system is exercised
///         in ZkPriceFeedIntegration.t.sol.
contract ZkPriceFeedTest is Test {
    bytes32 constant PROVIDER = keccak256("test provider");
    bytes32 constant QUERY = keccak256("test ETHUSD query");
    uint64 constant T = 1_767_225_600; // 2026-01-01T00:00:00Z
    uint64 constant DELAY = 30;
    uint256 constant MIN = 100e18;
    uint256 constant MAX = 100_000e18;
    bytes constant PROOF = hex"01";

    SwitchVerifier verifier;
    ZkPriceFeed feed;

    event PriceUpdated(uint256 price, uint64 observedAt, address indexed relayer);

    function setUp() public {
        verifier = new SwitchVerifier();
        feed = new ZkPriceFeed(_config());
        vm.warp(T + 5);
    }

    function _config() internal view returns (ZkPriceFeed.Config memory) {
        return ZkPriceFeed.Config({
            verifier: IZKDecoOracle(address(verifier)),
            providerId: PROVIDER,
            queryCommitment: QUERY,
            maxSubmissionDelay: DELAY,
            minPrice: MIN,
            maxPrice: MAX
        });
    }

    function _att(uint256 price, uint64 observedAt) internal pure returns (IZKDecoOracle.Attestation memory) {
        return IZKDecoOracle.Attestation({
            providerId: PROVIDER, queryCommitment: QUERY, price: price, observedAt: observedAt
        });
    }

    function _latest() internal view returns (uint256 price, uint64 observedAt) {
        return feed.latestVerifiedPrice();
    }

    // ------------------------------------------------------------------ configuration

    function test_constructor_rejectsAnUnusableConfiguration() public {
        ZkPriceFeed.Config memory c;

        c = _config();
        c.verifier = IZKDecoOracle(address(0));
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        c = _config();
        c.providerId = bytes32(0);
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        c = _config();
        c.queryCommitment = bytes32(0);
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        c = _config();
        c.maxSubmissionDelay = 0;
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        c = _config();
        c.maxSubmissionDelay = 61; // older than DarkPool's MAX_ORACLE_AGE: useless to the pool
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        c = _config();
        c.minPrice = 0;
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        c = _config();
        c.minPrice = c.maxPrice + 1;
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        c = _config();
        c.maxPrice = 1e24 + 1; // above DarkPool's MAX_PRICE
        vm.expectRevert(ZkPriceFeed.BadConfig.selector);
        new ZkPriceFeed(c);

        // The extremes that are allowed.
        c = _config();
        (c.maxSubmissionDelay, c.minPrice, c.maxPrice) = (60, 1, 1e24);
        ZkPriceFeed edge = new ZkPriceFeed(c);
        assertEq(edge.maxSubmissionDelay(), 60);
        assertEq(edge.maxPrice(), feed.MAX_POOL_PRICE());
    }

    function test_thereIsNoPriceBeforeTheFirstPush() public view {
        (uint256 price, uint64 observedAt) = _latest();
        assertEq(price, 0);
        assertEq(observedAt, 0, "updatedAt 0: every consumer, DarkPool included, sees it as stale");
    }

    // ------------------------------------------------------------------ the happy path

    function test_submit_storesThePriceAndTheObservationTime_fromAnyRelayer() public {
        address relayer = address(0xBEEF);
        vm.expectEmit(address(feed));
        emit PriceUpdated(3456.789012e18, T, relayer);
        vm.prank(relayer);
        feed.submit(_att(3456.789012e18, T), PROOF);

        (uint256 price, uint64 observedAt) = _latest();
        assertEq(price, 3456.789012e18);
        assertEq(observedAt, T, "the observation time, not the block time of the push");

        // A second relayer pushes the next observation.
        vm.warp(T + 20);
        vm.prank(address(0xCAFE));
        feed.submit(_att(3457e18, T + 15), PROOF);
        (price, observedAt) = _latest();
        assertEq(price, 3457e18);
        assertEq(observedAt, T + 15);
    }

    // ------------------------------------------------------------------ provider and instrument

    function test_submit_refusesAnotherProvider() public {
        IZKDecoOracle.Attestation memory a = _att(3456e18, T);
        a.providerId = keccak256("someone else");
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.WrongProvider.selector, a.providerId));
        feed.submit(a, PROOF);
    }

    function test_submit_refusesAnotherInstrument() public {
        IZKDecoOracle.Attestation memory a = _att(2345.678912e18, T);
        a.queryCommitment = keccak256("XAUUSD query");
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.WrongInstrument.selector, a.queryCommitment));
        feed.submit(a, PROOF);
    }

    // ------------------------------------------------------------------ order and time window

    function test_submit_refusesTheSameOrAnOlderObservation() public {
        feed.submit(_att(3456e18, T), PROOF);
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.NotNewer.selector, T, T));
        feed.submit(_att(3456e18, T), PROOF); // a replay
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.NotNewer.selector, T - 1, T));
        feed.submit(_att(3400e18, T - 1), PROOF); // out of order
        feed.submit(_att(3457e18, T + 1), PROOF);
        (, uint64 observedAt) = _latest();
        assertEq(observedAt, T + 1);
    }

    function test_submit_refusesAnObservationFromTheFuture() public {
        uint256 nowTs = block.timestamp; // T + 5
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.FromTheFuture.selector, uint64(nowTs + 1), nowTs));
        feed.submit(_att(3456e18, uint64(nowTs + 1)), PROOF);
        feed.submit(_att(3456e18, uint64(nowTs)), PROOF); // observed in this very second: accepted
    }

    function test_submit_refusesAnObservationHeldBackTooLong() public {
        vm.warp(T + DELAY + 1);
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.TooOld.selector, T, uint256(T + DELAY + 1)));
        feed.submit(_att(3456e18, T), PROOF);
        vm.warp(T + DELAY);
        feed.submit(_att(3456e18, T), PROOF); // exactly maxSubmissionDelay old: accepted
    }

    // ------------------------------------------------------------------ bounds and proof

    function test_submit_refusesPricesOutsideTheBounds() public {
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.PriceOutOfBounds.selector, MIN - 1));
        feed.submit(_att(MIN - 1, T), PROOF);
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.PriceOutOfBounds.selector, MAX + 1));
        feed.submit(_att(MAX + 1, T), PROOF);
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.PriceOutOfBounds.selector, type(uint256).max));
        feed.submit(_att(type(uint256).max, T), PROOF);

        feed.submit(_att(MIN, T), PROOF);
        feed.submit(_att(MAX, T + 1), PROOF);
        (uint256 price,) = _latest();
        assertEq(price, MAX);
    }

    function test_submit_refusesAnAttestationTheVerifierRejects() public {
        feed.submit(_att(3456e18, T), PROOF);
        verifier.set(false);
        vm.expectRevert(ZkPriceFeed.InvalidAttestation.selector);
        feed.submit(_att(1e18 * 3500, T + 1), PROOF);
        (uint256 price, uint64 observedAt) = _latest();
        assertEq(price, 3456e18, "a refused push changes nothing");
        assertEq(observedAt, T, "a refused push changes nothing");
    }

    /// The verifier is called with STATICCALL: a verifier that writes state cannot be used, and cannot re-enter.
    function test_theVerifierIsCalledWithStaticcall() public {
        WritingVerifier writer = new WritingVerifier();
        ZkPriceFeed.Config memory c = _config();
        c.verifier = IZKDecoOracle(address(writer));
        ZkPriceFeed f = new ZkPriceFeed(c);
        vm.expectRevert();
        f.submit(_att(3456e18, T), PROOF);
        assertEq(writer.calls(), 0);
    }

    // ------------------------------------------------------------------ properties

    /// Any sequence of pushes: the feed accepts exactly those the rules allow, and its stored observation time never
    /// goes back.
    function testFuzz_theFeedFollowsItsRulesOverAnySequence(
        uint64[12] memory offsets,
        uint64[12] memory gaps,
        uint96[12] memory prices
    ) public {
        uint64 stored;
        uint256 storedPrice;
        uint256 clock = T;
        for (uint256 i; i < 12; ++i) {
            clock += bound(gaps[i], 0, 20);
            vm.warp(clock);
            uint64 observedAt = uint64(bound(offsets[i], clock - 45, clock + 5)); // from 45 s old to 5 s ahead
            uint256 price = bound(prices[i], 1, 200_000e18);

            bool ok = observedAt > stored && observedAt <= clock && clock - observedAt <= DELAY && price >= MIN
                && price <= MAX;
            if (ok) {
                feed.submit(_att(price, observedAt), PROOF);
                (stored, storedPrice) = (observedAt, price);
            } else {
                vm.expectRevert();
                feed.submit(_att(price, observedAt), PROOF);
            }
            (uint256 p, uint64 t) = _latest();
            assertEq(p, storedPrice);
            assertEq(t, stored);
            assertLe(t, block.timestamp, "never stamped in the future");
        }
    }
}
