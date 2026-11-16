// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {ZkPriceFeed} from "../src/oracle/ZkPriceFeed.sol";
import {ZkDecoOracle, IGroth16Verifier5} from "../src/oracle/ZkDecoOracle.sol";
import {IZKDecoOracle} from "../src/interfaces/IZKDecoOracle.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../src/interfaces/ITreasury.sol";
import {Math} from "../src/utils/Math.sol";
import {OracleGroth16Verifier} from "./fixtures/OracleGroth16Verifier.dev.sol";
import {FeedFixtures} from "./fixtures/FeedFixtures.sol";
import {Fixtures} from "./fixtures/Fixtures.sol";
import {MockVerifier, MockHasher, MockERC20, MockTreasury, MockSearcher} from "./mocks/Mocks.sol";

/// @notice End to end: REAL zk-DECO attestations (Groth16 proofs of circuits/zkdeco_oracle for an ETHUSD series 15 s
///         apart, signed by a test notary) pushed to ZkPriceFeed, read by a real DarkPool: freshness, the series,
///         tampered or foreign attestations, and a flash rebalance settled at the attested price.
contract ZkPriceFeedIntegrationTest is Test {
    uint256 constant NOTARY_PK = 0xA11CE5;
    uint256 constant WAD = 1e18;
    uint64 constant DELAY = 30;

    ZkDecoOracle oracle;
    ZkPriceFeed feed;
    DarkPool pool;
    MockERC20 token0; // the ETH side (18 decimals, as DarkPool requires)
    MockERC20 token1; // the USD side (18 decimals, as DarkPool requires)
    MockTreasury treasury;
    address lp = address(0x1111);
    address relayer = address(0x7e1a7e);

    function setUp() public {
        bytes32[] memory ids = new bytes32[](1);
        address[] memory notaries = new address[](1);
        ids[0] = FeedFixtures.PROVIDER_ID;
        notaries[0] = vm.addr(NOTARY_PK);
        oracle = new ZkDecoOracle(IGroth16Verifier5(address(new OracleGroth16Verifier())), 600_000, ids, notaries);
        feed = new ZkPriceFeed(
            ZkPriceFeed.Config({
                verifier: IZKDecoOracle(address(oracle)),
                providerId: FeedFixtures.PROVIDER_ID,
                queryCommitment: FeedFixtures.ETHUSD_QUERY_COMMITMENT,
                maxSubmissionDelay: DELAY,
                minPrice: 100e18,
                maxPrice: 100_000e18
            })
        );

        MockERC20 a = new MockERC20("WETH");
        MockERC20 b = new MockERC20("USD");
        (token0, token1) = address(a) < address(b) ? (a, b) : (b, a); // DarkPool requires token0 < token1
        treasury = new MockTreasury();
        pool = new DarkPool(
            IERC20Min(address(token0)),
            IERC20Min(address(token1)),
            1_000e18, // virtual reserves: the pool starts at 3500 token1 per token0
            3_500_000e18,
            IZKVerifier(address(new MockVerifier(true))),
            IZKVerifier(address(new MockVerifier(true))),
            IPoseidonHasher(address(new MockHasher())),
            IZKOracle(address(feed)),
            ITreasury(address(treasury)),
            16
        );
        _addLiquidity(1_000e18, 3_500_000e18); // x = 2000, y = 7,000,000: spot 3500, above the attested ~3456.79
    }

    // ------------------------------------------------------------------ helpers

    function _addLiquidity(uint256 a0, uint256 a1) internal {
        token0.mint(lp, a0);
        token1.mint(lp, a1);
        vm.startPrank(lp);
        token0.approve(address(pool), a0);
        token1.approve(address(pool), a1);
        pool.addLiquidity(a0, a1, lp);
        vm.stopPrank();
    }

    function _attestation(uint256 i)
        internal
        pure
        returns (IZKDecoOracle.Attestation memory a, FeedFixtures.Attested memory f)
    {
        f = FeedFixtures.ethusd(i);
        a = IZKDecoOracle.Attestation({
            providerId: FeedFixtures.PROVIDER_ID,
            queryCommitment: FeedFixtures.ETHUSD_QUERY_COMMITMENT,
            price: f.price,
            observedAt: f.observedAt
        });
    }

    /// @dev The ZkDecoOracle wire format: Groth16 proof ‖ bodyCommitment ‖ notary signature over it.
    function _proof(bytes memory snark, bytes32 bodyCommitment, uint256 notaryPk) internal view returns (bytes memory) {
        bytes32 digest = oracle.notaryDigest(FeedFixtures.PROVIDER_ID, bodyCommitment);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(notaryPk, digest);
        return abi.encodePacked(snark, bodyCommitment, r, s, v);
    }

    function _push(uint256 i) internal returns (FeedFixtures.Attested memory f) {
        IZKDecoOracle.Attestation memory a;
        (a, f) = _attestation(i);
        bytes memory proof = _proof(f.snark, f.bodyCommitment, NOTARY_PK); // before the prank: it calls the oracle
        vm.prank(relayer);
        feed.submit(a, proof);
    }

    /// @dev A fresh copy: assigning a memory struct only copies the reference.
    function _copy(IZKDecoOracle.Attestation memory a) internal pure returns (IZKDecoOracle.Attestation memory) {
        return IZKDecoOracle.Attestation({
            providerId: a.providerId, queryCommitment: a.queryCommitment, price: a.price, observedAt: a.observedAt
        });
    }

    // ------------------------------------------------------------------ freshness through the pool

    function test_aRealAttestationReachesThePool_andAgesOutAfter60s() public {
        assertEq(address(pool.oracle()), address(feed));
        (, FeedFixtures.Attested memory f) = _attestation(0);

        vm.warp(f.observedAt + 2);
        vm.expectRevert(DarkPool.StaleOracle.selector); // nothing pushed yet: (0, 0) is stale
        pool.extractableValue();

        _push(0);
        (uint256 price, uint64 updatedAt) = feed.latestVerifiedPrice();
        assertEq(price, 3456.789012e18);
        assertEq(updatedAt, f.observedAt);
        assertGt(pool.extractableValue(), 0, "pool at 3500, oracle at 3456.79: value to harvest");

        vm.warp(f.observedAt + 60); // DarkPool.MAX_ORACLE_AGE, measured from the observation
        assertGt(pool.extractableValue(), 0);
        vm.warp(f.observedAt + 61);
        vm.expectRevert(DarkPool.StaleOracle.selector);
        pool.extractableValue();
    }

    function test_theFeedFollowsTheSeries_15sApart() public {
        for (uint256 i; i < FeedFixtures.COUNT; ++i) {
            (, FeedFixtures.Attested memory f) = _attestation(i);
            vm.warp(f.observedAt + 3); // proving and inclusion
            _push(i);
            (uint256 price, uint64 updatedAt) = feed.latestVerifiedPrice();
            assertEq(price, f.price);
            assertEq(updatedAt, f.observedAt);
            assertGt(pool.extractableValue(), 0);
        }
        (uint256 last,) = feed.latestVerifiedPrice();
        assertEq(last, 3458.0000000001e18, "ten fractional digits, exact");

        // Replaying an older, valid attestation is refused. (The proof is built first: building it calls the oracle,
        // and expectRevert applies to the next external call.)
        (IZKDecoOracle.Attestation memory a1, FeedFixtures.Attested memory f1) = _attestation(1);
        (, FeedFixtures.Attested memory f3) = _attestation(3);
        bytes memory replay = _proof(f1.snark, f1.bodyCommitment, NOTARY_PK);
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.NotNewer.selector, f1.observedAt, f3.observedAt));
        feed.submit(a1, replay);
    }

    // ------------------------------------------------------------------ what the feed refuses

    function test_tamperedOrForeignAttestationsAreRefused() public {
        (IZKDecoOracle.Attestation memory a, FeedFixtures.Attested memory f) = _attestation(0);
        vm.warp(f.observedAt + 2);
        // Every proof is built before its expectRevert: building one calls the oracle, and expectRevert applies to the
        // next external call.
        bytes memory good = _proof(f.snark, f.bodyCommitment, NOTARY_PK);
        bytes memory wrongSigner = _proof(f.snark, f.bodyCommitment, 0xB0B);
        bytes memory wrongBody = _proof(f.snark, keccak256("another body"), NOTARY_PK);

        // A different price than the one proven.
        IZKDecoOracle.Attestation memory t = _copy(a);
        t.price = a.price + 1;
        vm.expectRevert(ZkPriceFeed.InvalidAttestation.selector);
        feed.submit(t, good);

        // A different observation time than the one proven (still inside the window).
        t = _copy(a);
        t.observedAt = a.observedAt + 1;
        vm.expectRevert(ZkPriceFeed.InvalidAttestation.selector);
        feed.submit(t, good);

        // The right proof, signed by a key that is not the provider's notary.
        vm.expectRevert(ZkPriceFeed.InvalidAttestation.selector);
        feed.submit(a, wrongSigner);

        // A body commitment the notary signed but the proof is not about.
        vm.expectRevert(ZkPriceFeed.InvalidAttestation.selector);
        feed.submit(a, wrongBody);

        // A truncated proof.
        bytes memory cut = new bytes(good.length - 1);
        for (uint256 i; i < cut.length; ++i) {
            cut[i] = good[i];
        }
        vm.expectRevert(ZkPriceFeed.InvalidAttestation.selector);
        feed.submit(a, cut);

        // A genuine attestation, but of another provider and instrument (the repo's XAUUSD fixture).
        IZKDecoOracle.Attestation memory xau = IZKDecoOracle.Attestation({
            providerId: Fixtures.ORACLE_PROVIDER_ID,
            queryCommitment: Fixtures.ORACLE_QUERY_COMMITMENT,
            price: Fixtures.ORACLE_PRICE,
            observedAt: uint64(block.timestamp)
        });
        vm.expectRevert(abi.encodeWithSelector(ZkPriceFeed.WrongProvider.selector, Fixtures.ORACLE_PROVIDER_ID));
        feed.submit(xau, good);

        (uint256 price, uint64 updatedAt) = feed.latestVerifiedPrice();
        assertEq(price, 0, "nothing was stored");
        assertEq(updatedAt, 0);

        // The untouched attestation still goes through.
        feed.submit(a, good);
        (price,) = feed.latestVerifiedPrice();
        assertEq(price, f.price);
    }

    // ------------------------------------------------------------------ economics at the attested price

    /// A searcher sells token0 into the pool (3500) down to the attested price (3456.789012): K does not fall, the
    /// pool lands on the oracle, and the Treasury is paid 80 % of the harvest measured at the attested price.
    function test_aFlashRebalanceSettlesAtTheAttestedPrice() public {
        FeedFixtures.Attested memory f = _push0();
        uint256 p = f.price;

        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 k = x * y;
        uint256 in0 = Math.sqrt(k * WAD / p) - x;
        uint256 yNew = (k + (x + in0) - 1) / (x + in0); // ceil
        uint256 out1 = y - yNew;
        uint256 cut = ((x * p / WAD + y) - ((x + in0) * p / WAD + yNew)) * 8_000 / 10_000;
        assertEq(pool.quoteMinPayment(uint256(pool.reserve0()) + in0, uint256(pool.reserve1()) - out1), cut);

        MockSearcher s = new MockSearcher(token0, token1, address(pool), address(treasury));
        token0.mint(address(s), in0);
        s.configure(in0, 0, 0); // repays token0 in the callback; the pool pulls the Treasury's cut in token1
        uint256 kBefore = pool.invariantK();
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, out1, address(s), "")));

        assertGe(pool.invariantK(), kBefore, "I1: K does not fall");
        assertApproxEqRel(pool.spotPrice(), p, 1e9, "the pool now prices token0 at the attested price");
        assertApproxEqAbs(token1.balanceOf(address(treasury)), cut, 1, "80 % of the harvest");
        assertEq(treasury.recorded(), token1.balanceOf(address(treasury)));
    }

    function _push0() internal returns (FeedFixtures.Attested memory f) {
        (, f) = _attestation(0);
        vm.warp(f.observedAt + 2);
        _push(0);
    }

    /// What a push costs (the relayer pays it every ~15 s on testnet).
    function test_gasOfAPush() public {
        (IZKDecoOracle.Attestation memory a, FeedFixtures.Attested memory f) = _attestation(0);
        vm.warp(f.observedAt + 2);
        bytes memory proof = _proof(f.snark, f.bodyCommitment, NOTARY_PK);
        uint256 before = gasleft();
        feed.submit(a, proof);
        uint256 used = before - gasleft();
        emit log_named_uint("ZkPriceFeed.submit gas (first push, cold slot)", used);
        assertLt(used, 400_000);
    }
}
