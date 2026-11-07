// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {SovereignBank, DemandDeposits, TimeDeposits, IYieldAdapter} from "../src/banking/SovereignBank.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {MockERC20} from "./mocks/Mocks.sol";
import {RebasingToken, FeeToken, PullAdapter, OpenPushAdapter, ThiefAdapter, DeadAdapter} from "./mocks/BankMocks.sol";

/// @notice Module 5: demand deposits (pure custody) and time deposits (HOLD / ROUTED), including the Phase-8 fixes
///         M-1 (pro-rata exits instead of a global freeze) and H-6 (pull-measured ROUTED settlement).
contract SovereignBankTest is Test {
    DemandDeposits dd;
    TimeDeposits td;
    MockERC20 usd;
    address alice = address(0xA11CE);
    address bob = address(0xB0B);
    address mallory = address(0x3A11);

    function setUp() public {
        SovereignBank bank = new SovereignBank();
        dd = bank.demand();
        td = bank.time();
        usd = new MockERC20("USD");
        vm.warp(1_000_000);
    }

    function _deposit(IERC20Min t, address who, uint256 amt) internal {
        MockERC20(address(t)).mint(who, amt);
        vm.startPrank(who);
        MockERC20(address(t)).approve(address(dd), amt);
        dd.deposit(t, amt);
        vm.stopPrank();
    }

    function _open(IERC20Min t, address who, uint256 amt, uint64 term, IYieldAdapter a) internal returns (uint256 id) {
        MockERC20(address(t)).mint(who, amt);
        vm.startPrank(who);
        MockERC20(address(t)).approve(address(td), amt);
        id = td.open(t, amt, term, a, "");
        vm.stopPrank();
    }

    // ============================================================== demand deposits
    function test_demand_depositWithdrawRoundTrip() public {
        _deposit(usd, alice, 1_000e18);
        assertEq(dd.balanceOf(usd, alice), 1_000e18);
        assertEq(dd.totalDeposits(usd), 1_000e18);
        vm.prank(alice);
        dd.withdraw(usd, 400e18, bob);
        assertEq(usd.balanceOf(bob), 400e18);
        assertEq(dd.balanceOf(usd, alice), 600e18);
        assertEq(usd.balanceOf(address(dd)), dd.totalDeposits(usd));
    }

    function test_demand_cannotWithdrawSomeoneElsesMoneyOrMoreThanOwned() public {
        _deposit(usd, alice, 100e18);
        vm.prank(bob);
        vm.expectRevert(DemandDeposits.InsufficientBalance.selector);
        dd.withdraw(usd, 1, bob);
        vm.prank(alice);
        vm.expectRevert(DemandDeposits.InsufficientBalance.selector);
        dd.withdraw(usd, 100e18 + 1, alice);
        vm.prank(alice);
        vm.expectRevert(DemandDeposits.AmountZero.selector);
        dd.withdraw(usd, 0, alice);
        vm.prank(alice);
        vm.expectRevert(DemandDeposits.InvalidRecipient.selector);
        dd.withdraw(usd, 1, address(0));
    }

    function test_demand_feeOnTransferTokensAreRejected() public {
        FeeToken fee = new FeeToken(10);
        fee.mint(alice, 100e18);
        vm.startPrank(alice);
        fee.approve(address(dd), 100e18);
        vm.expectRevert(DemandDeposits.FeeOnTransferUnsupported.selector);
        dd.deposit(IERC20Min(address(fee)), 100e18);
        vm.stopPrank();
    }

    /// M-1: a token-side loss is shared pro rata, in any order, and nobody is frozen.
    function testFuzz_demand_shortfallIsSharedProRataInAnyOrder(uint256 lossBps, bool bobFirst) public {
        lossBps = bound(lossBps, 1, 5_000);
        RebasingToken t = new RebasingToken();
        _deposit(IERC20Min(address(t)), alice, 1_000e18);
        _deposit(IERC20Min(address(t)), bob, 3_000e18);
        t.rebaseDown(address(dd), lossBps);
        uint256 held = t.balanceOf(address(dd));
        (address first, address second) = bobFirst ? (bob, alice) : (alice, bob);
        uint256 owedFirst = dd.balanceOf(IERC20Min(address(t)), first); // read BEFORE vm.prank
        uint256 owedSecond = dd.balanceOf(IERC20Min(address(t)), second);
        vm.prank(first);
        dd.withdraw(IERC20Min(address(t)), owedFirst, first);
        vm.prank(second);
        dd.withdraw(IERC20Min(address(t)), owedSecond, second);
        // each received deposit·held/total (± rounding), whatever the order
        assertApproxEqAbs(t.balanceOf(alice), 1_000e18 * held / 4_000e18, 2);
        assertApproxEqAbs(t.balanceOf(bob), 3_000e18 * held / 4_000e18, 2);
        assertLe(t.balanceOf(alice) + t.balanceOf(bob), held);
    }

    // ============================================================== time deposits: HOLD
    function test_time_holdPaysPrincipalAtMaturityOnly() public {
        uint256 id = _open(usd, alice, 500e18, 30 days, IYieldAdapter(address(0)));
        vm.prank(alice);
        vm.expectRevert(TimeDeposits.NotMatured.selector);
        td.settle(id, alice);
        vm.warp(block.timestamp + 30 days);
        vm.prank(bob);
        vm.expectRevert(TimeDeposits.NotOwner.selector);
        td.settle(id, bob);
        vm.prank(alice);
        td.settle(id, alice);
        assertEq(usd.balanceOf(alice), 500e18);
        vm.prank(alice);
        vm.expectRevert(TimeDeposits.AlreadyClosed.selector);
        td.settle(id, alice);
    }

    function test_time_termBounds() public {
        usd.mint(alice, 1e18);
        vm.startPrank(alice);
        usd.approve(address(td), 1e18);
        vm.expectRevert(TimeDeposits.BadTerm.selector);
        td.open(usd, 1e18, 1 days - 1, IYieldAdapter(address(0)), "");
        vm.expectRevert(TimeDeposits.BadTerm.selector);
        td.open(usd, 1e18, 5 * 365 days + 1, IYieldAdapter(address(0)), "");
        vm.stopPrank();
    }

    /// M-1 in HOLD mode: a token-side loss is shared, never a freeze.
    function test_time_holdShortfallIsSharedProRata() public {
        RebasingToken t = new RebasingToken();
        uint256 a = _open(IERC20Min(address(t)), alice, 1_000e18, 1 days, IYieldAdapter(address(0)));
        uint256 b = _open(IERC20Min(address(t)), bob, 1_000e18, 1 days, IYieldAdapter(address(0)));
        t.rebaseDown(address(td), 100); // -1 %
        vm.warp(block.timestamp + 1 days);
        vm.prank(alice);
        td.settle(a, alice);
        vm.prank(bob);
        td.settle(b, bob);
        assertEq(t.balanceOf(alice), 990e18);
        assertEq(t.balanceOf(bob), 990e18);
    }

    // ============================================================== time deposits: ROUTED
    function test_time_routedPullsExactlyWhatTheAdapterReturns() public {
        PullAdapter ad = new PullAdapter();
        ad.setYieldBps(500); // +5 %
        uint256 id = _open(usd, alice, 1_000e18, 90 days, ad);
        assertEq(usd.balanceOf(address(ad)), 1_000e18, "the exact deposit was pushed to the adapter");
        vm.warp(block.timestamp + 90 days);
        vm.prank(alice);
        td.settle(id, alice);
        assertEq(usd.balanceOf(alice), 1_050e18);
        assertEq(usd.balanceOf(address(td)), 0);
    }

    function test_time_routedLossIsBorneByThatDepositOnly() public {
        PullAdapter ad = new PullAdapter();
        ad.setYieldBps(-2_000); // the strategy lost 20 %
        uint256 held = _open(usd, bob, 300e18, 1 days, IYieldAdapter(address(0)));
        uint256 id = _open(usd, alice, 1_000e18, 1 days, ad);
        vm.warp(block.timestamp + 1 days);
        vm.prank(alice);
        td.settle(id, alice);
        assertEq(usd.balanceOf(alice), 800e18);
        vm.prank(bob);
        td.settle(held, bob);
        assertEq(usd.balanceOf(bob), 300e18, "HOLD money is untouched by another deposit's loss");
    }

    function test_time_routedWriteOffOnlyAfterGrace() public {
        DeadAdapter ad = new DeadAdapter();
        uint256 id = _open(usd, alice, 1_000e18, 1 days, ad);
        vm.warp(block.timestamp + 1 days);
        vm.prank(alice);
        vm.expectRevert(TimeDeposits.RecallFailed.selector);
        td.settle(id, alice);
        vm.warp(block.timestamp + td.RECALL_GRACE());
        vm.prank(alice);
        td.settle(id, alice); // written off: closed, nothing paid, nobody else touched
        (,,,,, bool closed,) = td.deposits(id);
        assertTrue(closed);
        assertEq(usd.balanceOf(alice), 0);
    }

    /// H-6: an adapter that triggers someone else's repayment inside settle() gains nothing.
    function test_H6_thiefAdapterCannotCaptureAnotherRepayment() public {
        OpenPushAdapter honest = new OpenPushAdapter();
        ThiefAdapter thief = new ThiefAdapter(honest);
        uint256 aliceId = _open(usd, alice, 1_000_000e18, 1 days, honest);
        (,,,,,, bytes32 h) = td.deposits(aliceId);
        thief.setTarget(h);
        uint256 malloryId = _open(usd, mallory, 1, 1 days, thief);
        vm.warp(block.timestamp + 1 days);
        vm.prank(mallory);
        td.settle(malloryId, mallory);
        assertEq(usd.balanceOf(mallory), 0, "the thief is credited only what its own adapter hands over");
    }

    /// With a compliant (pull, depositor-only) adapter the attack cannot even start.
    function test_H6_compliantAdapterCannotBeTriggeredByAThirdParty() public {
        PullAdapter honest = new PullAdapter();
        uint256 aliceId = _open(usd, alice, 1_000e18, 1 days, honest);
        (,,,,,, bytes32 h) = td.deposits(aliceId);
        ThiefAdapter thief = new ThiefAdapter(honest);
        thief.setTarget(h);
        uint256 malloryId = _open(usd, mallory, 1, 1 days, thief);
        vm.warp(block.timestamp + 1 days);
        vm.prank(mallory);
        vm.expectRevert(TimeDeposits.RecallFailed.selector);
        td.settle(malloryId, mallory);
        vm.prank(alice);
        td.settle(aliceId, alice);
        assertEq(usd.balanceOf(alice), 1_000e18);
    }
}
