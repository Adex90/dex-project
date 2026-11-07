// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {StdInvariant} from "forge-std/StdInvariant.sol";
import {DarkPoolBase} from "./DarkPoolBase.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {Math} from "../src/utils/Math.sol";
import {MockSearcher} from "./mocks/Mocks.sol";

/// @dev Random-walk handler over every state-changing path that touches the curve. It keeps ghost counters
///      of VIOLATIONS; the invariant tests assert they are all zero after hundreds of random sequences.
contract PoolHandler is DarkPoolBase {
    address[3] public lps = [address(0x1111), address(0x4444), address(0x5555)];
    MockSearcher public searcher;

    uint256 public kViolations; // an accepted swap/rebalance lowered K
    uint256 public dilutions; // an LP op lowered per-share reserves
    uint256 public treasuryViolations; // an accepted rebalance under-paid the Treasury
    uint256 public greedyAccepted; // a batch that took more than the curve allows was accepted
    // coverage counters
    uint256 public swapsOk;
    uint256 public greedyBlocked;
    uint256 public rebalancesOk;
    uint256 public lpOps;

    constructor() {
        _deploy();
        vm.warp(1_000_000);
        searcher = new MockSearcher(token0, token1, address(pool), address(treasury));
        _addLiquidity(lps[0], 1_000_000e18, 2_000_000e18);
        _custody(0, 1e30);
        _custody(1, 1e30);
    }

    function poolAddr() external view returns (DarkPool) { return pool; }
    function t0() external view returns (address) { return address(token0); }
    function t1() external view returns (address) { return address(token1); }
    function treasuryAddr() external view returns (address) { return address(treasury); }
    function lpAt(uint256 i) external view returns (address) { return lps[i]; }

    // ---------------------------------------------------------------- liquidity
    function lpAdd(uint256 who, uint256 a0, uint256 a1) external {
        address w = lps[who % 3];
        a0 = bound(a0, 1e3, 1e25);
        a1 = bound(a1, 1e3, 1e25);
        (uint256 r0, uint256 r1, uint256 ts) = (pool.reserve0(), pool.reserve1(), pool.totalShares());
        token0.mint(w, a0);
        token1.mint(w, a1);
        vm.startPrank(w);
        token0.approve(address(pool), a0);
        token1.approve(address(pool), a1);
        try pool.addLiquidity(a0, a1, w) {
            // per-share reserves must not fall:  r' / ts'  >=  r / ts   (cross-multiplied, exact)
            if (pool.reserve0() * ts < r0 * pool.totalShares() || pool.reserve1() * ts < r1 * pool.totalShares()) dilutions++;
            lpOps++;
        } catch {}
        vm.stopPrank();
    }

    function lpRemove(uint256 who, uint256 shareSeed) external {
        address w = lps[who % 3];
        uint256 bal = pool.shares(w);
        if (bal == 0) return;
        uint256 sh = bound(shareSeed, 1, bal);
        (uint256 r0, uint256 r1, uint256 ts) = (pool.reserve0(), pool.reserve1(), pool.totalShares());
        vm.prank(w);
        try pool.removeLiquidity(sh, w) {
            if (pool.reserve0() * ts < r0 * pool.totalShares() || pool.reserve1() * ts < r1 * pool.totalShares()) dilutions++;
            lpOps++;
        } catch {}
    }

    // ---------------------------------------------------------------- swaps (private batch settlement)
    /// `greedy` wei beyond the curve quote must always be rejected; 0 must be accepted (when fillable).
    function swap(bool zeroForOne, uint256 amtSeed, uint256 greedy) external {
        uint256 amt = bound(amtSeed, 1, 1e26);
        uint256 out = pool.getAmountOut(amt, zeroForOne);
        uint256 reserveOut = zeroForOne ? pool.reserve1() : pool.reserve0();
        if (out == 0 || out >= reserveOut) return;
        greedy = bound(greedy, 0, 3) == 0 ? bound(greedy, 1, out / 50 + 1) : 0; // 1 in 4 attempts is greedy
        uint256 claimed = out + greedy;
        if (claimed >= reserveOut) return;

        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 kBefore = x * y;
        DarkPool.Batch memory b = zeroForOne ? _batch(amt, 0, 0, claimed) : _batch(0, amt, claimed, 0);
        bool shouldReject = zeroForOne ? (x + amt) * (y - claimed) < kBefore : (y + amt) * (x - claimed) < kBefore;
        try pool.settleBatch(b) {
            if (shouldReject) greedyAccepted++;
            if (_k() < kBefore) kViolations++;
            swapsOk++;
        } catch {
            if (shouldReject) greedyBlocked++;
        }
    }

    // ---------------------------------------------------------------- MEV rebalance (guided, sometimes under-paying)
    function rebalance(uint256 devSeed, uint256 fracSeed, uint256 paySeed) external {
        (uint256 x, uint256 y) = pool.effectiveReserves();
        uint256 spot = y * WAD / x;
        uint256 P = spot * bound(devSeed, 9_500, 10_500) / 10_000;
        if (P == spot || P == 0) return;
        oracle.set(P);
        uint256 frac = bound(fracSeed, 1, 10_000); // how much of the full rebalance to attempt
        uint256 payBps = bound(paySeed, 7_500, 10_500); // sometimes below the required 80 %
        uint256 k = x * y;
        uint256 xt = Math.sqrt(k * WAD / P);
        uint256 in0; uint256 out0; uint256 in1; uint256 out1;
        uint256 xNew; uint256 yNew;
        if (spot > P) { // pool token0 too expensive: searcher SELLS token0 into the pool
            in0 = (xt - x) * frac / 10_000;
            if (in0 == 0) return;
            xNew = x + in0;
            yNew = (k + xNew - 1) / xNew;
            out1 = y - yNew;
        } else { // pool token0 too cheap: searcher BUYS token0 from the pool
            out0 = (x - xt) * frac / 10_000;
            if (out0 == 0) return;
            xNew = x - out0;
            yNew = (k + xNew - 1) / xNew;
            in1 = yNew - y;
        }
        if (out1 >= pool.reserve1() || out0 >= pool.reserve0()) return;
        uint256 vb = x * P / WAD + y;
        uint256 va = xNew * P / WAD + yNew;
        uint256 required = vb > va ? (vb - va) * 8_000 / 10_000 : 0; // planning estimate (ignores donations)
        uint256 pay = required * payBps / 10_000;

        token0.mint(address(searcher), in0);
        token1.mint(address(searcher), in1 + pay);
        searcher.configure(in0, in1, pay);
        uint256 tBefore = token1.balanceOf(address(treasury));
        try searcher.go(abi.encodeCall(DarkPool.flashRebalance, (out0, out1, address(searcher), ""))) {
            if (_k() < k) kViolations++;
            // The contract's rule (I3b) measures harvest on the pool's ACTUAL post-call reserves, which also
            // absorb any donated surplus. Re-derive that exact quantity independently and check payment.
            (uint256 nx, uint256 ny) = pool.effectiveReserves();
            uint256 vAfter = nx * P / WAD + ny;
            uint256 requiredActual = vb > vAfter ? (vb - vAfter) * 8_000 / 10_000 : 0;
            if (token1.balanceOf(address(treasury)) - tBefore < requiredActual) treasuryViolations++;
            rebalancesOk++;
        } catch {}
    }

    // ---------------------------------------------------------------- hostile donation
    function donate(bool which, uint256 amt) external {
        amt = bound(amt, 1, 1e24);
        (which ? token0 : token1).mint(address(pool), amt);
    }
}

contract DarkPoolInvariantTest is StdInvariant, DarkPoolBase {
    PoolHandler h;
    DarkPool p;

    function setUp() public {
        h = new PoolHandler();
        p = h.poolAddr();
        targetContract(address(h));
    }

    /// Guard against VACUOUS invariants: drive the handler directly and prove every interesting path is
    /// reachable and actually executes (accepted swaps, blocked greedy batches, successful rebalances, LP ops).
    function test_handlerExercisesEveryPath() public {
        for (uint256 i = 1; i <= 400; ++i) {
            uint256 r = uint256(keccak256(abi.encode("cov", i)));
            h.swap(r % 2 == 0, r >> 8, r >> 16);
            h.rebalance(r >> 24, r >> 32, 10_000 + (r >> 40) % 600); // pay >= required most of the time
            h.lpAdd(r >> 48, r >> 56, r >> 64);
            h.lpRemove(r >> 72, r >> 80);
            if (i % 50 == 0) h.donate(r % 2 == 0, r >> 88);
        }
        assertGt(h.swapsOk(), 20, "accepted swaps");
        assertGt(h.greedyBlocked(), 5, "blocked greedy batches");
        assertGt(h.rebalancesOk(), 10, "successful rebalances");
        assertGt(h.lpOps(), 20, "LP operations");
        assertEq(h.kViolations(), 0);
        assertEq(h.dilutions(), 0);
        assertEq(h.treasuryViolations(), 0);
        assertEq(h.greedyAccepted(), 0);
    }

    /// K never decreases across any accepted swap or rebalance (LP add/remove change K legitimately and are
    /// covered by the per-share invariant below).
    function invariant_kMonotoneAcrossSwapsAndRebalances() public view {
        assertEq(h.kViolations(), 0);
    }

    /// An LP operation never lowers reserves-per-share, so LPs cannot dilute or be diluted by rounding.
    function invariant_noLiquidityDilution() public view {
        assertEq(h.dilutions(), 0);
    }

    /// An accepted rebalance paid the Treasury at least 80 % of the harvested value.
    function invariant_treasuryAlwaysPaid() public view {
        assertEq(h.treasuryViolations(), 0);
    }

    /// A batch claiming even 1 wei beyond the curve was never accepted.
    function invariant_greedyBatchNeverAccepted() public view {
        assertEq(h.greedyAccepted(), 0);
    }

    /// 100 % reserve: real balances always cover curve reserves + note custody, for both assets.
    function invariant_poolIsFullyBacked() public view {
        assertGe(IERC(h.t0()).balanceOf(address(p)), uint256(p.reserve0()) + p.custody0());
        assertGe(IERC(h.t1()).balanceOf(address(p)), uint256(p.reserve1()) + p.custody1());
    }

    function invariant_reservesWithinBounds() public view {
        assertLe(p.reserve0(), p.MAX_RESERVE());
        assertLe(p.reserve1(), p.MAX_RESERVE());
        assertGt(p.totalShares(), 0);
    }

    /// Σ LP shares + the permanently locked minimum == totalShares.
    function invariant_sharesAddUp() public view {
        uint256 sum = p.shares(address(0));
        for (uint256 i; i < 3; ++i) sum += p.shares(h.lpAt(i));
        assertEq(sum, p.totalShares());
    }
}

interface IERC {
    function balanceOf(address) external view returns (uint256);
}
