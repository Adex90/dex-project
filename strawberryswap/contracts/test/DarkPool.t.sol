// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {DarkPoolBase} from "./DarkPoolBase.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {NoteTree} from "../src/privacy/NoteTree.sol";
import {Math} from "../src/utils/Math.sol";
import {MockSearcher, ExpectingVerifier} from "./mocks/Mocks.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";

/// @notice Fuzz + unit tests for the DarkPool curve. Theorem under test:
///           K = (R0+v0)(R1+v1) never decreases across swaps (batch settlement) or rebalances, and rounding
///           always favours the pool, so no sequence of trades can slowly extract value.
contract DarkPoolTest is DarkPoolBase {
    function setUp() public {
        _deploy();
        vm.warp(1_000_000);
    }

    function _seed(uint256 r0, uint256 r1) internal {
        r0 = bound(r0, 1e6, 1e30);
        r1 = bound(r1, 1e6, 1e30);
        _addLiquidity(lp, r0, r1);
    }

    // ================================================================== axioms
    function test_retailFeeIsZeroByConstruction() public view {
        assertEq(pool.RETAIL_FEE_BPS(), 0);
        assertEq(pool.EXTRACTION_BPS(), 8_000);
    }

    // ================================================================== 1. quote maths
    /// out = ⌊rout·in/(rin+in)⌋  ⇒  (rin+in)(rout−out) ≥ rin·rout  for ALL reserves and sizes.
    function testFuzz_quoteNeverDecreasesK(uint256 r0, uint256 r1, uint256 amountIn, bool zeroForOne) public {
        _seed(r0, r1);
        amountIn = bound(amountIn, 1, pool.MAX_RESERVE());
        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 out = pool.getAmountOut(amountIn, zeroForOne);
        (uint256 rin, uint256 rout) = zeroForOne ? (x, y) : (y, x);
        assertLt(out, rout, "a swap can never drain the pool");
        assertGe((rin + amountIn) * (rout - out), rin * rout, "K must not decrease");
    }

    /// Rounding is strictly in the pool's favour whenever it matters: the trader never gets more than the
    /// exact real-valued output.   out ≤ rout·in/(rin+in)  (cross-multiplied to stay in integers)
    function testFuzz_quoteNeverExceedsExactOutput(uint256 r0, uint256 r1, uint256 amountIn, bool zeroForOne) public {
        _seed(r0, r1);
        amountIn = bound(amountIn, 1, pool.MAX_RESERVE());
        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 out = pool.getAmountOut(amountIn, zeroForOne);
        (uint256 rin, uint256 rout) = zeroForOne ? (x, y) : (y, x);
        assertLe(out * (rin + amountIn), rout * amountIn);
    }

    // ================================================================== 2. batch settlement
    /// Any aggregate flow the aggregator proposes: it is accepted iff K' ≥ K. Never otherwise.
    function testFuzz_settleBatchEnforcesKMonotonicity(
        uint256 r0,
        uint256 r1,
        uint256 in0,
        uint256 in1,
        uint256 out0,
        uint256 out1
    ) public {
        _seed(r0, r1);
        in0 = bound(in0, 0, 1e30);
        in1 = bound(in1, 0, 1e30);
        out0 = bound(out0, 0, uint256(pool.reserve0()) + in0);
        out1 = bound(out1, 0, uint256(pool.reserve1()) + in1);
        _custody(0, in0 + 1);
        _custody(1, in1 + 1);

        uint256 kBefore = _k();
        uint256 n0 = uint256(pool.reserve0()) + in0 - out0;
        uint256 n1 = uint256(pool.reserve1()) + in1 - out1;
        bool kOk = (n0 + pool.virtual0()) * (n1 + pool.virtual1()) >= kBefore;
        DarkPool.Batch memory b = _batch(in0, in1, out0, out1);

        try pool.settleBatch(b) {
            assertTrue(kOk, "accepted a batch that lowers K");
            assertGe(_k(), kBefore, "K decreased after an accepted batch");
        } catch {
            // may revert for other reasons (ReserveOverflow) but MUST revert if K would fall
            assertTrue(!kOk || n0 > pool.MAX_RESERVE() || n1 > pool.MAX_RESERVE());
        }
    }

    /// Swaps executed exactly at the quote keep K non-decreasing; one wei more than the quote is rejected.
    function testFuzz_swapAtQuoteOkOneWeiMoreRejected(uint256 r0, uint256 r1, uint256 amountIn, bool zeroForOne) public {
        _seed(r0, r1);
        amountIn = bound(amountIn, 1e3, 1e27);
        uint256 out = pool.getAmountOut(amountIn, zeroForOne);
        vm.assume(out > 0 && out + 1 < (zeroForOne ? pool.reserve1() : pool.reserve0()));
        _custody(zeroForOne ? 0 : 1, amountIn);
        uint256 kBefore = _k();
        // greedy by 1 wei beyond the curve
        DarkPool.Batch memory greedy = zeroForOne ? _batch(amountIn, 0, 0, out + 1) : _batch(0, amountIn, out + 1, 0);
        // It is only forbidden when floor-rounding slack is exhausted; assert via the exact predicate:
        (uint256 x, uint256 y) = pool.effectiveReserves();
        bool greedyOk = zeroForOne ? (x + amountIn) * (y - out - 1) >= x * y : (y + amountIn) * (x - out - 1) >= x * y;
        if (!greedyOk) vm.expectRevert(DarkPool.InvariantBroken.selector);
        pool.settleBatch(greedy);
        if (greedyOk) {
            assertGe(_k(), kBefore);
            return;
        }
        DarkPool.Batch memory fair = zeroForOne ? _batch(amountIn, 0, 0, out) : _batch(0, amountIn, out, 0);
        pool.settleBatch(fair);
        assertGe(_k(), kBefore);
    }

    /// A trader who swaps A→B and immediately back B→A never ends with more than they started.
    function testFuzz_roundTripNeverProfits(uint256 r0, uint256 r1, uint256 amount, bool zeroForOne) public {
        _seed(r0, r1);
        amount = bound(amount, 1, 1e27);
        uint256 a = zeroForOne ? 0 : 1;
        uint256 b = 1 - a;
        _custody(a, amount);
        _custody(b, 0 + 1); // ensure custody exists for both assets

        uint256 got = pool.getAmountOut(amount, zeroForOne);
        // the curve quotes against EFFECTIVE reserves; settlement can only pay out REAL reserves
        vm.assume(got > 0 && got < (zeroForOne ? pool.reserve1() : pool.reserve0()));
        pool.settleBatch(zeroForOne ? _batch(amount, 0, 0, got) : _batch(0, amount, got, 0));
        _custody(b, got);
        uint256 back = pool.getAmountOut(got, !zeroForOne);
        assertLe(back, amount, "round trip created value");
    }

    /// Many consecutive round trips with random sizes: the trader's holdings never increase. (The
    /// "death by a thousand roundings" scenario.)
    function testFuzz_manyRoundTripsNeverExtractValue(uint256 seed) public {
        _addLiquidity(lp, 5e24, 1e25);
        uint256 holding = 1_000e18; // token0
        uint256 start = holding;
        _custody(0, 1e27);
        _custody(1, 1e27);
        for (uint256 i; i < 20; ++i) {
            uint256 amt = bound(uint256(keccak256(abi.encode(seed, i))), 1, holding);
            uint256 out1 = pool.getAmountOut(amt, true);
            pool.settleBatch(_batch(amt, 0, 0, out1));
            uint256 out0 = pool.getAmountOut(out1, false);
            pool.settleBatch(_batch(0, out1, out0, 0));
            holding = holding - amt + out0;
            assertLe(holding, start, "holdings grew through rounding");
        }
    }

    // ================================================================== 3. liquidity can't be gamed either
    function testFuzz_addThenRemoveNeverProfits(uint256 r0, uint256 r1, uint256 a0, uint256 a1) public {
        _seed(r0, r1);
        a0 = bound(a0, 1e6, 1e30);
        a1 = bound(a1, 1e6, 1e30);
        address who = address(0x3333);
        // a deposit that would mint 0 shares is (correctly) refused by the pool: not a case to test here
        uint256 expectMint = Math.min(a0 * pool.totalShares() / pool.reserve0(), a1 * pool.totalShares() / pool.reserve1());
        vm.assume(expectMint > 0);
        uint256 minted = _addLiquidity(who, a0, a1);
        vm.prank(who);
        (uint256 o0, uint256 o1) = pool.removeLiquidity(minted, who);
        assertLe(o0, a0, "LP extracted token0 from nothing");
        assertLe(o1, a1, "LP extracted token1 from nothing");
    }

    function test_minimumLiquidityIsPermanentlyLocked() public {
        _addLiquidity(lp, 1e24, 2e24);
        assertEq(pool.shares(address(0)), pool.MINIMUM_LIQUIDITY());
        uint256 all = pool.shares(lp);
        vm.prank(lp);
        pool.removeLiquidity(all, lp);
        assertGt(pool.totalShares(), 0, "pool can never be fully drained");
    }

    // ================================================================== 4. flashRebalance
    function _rebalanceSetup() internal returns (MockSearcher s) {
        _addLiquidity(lp, 1_000_000e18, 2_000_000e18); // x = 2e24, y = 4e24 -> spot 2.0
        s = new MockSearcher(token0, token1, address(pool), address(treasury));
        oracle.set(1.9e18); // oracle says 1.9, pool says 2.0: pool token0 is too expensive
    }

    /// Full rebalance towards the oracle: pool needs more token0 and gives out token1.
    function _plan() internal view returns (uint256 in0, uint256 out1, uint256 treasuryReq, uint256 vb, uint256 va) {
        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 P = 1.9e18;
        uint256 k = x * y;
        uint256 xt = Math.sqrt(k * WAD / P);
        in0 = xt - x;
        uint256 yNew = (k + (x + in0) - 1) / (x + in0); // ceil
        out1 = y - yNew;
        vb = x * P / WAD + y;
        va = (x + in0) * P / WAD + yNew;
        treasuryReq = (vb - va) * 8_000 / 10_000;
    }

    function test_flashRebalance_happyPathPaysTreasury() public {
        MockSearcher s = _rebalanceSetup();
        (uint256 in0, uint256 out1, uint256 req,,) = _plan();
        token0.mint(address(s), in0);
        token1.mint(address(s), req);
        s.configure(in0, 0, req);
        uint256 kBefore = _k();
        uint256 tBefore = token1.balanceOf(address(treasury));
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, out1, address(s), "")));
        assertGe(_k(), kBefore);
        assertGe(token1.balanceOf(address(treasury)) - tBefore, req);
        assertLt(pool.spotPrice(), 2e18, "price moved toward the oracle");
    }

    function test_flashRebalance_underpayingTreasuryReverts() public {
        MockSearcher s = _rebalanceSetup();
        (uint256 in0, uint256 out1, uint256 req,,) = _plan();
        token0.mint(address(s), in0);
        token1.mint(address(s), req);
        s.configure(in0, 0, req - 1); // one wei short
        vm.expectRevert(abi.encodeWithSelector(DarkPool.TreasuryUnderpaid.selector, req, req - 1));
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, out1, address(s), "")));
    }

    function test_flashRebalance_takingValueWithoutRepayingBreaksK() public {
        MockSearcher s = _rebalanceSetup();
        s.configure(0, 0, 0);
        vm.expectRevert(DarkPool.InvariantBroken.selector);
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, 1e18, address(s), "")));
    }

    function test_flashRebalance_movingAwayFromOracleReverts() public {
        MockSearcher s = _rebalanceSetup();
        // pool price 2.0 > oracle 1.9; pushing it further UP (taking token0, paying token1) is not rebalancing
        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 out0 = 1e18;
        uint256 in1 = (x * y + (x - out0) - 1) / (x - out0) - y + 1; // exactly enough to keep K
        token1.mint(address(s), in1);
        s.configure(0, in1, 0);
        vm.expectRevert(DarkPool.DeviationNotReduced.selector);
        s.go(abi.encodeCall(DarkPool.flashRebalance, (out0, 0, address(s), "")));
    }

    function test_flashRebalance_staleOracleReverts() public {
        MockSearcher s = _rebalanceSetup();
        vm.warp(block.timestamp + pool.MAX_ORACLE_AGE() + 1);
        vm.expectRevert(DarkPool.StaleOracle.selector);
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, 1e18, address(s), "")));
    }

    /// For ARBITRARY searcher behaviour: if flashRebalance succeeds, every acceptance rule holds.
    function testFuzz_flashRebalanceSuccessImpliesAllRules(
        uint256 priceBps,
        uint256 out0Seed,
        uint256 out1Seed,
        uint256 pay0Seed,
        uint256 pay1Seed,
        uint256 treasurySeed
    ) public {
        _addLiquidity(lp, 1_000_000e18, 2_000_000e18);
        MockSearcher s = new MockSearcher(token0, token1, address(pool), address(treasury));
        uint256 spot = pool.spotPrice();
        uint256 P = spot * bound(priceBps, 9_000, 11_000) / 10_000;
        oracle.set(P);

        uint256 out0 = bound(out0Seed, 0, 1e24);
        uint256 out1 = bound(out1Seed, 0, 2e24);
        if (out0 == 0 && out1 == 0) out1 = 1e18;
        uint256 pay0 = bound(pay0Seed, 0, 2e24);
        uint256 pay1 = bound(pay1Seed, 0, 4e24);
        uint256 tPay = bound(treasurySeed, 0, 1e24);
        token0.mint(address(s), pay0);
        token1.mint(address(s), pay1 + tPay);
        s.configure(pay0, pay1, tPay);

        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 kBefore = x * y;
        uint256 devBefore = _dev(x, y, P);
        uint256 vb = x * P / WAD + y;
        uint256 tBefore = token1.balanceOf(address(treasury));

        try s.go(abi.encodeCall(DarkPool.flashRebalance, (out0, out1, address(s), ""))) {
            (uint256 nx, uint256 ny) = pool.effectiveReserves();
            assertGe(nx * ny, kBefore, "I1: K decreased");
            assertLt(_dev(nx, ny, P), devBefore, "I3a: deviation not reduced");
            uint256 va = nx * P / WAD + ny;
            uint256 harvest = vb > va ? vb - va : 0;
            assertGe(token1.balanceOf(address(treasury)) - tBefore, harvest * 8_000 / 10_000, "I3b: treasury underpaid");
        } catch {}
    }

    function _dev(uint256 x, uint256 y, uint256 P) internal pure returns (uint256) {
        return Math.absDiff(y * WAD / x, P) * 10_000 / P;
    }

    /// A donation is absorbed into reserves, so it LOWERS the harvest the Treasury's 80 % is computed on.
    /// This test pins down the exact relationship and shows it is no exploit: whoever donates D gives up the
    /// full value D·P to save only 80 % of it in Treasury payments (net loss of 20 % · D·P).
    function test_donationLowersTreasuryDueByExactlyEightyPercentAndCostsMoreThanItSaves() public {
        MockSearcher s = _rebalanceSetup();
        (uint256 in0, uint256 out1, uint256 reqA,,) = _plan();
        uint256 P = 1.9e18;
        uint256 D = 100e18; // token0 donated to the pool
        uint256 saving = D * P / WAD * 8_000 / 10_000;
        uint256 reqB = reqA - saving; // expected requirement with the donation present

        token0.mint(address(s), in0);
        token1.mint(address(s), reqA);
        uint256 snap = vm.snapshotState();

        // (1) with donation, paying slightly LESS than reqB reverts ...
        token0.mint(address(pool), D);
        s.configure(in0, 0, reqB - 1e6);
        vm.expectRevert();
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, out1, address(s), "")));
        // (2) ... and paying slightly MORE succeeds
        s.configure(in0, 0, reqB + 1e6);
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, out1, address(s), "")));

        // (3) economics: donor pays D·P (value gifted to LPs) and saves `saving` < D·P
        assertLt(saving, D * P / WAD, "a donor always loses more than they save");
        assertGe(reqA - reqB, saving - 1);
        vm.revertToState(snap); // (baseline untouched)
    }

    // ================================================================== 5. batch plumbing
    function test_settleBatchRejectsBadProofReplayAndUnknownRoot() public {
        _addLiquidity(lp, 1e24, 2e24);
        _custody(0, 1e21);
        DarkPool.Batch memory b = _batch(1e20, 0, 0, 1);
        batchV.setAccept(false);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.settleBatch(b);
        batchV.setAccept(true);
        pool.settleBatch(b);
        vm.expectRevert(NoteTree.NullifierReuse.selector); // replay of the same batch
        pool.settleBatch(b);
        DarkPool.Batch memory stale = _batch(1e20, 0, 0, 1);
        stale.root = bytes32(uint256(1));
        vm.expectRevert(NoteTree.UnknownRoot.selector);
        pool.settleBatch(stale);
    }

    function test_settleBatchCannotSpendMoreCustodyThanExists() public {
        _addLiquidity(lp, 1e24, 2e24);
        _custody(0, 1e18);
        DarkPool.Batch memory b = _batch(1e18 + 1, 0, 0, 0);
        vm.expectRevert(); // checked arithmetic: custody underflow
        pool.settleBatch(b);
    }

    // ---- batch circuit interface: [root, tNow, X, Y, in0, in1, out0, out1, nullifiers(N), commitments(N)]
    function test_batchPublicInputLayoutMatchesTheCircuit() public {
        _addLiquidity(lp, 1e24, 2e24);
        _custody(0, 1e21);
        ExpectingVerifier ev = new ExpectingVerifier();
        // fresh pool wired to the layout-checking verifier, batch size 4
        DarkPool p = new DarkPool(
            pool.token0(), pool.token1(), V0, V1, joinSplitV, ev, IPoseidonHasher(address(hasher)), oracle, treasury, 4
        );
        token0.mint(lp, 1e24);
        token1.mint(lp, 2e24);
        vm.startPrank(lp);
        token0.approve(address(p), type(uint256).max);
        token1.approve(address(p), type(uint256).max);
        p.addLiquidity(1e24, 2e24, lp);
        vm.stopPrank();
        token0.mint(trader, 1e21);
        NoteTree.JoinSplit memory js = _js();
        js.root = p.currentRoot();
        vm.startPrank(trader);
        token0.approve(address(p), 1e21);
        p.depositNote(0, 1e21, js);
        vm.stopPrank();

        DarkPool.Batch memory b = _batch(1e20, 0, 0, 7);
        b.root = p.currentRoot();
        b.nullifiers = new bytes32[](2);
        b.commitments = new bytes32[](2);
        b.encryptedNotes = new bytes[](2);
        (b.nullifiers[0], b.nullifiers[1]) = (bytes32(uint256(11)), bytes32(uint256(12)));
        (b.commitments[0], b.commitments[1]) = (bytes32(uint256(21)), bytes32(uint256(22)));
        uint256[] memory e = new uint256[](16); // 8 + 2*4
        e[0] = uint256(b.root); e[1] = b.tNow; e[2] = 1e24 + V0; e[3] = 2e24 + V1;
        e[4] = 1e20; e[5] = 0; e[6] = 0; e[7] = 7;
        e[8] = 11; e[9] = 12; /* e[10], e[11] = 0 padding */ e[12] = 21; e[13] = 22; /* e[14], e[15] = 0 */
        ev.expect(e);
        p.settleBatch(b); // reverts InvalidProof if ANY public input differs from the circuit's order

        e[2] += 1; // pool state differs by 1 wei => proof built for another state must fail
        ev.expect(e);
        DarkPool.Batch memory b2 = _batch(1e20, 0, 0, 7);
        b2.root = p.currentRoot();
        vm.expectRevert(NoteTree.InvalidProof.selector);
        p.settleBatch(b2);
    }

    function test_batchClockWindowEnforced() public {
        _addLiquidity(lp, 1e24, 2e24);
        _custody(0, 1e21);
        DarkPool.Batch memory b = _batch(1e20, 0, 0, 1);
        b.tNow = uint64(block.timestamp + 1); // clock in the future
        vm.expectRevert(DarkPool.StaleBatchClock.selector);
        pool.settleBatch(b);
        b.tNow = uint64(block.timestamp - pool.BATCH_CLOCK_WINDOW() - 1); // too old
        vm.expectRevert(DarkPool.StaleBatchClock.selector);
        pool.settleBatch(b);
        b.tNow = uint64(block.timestamp - pool.BATCH_CLOCK_WINDOW()); // exactly at the edge: ok
        pool.settleBatch(b);
    }

    function test_batchShapeChecks() public {
        _addLiquidity(lp, 1e24, 2e24);
        _custody(0, 1e21);
        DarkPool.Batch memory b = _batch(1e20, 0, 0, 1);
        b.commitments = new bytes32[](2); // commitments != nullifiers
        vm.expectRevert(DarkPool.BadBatch.selector);
        pool.settleBatch(b);
        b = _batch(1e20, 0, 0, 1);
        b.nullifiers = new bytes32[](0);
        b.commitments = new bytes32[](0);
        b.encryptedNotes = new bytes[](0);
        vm.expectRevert(DarkPool.BadBatch.selector); // empty batch
        pool.settleBatch(b);
        b = _batch(1e20, 0, 0, 1);
        uint256 n = pool.batchSize() + 1; // more intents than circuit slots
        b.nullifiers = new bytes32[](n);
        b.commitments = new bytes32[](n);
        b.encryptedNotes = new bytes[](n);
        vm.expectRevert(DarkPool.BadBatch.selector);
        pool.settleBatch(b);
    }

    function test_donationCannotBrickPool() public {
        _addLiquidity(lp, 1e24, 2e24);
        token0.mint(address(pool), 12345);
        _custody(0, 1e21);
        pool.settleBatch(_batch(1e20, 0, 0, 1)); // balance >= reserve+custody still holds
    }
}
