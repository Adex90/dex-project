// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {SecurityBase} from "./SecurityBase.sol";
import {DarkPool} from "../../src/dex/DarkPool.sol";
import {NoteTree} from "../../src/privacy/NoteTree.sol";
import {Math} from "../../src/utils/Math.sol";
import {IERC20Min} from "../../src/interfaces/IERC20Min.sol";
import {IFlashRebalancer} from "../../src/interfaces/IFlashRebalancer.sol";
import {MockERC20, MockSearcher} from "../mocks/Mocks.sol";

/// @notice Regression suite for the Phase-8 DarkPool / NoteTree findings (docs/AUDIT_PHASE8.md). Every test replays
///         the original exploit against the fixed code and asserts the exact guard that now stops it, plus the
///         honest path that must keep working.
contract DarkPoolRegressionTest is SecurityBase {
    function setUp() public {
        _tokens();
        vm.warp(1_000_000);
    }

    /// deposit + transferNote into an intent note, ready for a batch (the exploit's first two steps)
    function _intent(DarkPool p, address who, bool sell0, uint256 amountIn) internal {
        MockERC20 tin = MockERC20(address(sell0 ? p.token0() : p.token1()));
        tin.mint(who, amountIn);
        vm.startPrank(who);
        tin.approve(address(p), amountIn);
        p.depositNote(sell0 ? 0 : 1, amountIn, _js(p));
        p.transferNote(sell0 ? 0 : 1, _js(p));
        vm.stopPrank();
    }

    // ============================================================== C-1  batch arbitrage paid nothing to the Treasury
    function test_C1_batchArbitrageTowardTheOracleReverts() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        _addLiq(p, lp, 100_000e18, 200_000e18); // spot 2.0
        oracle.set(1.9e18);                      // the pool overprices token0
        uint256 a0 = _sell0ToReach(p, 1.9e18);
        _intent(p, attacker, true, a0);
        DarkPool.Batch memory b = _oneIntentBatch(p, true, a0, _quoteSell0(p, a0));
        vm.expectRevert(DarkPool.ValueLeak.selector);
        p.settleBatch(b);
    }

    function test_C1_valueMovingBatchNeedsAFreshOracle_refundsDoNot() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        _addLiq(p, lp, 100_000e18, 200_000e18);
        oracle.set(2e18);
        vm.warp(block.timestamp + p.MAX_ORACLE_AGE() + 1); // oracle now stale
        _intent(p, victim, true, 10e18);
        DarkPool.Batch memory b = _oneIntentBatch(p, true, 10e18, _quoteSell0(p, 10e18));
        vm.expectRevert(DarkPool.StaleOracle.selector);
        p.settleBatch(b);
        // a refund-only batch moves no value and never reads the oracle
        DarkPool.Batch memory r = _oneIntentBatch(p, true, 0, 0);
        p.settleBatch(r);
    }

    function test_C1_retailTradeAtTheOraclePriceSettles() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        _addLiq(p, lp, 100_000e18, 200_000e18);
        oracle.set(p.spotPrice());
        uint256 got = _swapViaBatch(p, victim, true, 1_000e18);
        assertEq(t1.balanceOf(victim), got);
        assertGt(got, 0);
    }

    /// The designed arbitrage path still works and pays exactly 80 % of the harvest, PULLED by the pool.
    function test_C1_flashRebalancePaysExactlyTheCut() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        _addLiq(p, lp, 100_000e18, 200_000e18);
        uint256 P = 1.9e18;
        oracle.set(P);
        uint256 a0 = _sell0ToReach(p, P);
        uint256 o1 = _quoteSell0(p, a0);
        uint256 required = p.quoteMinPayment(uint256(p.reserve0()) + a0, uint256(p.reserve1()) - o1);
        MockSearcher s = new MockSearcher(IERC20Min(address(t0)), IERC20Min(address(t1)), address(p), address(treasury));
        t0.mint(address(s), a0);
        s.configure(a0, 0, 0); // pays the Treasury NOTHING itself
        s.go(abi.encodeCall(DarkPool.flashRebalance, (0, o1, address(s), "")));
        assertEq(t1.balanceOf(address(treasury)), required, "the pool pulled exactly the cut");
        assertGt(required, 0);
    }

    // ============================================================== C-2  liquidity as a price weapon
    function _poolAt2400() internal returns (DarkPool p) {
        p = _pool(t0, t1, 1_000e18, 2_000_000e18); // launch 2000, deep virtual liquidity
        _addLiq(p, lp, 100e18, 200_000e18);
        uint256 P = 2_400e18;
        oracle.set(P);
        // the market moves; a searcher rebalances through the designed path
        (uint256 x, uint256 y) = p.effectiveReserves();
        uint256 xt = Math.sqrt(x * y * WAD / P);
        uint256 out0 = x - xt;
        uint256 in1 = x * y / (x - out0) + 1 - y;
        uint256 req = p.quoteMinPayment(uint256(p.reserve0()) - out0, uint256(p.reserve1()) + in1);
        MockSearcher s = new MockSearcher(IERC20Min(address(t0)), IERC20Min(address(t1)), address(p), address(treasury));
        t1.mint(address(s), in1 + req);
        s.configure(0, in1, 0);
        s.go(abi.encodeCall(DarkPool.flashRebalance, (out0, 0, address(s), "")));
    }

    function test_C2_proportionalLiquidityNeverMovesThePrice() public {
        DarkPool p = _poolAt2400();
        uint256 spot0 = p.spotPrice();
        uint256 sh = _addLiq(p, attacker, uint256(p.reserve0()) * 100, uint256(p.reserve1()) * 100);
        assertApproxEqRel(p.spotPrice(), spot0, 1e6, "deposit moved the price"); // 1e-12
        vm.prank(attacker);
        p.removeLiquidity(sh, attacker);
        assertApproxEqRel(p.spotPrice(), spot0, 1e6, "withdrawal moved the price");
    }

    function test_C2_flashLiquidityRoundTripExtractsNothing() public {
        DarkPool p = _poolAt2400();
        uint256 P = 2_400e18;
        uint256 honest = (uint256(p.reserve0()) * P / WAD + p.reserve1()) * p.shares(lp) / p.totalShares();
        uint256 a0 = uint256(p.reserve0()) * 100;
        uint256 a1 = uint256(p.reserve1()) * 100;
        uint256 sh = _addLiq(p, attacker, a0, a1);
        vm.prank(attacker);
        (uint256 b0, uint256 b1) = p.removeLiquidity(sh, attacker);
        assertLe(b0, a0);
        assertLe(b1, a1);
        uint256 honestAfter = (uint256(p.reserve0()) * P / WAD + p.reserve1()) * p.shares(lp) / p.totalShares();
        assertGe(honestAfter, honest, "honest LP lost value");
    }

    // ============================================================== H-1  nested rebalances paid once
    function test_H1_nestedRebalancesEachPayTheirFullCut() public {
        MockERC20[3] memory s3 = [new MockERC20("X"), new MockERC20("Y"), new MockERC20("Z")];
        for (uint256 i; i < 3; ++i) for (uint256 j = i + 1; j < 3; ++j) if (address(s3[j]) < address(s3[i])) (s3[i], s3[j]) = (s3[j], s3[i]);
        (MockERC20 tA, MockERC20 tB, MockERC20 quote) = (s3[0], s3[1], s3[2]);
        DarkPool pA = _pool(tA, quote, 1_000_000e18, 2_000_000e18);
        DarkPool pB = _pool(tB, quote, 1_000_000e18, 2_000_000e18);
        _addLiq(pA, lp, 100_000e18, 200_000e18);
        _addLiq(pB, lp, 100_000e18, 200_000e18);
        uint256 P = 1.9e18;
        oracle.set(P);
        (uint256 inA, uint256 outA, uint256 reqA) = _plan(pA, P);
        (uint256 inB, uint256 outB, uint256 reqB) = _plan(pB, P);
        NestedSearcher ns = new NestedSearcher(pA, pB, tA, tB, quote);
        tA.mint(address(ns), inA);
        tB.mint(address(ns), inB);
        ns.run(inA, outA, inB, outB);
        assertEq(quote.balanceOf(address(treasury)), reqA + reqB, "each pool pulled its own cut");
    }

    function _plan(DarkPool p, uint256 P) internal view returns (uint256 a0, uint256 o1, uint256 req) {
        a0 = _sell0ToReach(p, P);
        o1 = _quoteSell0(p, a0);
        req = p.quoteMinPayment(uint256(p.reserve0()) + a0, uint256(p.reserve1()) - o1);
    }

    // ============================================================== H-2  root eviction by junk transactions
    function test_H2_rootWindowIsExactly1024Transactions() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        t0.mint(victim, 10e18);
        vm.startPrank(victim);
        t0.approve(address(p), 10e18);
        p.depositNote(0, 10e18, _js(p));
        vm.stopPrank();
        NoteTree.JoinSplit memory userProof = _js(p);
        bytes32 r = userProof.root;
        for (uint256 i; i < 1023; ++i) p.transferNote(0, _js(p)); // junk: free to prove, 2 leaves each
        assertTrue(p.isKnownRoot(r), "1023 later transactions must not evict a root");
        vm.prank(victim);
        p.withdrawNote(0, 10e18, victim, userProof);
        assertTrue(!p.isKnownRoot(r), "the 1024th transaction evicts it");
    }

    // ============================================================== H-3  a full tree froze every note
    function test_H3_fullTreeRollsOverAndOldNotesStaySpendable() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        t0.mint(victim, 10e18);
        vm.startPrank(victim);
        t0.approve(address(p), 10e18);
        p.depositNote(0, 10e18, _js(p));
        vm.stopPrank();
        bytes32 s0 = vm.load(address(p), bytes32(uint256(0))); // nextLeafIndex is the low 32 bits of slot 0
        vm.store(address(p), bytes32(uint256(0)), bytes32((uint256(s0) & ~uint256(type(uint32).max)) | ((1 << 20) - 1)));

        NoteTree.JoinSplit memory junk = _js(p);
        p.transferNote(0, junk); // fills the last slot, then rolls over for its second output
        assertEq(p.treeId(), 1);
        assertEq(p.nextLeafIndex(), 1);

        NoteTree.JoinSplit memory w = _js(p);
        vm.prank(victim);
        p.withdrawNote(0, 10e18, victim, w); // shielded funds are never frozen by capacity
        assertEq(t0.balanceOf(victim), 10e18);
        assertEq(p.custody0(), 0);
    }

    // ============================================================== H-4  the opening holder's sandwich
    function test_H4_sandwichBackRunIsRejected() public {
        DarkPool p = _pool(t0, t1, 1_000e18, 2_000_000e18);
        _addLiq(p, lp, 100e18, 200_000e18); // spot == oracle == 2000
        uint256 P = 2_000e18;
        oracle.set(P);
        _swapViaBatch(p, attacker, true, 5.5e18); // front-run: moves the pool away from the oracle (allowed)
        _swapViaBatch(p, victim, true, 10e18);    // the victim's batch
        // back-run: buying token0 back moves the pool TOWARD the oracle = value leaving the pool at P
        uint256 a1 = 10_000e18;
        _intent(p, attacker, false, a1);
        DarkPool.Batch memory b = _oneIntentBatch(p, false, a1, _quoteSell1(p, a1));
        vm.expectRevert(DarkPool.ValueLeak.selector);
        p.settleBatch(b);
    }

    // ============================================================== H-5  liveness of batch proofs
    /// The grief that used to void every in-flight proof (any 1-wei reserve change, e.g. an LP deposit) is tolerated:
    /// the proof was generated against a state that stopped being current within STATE_WINDOW_BLOCKS.
    function test_H5_batchProofSurvivesALiquidityChangeWithinTheWindow() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        _addLiq(p, lp, 100_000e18, 200_000e18);
        oracle.set(p.spotPrice());
        _intent(p, victim, true, 10e18);
        DarkPool.Batch memory b = _oneIntentBatch(p, true, 10e18, _quoteSell0(p, 10e18)); // proven against (X, Y)
        vm.roll(block.number + 1);
        _addLiq(p, address(0xBEEF), 1_000e18, 2_000e18); // state changes while the proof is generated
        p.settleBatch(b);
        assertEq(p.custody1(), b.out1, "the victim's output note is backed");
    }

    /// Residual, by design: if the state moved AGAINST the batch (a large withdrawal shrank the pool), the users' payouts
    /// are more generous than the current curve allows, and the K check against the CURRENT state still rejects it.
    function test_H5_adverseStateChangeStillFailsTheCurrentKCheck() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        uint256 sh = _addLiq(p, lp, 100_000e18, 200_000e18);
        oracle.set(p.spotPrice());
        _intent(p, victim, true, 10e18);
        DarkPool.Batch memory b = _oneIntentBatch(p, true, 10e18, _quoteSell0(p, 10e18));
        vm.prank(lp);
        p.removeLiquidity(sh / 2, lp);
        vm.expectRevert(DarkPool.InvariantBroken.selector);
        p.settleBatch(b);
    }

    function test_H5_staleOrUnknownStateIsRejected() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        uint256 sh = _addLiq(p, lp, 100_000e18, 200_000e18);
        oracle.set(p.spotPrice());
        _intent(p, victim, true, 10e18);
        DarkPool.Batch memory b = _oneIntentBatch(p, true, 10e18, _quoteSell0(p, 10e18));
        vm.prank(lp);
        p.removeLiquidity(sh / 1_000_000, lp);
        vm.roll(block.number + p.STATE_WINDOW_BLOCKS() + 1);
        vm.warp(block.timestamp + 10);
        oracle.set(p.spotPrice());
        b.tNow = uint64(block.timestamp);
        vm.expectRevert(DarkPool.StaleState.selector);
        p.settleBatch(b);
        b.X += 1; // a state the pool never had
        vm.expectRevert(DarkPool.StaleState.selector);
        p.settleBatch(b);
    }

    // ============================================================== L-1  zero entries inside n
    function test_L1_zeroEntriesAreRejected() public {
        DarkPool p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        _addLiq(p, lp, 100_000e18, 200_000e18);
        DarkPool.Batch memory b = _oneIntentBatch(p, true, 0, 0);
        b.nullifiers[0] = bytes32(0);
        vm.expectRevert(DarkPool.BadBatch.selector);
        p.settleBatch(b);
        b.nullifiers[0] = _fresh();
        b.commitments[0] = bytes32(0);
        vm.expectRevert(DarkPool.BadBatch.selector);
        p.settleBatch(b);
    }
}

/// Rebalances pool B inside pool A's callback (the H-1 attack shape). It pays the Treasury nothing itself: each pool
/// pulls its own cut.
contract NestedSearcher is IFlashRebalancer {
    DarkPool immutable a;
    DarkPool immutable b;
    MockERC20 immutable tA;
    MockERC20 immutable tB;
    uint256 inA;
    uint256 inB;
    uint256 outB;

    constructor(DarkPool a_, DarkPool b_, MockERC20 tA_, MockERC20 tB_, MockERC20 quote) {
        (a, b, tA, tB) = (a_, b_, tA_, tB_);
        quote.approve(address(a_), type(uint256).max);
        quote.approve(address(b_), type(uint256).max);
    }

    function run(uint256 inA_, uint256 outA, uint256 inB_, uint256 outB_) external {
        (inA, inB, outB) = (inA_, inB_, outB_);
        a.flashRebalance(0, outA, address(this), "");
    }

    function onFlashRebalance(address, uint256, uint256, bytes calldata) external {
        if (msg.sender == address(a)) {
            b.flashRebalance(0, outB, address(this), "");
            tA.transfer(address(a), inA);
        } else {
            tB.transfer(address(b), inB);
        }
    }
}
