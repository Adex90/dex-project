// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {SecurityBase} from "./security/SecurityBase.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {SearcherExecutor, IDarkPoolSearcher} from "../src/periphery/SearcherExecutor.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";

/// @notice The on-chain half of the searcher bot against the real DarkPool: the pool pulls the exact Treasury cut,
///         `treasuryPay` caps it, and nobody but the operator (through the pool) can make the executor move funds.
contract SearcherExecutorTest is SecurityBase {
    DarkPool p;
    SearcherExecutor ex;
    address operator = address(0x0BE);
    uint256 constant P = 1.9e18;

    function setUp() public {
        _tokens();
        vm.warp(1_000_000);
        p = _pool(t0, t1, 1_000_000e18, 2_000_000e18);
        _addLiq(p, lp, 100_000e18, 200_000e18); // spot 2.0
        oracle.set(P);                           // pool overprices token0: sell token0 into it
        ex = new SearcherExecutor(operator, IDarkPoolSearcher(address(p)));
    }

    function _plan() internal view returns (uint256 a0, uint256 o1, uint256 required) {
        a0 = _sell0ToReach(p, P);
        o1 = _quoteSell0(p, a0);
        required = p.quoteMinPayment(uint256(p.reserve0()) + a0, uint256(p.reserve1()) - o1);
    }

    function test_rebalancePaysExactlyTheCutAndResetsTheAllowance() public {
        (uint256 a0, uint256 o1, uint256 required) = _plan();
        t0.mint(address(ex), a0);
        vm.prank(operator);
        ex.rebalance(0, o1, a0, 0, required + 1e18); // the cap may exceed the cut: only the cut is pulled
        assertEq(t1.balanceOf(address(treasury)), required);
        assertEq(t1.balanceOf(address(ex)), o1 - required, "the searcher keeps its 20 %");
        assertEq(t1.allowance(address(ex), address(p)), 0);
        assertApproxEqRel(p.spotPrice(), P, 1e12);
    }

    function test_treasuryPayIsAHardCap() public {
        (uint256 a0, uint256 o1, uint256 required) = _plan();
        t0.mint(address(ex), a0);
        vm.prank(operator);
        vm.expectRevert(); // the pool's pull exceeds the allowance: the whole rebalance reverts
        ex.rebalance(0, o1, a0, 0, required - 1);
    }

    function test_onlyTheOperatorTradesOrSweeps() public {
        vm.expectRevert(SearcherExecutor.NotOperator.selector);
        ex.rebalance(0, 1, 0, 0, 0);
        t1.mint(address(ex), 5e18);
        vm.expectRevert(SearcherExecutor.NotOperator.selector);
        ex.sweep(IERC20Min(address(t1)), address(this), 5e18);
        vm.prank(operator);
        ex.sweep(IERC20Min(address(t1)), operator, 5e18);
        assertEq(t1.balanceOf(operator), 5e18);
    }

    function test_nobodyElseCanDriveTheCallback() public {
        t0.mint(address(ex), 100e18);
        vm.expectRevert(SearcherExecutor.NotPool.selector);
        ex.onFlashRebalance(address(ex), 0, 0, abi.encode(uint256(100e18), uint256(0), uint256(0)));
        // a third party naming the executor as `to` cannot make it repay anything
        vm.expectRevert(SearcherExecutor.NoActiveRebalance.selector);
        p.flashRebalance(0, 1e18, address(ex), abi.encode(uint256(100e18), uint256(0), uint256(0)));
    }
}
