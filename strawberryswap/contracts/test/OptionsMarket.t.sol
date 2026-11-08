// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OptionsMarket} from "../src/derivatives/OptionsMarket.sol";
import {NoteTree} from "../src/privacy/NoteTree.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";
import {MockERC20, MockVerifier, MockHasher} from "./mocks/Mocks.sol";
import {RebasingToken, FeeToken} from "./mocks/BankMocks.sol";

/// @notice Module 3: fully collateralised European options. Covers the write / close / exercise / claim lifecycle for
///         calls and puts, the 100 % coverage gate, the ERC-1155 subset, the shielded domain, and the Phase-8 fix M-2
///         (a token-side shortfall is shared pro rata instead of freezing every series of that token).
contract OptionsMarketTest is Test {
    OptionsMarket om;
    MockERC20 weth;
    MockERC20 usd;
    address alice = address(0xA11CE); // writer
    address bob = address(0xB0B);     // writer / holder
    address carol = address(0xCA201); // holder
    uint64 expiry;
    uint256 nonce;

    function setUp() public {
        om = new OptionsMarket(IZKVerifier(address(new MockVerifier(true))), IPoseidonHasher(address(new MockHasher())));
        weth = new MockERC20("WETH");
        usd = new MockERC20("USD");
        vm.warp(1_000_000);
        expiry = uint64(block.timestamp + 30 days);
    }

    function _call(IERC20Min u) internal view returns (OptionsMarket.SeriesParams memory) {
        return OptionsMarket.SeriesParams(u, IERC20Min(address(usd)), 1e18, 2_000e18, expiry, true);
    }

    function _put() internal view returns (OptionsMarket.SeriesParams memory) {
        return OptionsMarket.SeriesParams(IERC20Min(address(weth)), IERC20Min(address(usd)), 1e18, 2_000e18, expiry, false);
    }

    function _write(OptionsMarket.SeriesParams memory p, address writer, uint256 qty, address to) internal returns (uint256 id) {
        MockERC20 c = MockERC20(address(p.isCall ? p.underlying : p.quote));
        uint256 amt = qty * (p.isCall ? p.underlyingPerContract : p.quotePerContract);
        c.mint(writer, amt);
        vm.startPrank(writer);
        c.approve(address(om), amt);
        id = om.mintOptionToken(p, qty, to);
        vm.stopPrank();
    }

    // ============================================================== calls
    function test_callLifecycle_writeExerciseClaim() public {
        uint256 id = _write(_call(IERC20Min(address(weth))), alice, 10, carol);
        assertEq(weth.balanceOf(address(om)), 10e18, "100 % collateral pulled");
        assertEq(om.balanceOf(id, carol), 10);
        assertEq(om.liabilities(address(weth)), 10e18);

        vm.prank(carol);
        vm.expectRevert(OptionsMarket.OutsideExerciseWindow.selector);
        om.exercise(id, 1, carol); // European: never before expiry

        vm.warp(expiry);
        usd.mint(carol, 8_000e18);
        vm.startPrank(carol);
        usd.approve(address(om), 8_000e18);
        om.exercise(id, 4, carol); // pays 4 × 2000, receives 4 underlying
        vm.stopPrank();
        assertEq(weth.balanceOf(carol), 4e18);
        assertEq(usd.balanceOf(carol), 0);

        vm.warp(expiry + om.EXERCISE_WINDOW());
        vm.prank(carol);
        vm.expectRevert(OptionsMarket.OutsideExerciseWindow.selector);
        om.exercise(id, 1, carol);
        vm.prank(alice);
        om.claimWriter(id, alice);
        assertEq(weth.balanceOf(alice), 6e18, "unexercised collateral back");
        assertEq(usd.balanceOf(alice), 8_000e18, "exercise proceeds");
        assertEq(om.liabilities(address(weth)), 0);
        assertEq(om.liabilities(address(usd)), 0);
    }

    function test_putLifecycle() public {
        uint256 id = _write(_put(), alice, 3, carol);
        assertEq(usd.balanceOf(address(om)), 6_000e18, "put collateral = strike in quote");
        vm.warp(expiry + 1);
        weth.mint(carol, 2e18);
        vm.startPrank(carol);
        weth.approve(address(om), 2e18);
        om.exercise(id, 2, carol);
        vm.stopPrank();
        assertEq(usd.balanceOf(carol), 4_000e18);
        vm.warp(expiry + om.EXERCISE_WINDOW());
        vm.prank(alice);
        om.claimWriter(id, alice);
        assertEq(usd.balanceOf(alice), 2_000e18);
        assertEq(weth.balanceOf(alice), 2e18);
    }

    function test_closePositionBeforeExpiry() public {
        uint256 id = _write(_call(IERC20Min(address(weth))), alice, 5, alice);
        vm.prank(alice);
        om.closePosition(id, 2);
        assertEq(weth.balanceOf(alice), 2e18);
        assertEq(om.written(id), 3);
        vm.warp(expiry);
        vm.prank(alice);
        vm.expectRevert(OptionsMarket.Expired.selector);
        om.closePosition(id, 1);
    }

    function test_twoWritersClaimProRataAndNothingIsLeftBehind() public {
        OptionsMarket.SeriesParams memory p = _call(IERC20Min(address(weth)));
        uint256 id = _write(p, alice, 3, carol);
        _write(p, bob, 4, carol);
        vm.warp(expiry);
        usd.mint(carol, 10_000e18);
        vm.startPrank(carol);
        usd.approve(address(om), 10_000e18);
        om.exercise(id, 5, carol);
        vm.stopPrank();
        vm.warp(expiry + om.EXERCISE_WINDOW());
        vm.prank(alice);
        om.claimWriter(id, alice);
        vm.prank(bob);
        om.claimWriter(id, bob);
        assertEq(weth.balanceOf(alice) + weth.balanceOf(bob), 2e18);
        assertEq(usd.balanceOf(alice) + usd.balanceOf(bob), 10_000e18);
        assertEq(weth.balanceOf(address(om)), 0, "no dust left");
        assertEq(usd.balanceOf(address(om)), 0);
        vm.prank(alice);
        vm.expectRevert(OptionsMarket.NothingToClaim.selector);
        om.claimWriter(id, alice);
    }

    function test_feeOnTransferCollateralIsRejected() public {
        FeeToken fee = new FeeToken(10);
        OptionsMarket.SeriesParams memory p = _call(IERC20Min(address(fee)));
        fee.mint(alice, 1e18);
        vm.startPrank(alice);
        fee.approve(address(om), 1e18);
        vm.expectRevert(OptionsMarket.CollateralShortfall.selector);
        om.mintOptionToken(p, 1, alice);
        vm.stopPrank();
    }

    // ============================================================== M-2 regression
    function test_M2_shortfallIsSharedNotAFreeze() public {
        RebasingToken under = new RebasingToken();
        uint256 idA = _write(_call(IERC20Min(address(under))), alice, 10, alice);
        OptionsMarket.SeriesParams memory pB = _call(IERC20Min(address(under)));
        pB.quotePerContract = 2_500e18;
        uint256 idB = _write(pB, bob, 10, bob);
        under.rebaseDown(address(om), 100); // -1 % of the underlying
        vm.prank(bob);
        om.closePosition(idB, 10); // used to revert Insolvent: now paid 99 %
        assertEq(under.balanceOf(bob), 9.9e18);
        vm.warp(expiry + om.EXERCISE_WINDOW());
        vm.prank(alice);
        om.claimWriter(idA, alice);
        assertEq(under.balanceOf(alice), 9.9e18);
    }

    function test_M2_noNewLiabilitiesAgainstAShortToken() public {
        RebasingToken under = new RebasingToken();
        _write(_call(IERC20Min(address(under))), alice, 10, alice);
        under.rebaseDown(address(om), 100);
        OptionsMarket.SeriesParams memory p = _call(IERC20Min(address(under)));
        under.mint(bob, 1e18);
        vm.startPrank(bob);
        under.approve(address(om), 1e18);
        vm.expectRevert(OptionsMarket.Insolvent.selector);
        om.mintOptionToken(p, 1, bob);
        vm.stopPrank();
    }

    // ============================================================== ERC-1155 subset and the shielded domain
    function test_publicTransfersAndApprovals() public {
        uint256 id = _write(_call(IERC20Min(address(weth))), alice, 5, alice);
        vm.prank(alice);
        om.safeTransferFrom(alice, bob, id, 2, "");
        assertEq(om.balanceOf(id, bob), 2);
        vm.prank(carol);
        vm.expectRevert(OptionsMarket.NotAuthorized.selector);
        om.safeTransferFrom(alice, carol, id, 1, "");
        vm.prank(alice);
        om.setApprovalForAll(carol, true);
        vm.prank(carol);
        om.safeTransferFrom(alice, carol, id, 1, "");
        assertEq(om.balanceOf(id, carol), 1);
        vm.prank(alice);
        vm.expectRevert(OptionsMarket.InvalidRecipient.selector);
        om.safeTransferFrom(alice, address(om), id, 1, "");
    }

    function _js() internal returns (NoteTree.JoinSplit memory js) {
        js.proof = hex"01";
        js.root = om.currentRoot();
        js.nullifiers = [bytes32(++nonce), bytes32(++nonce)];
        js.commitments = [bytes32(++nonce), bytes32(++nonce)];
        js.encryptedNotes = [bytes(""), bytes("")];
    }

    function test_shieldAndUnshieldRespectTheCeiling() public {
        uint256 id = _write(_call(IERC20Min(address(weth))), alice, 5, alice);
        NoteTree.JoinSplit memory js = _js();
        vm.prank(alice);
        om.shield(id, 3, js);
        assertEq(om.shieldedSupply(id), 3);
        assertEq(om.balanceOf(id, alice), 2);
        js = _js();
        vm.expectRevert(OptionsMarket.ShieldedSupplyExceeded.selector);
        om.unshield(id, 4, bob, js); // a forged proof still cannot release more than was shielded
        om.unshield(id, 3, bob, js);
        assertEq(om.balanceOf(id, bob), 3);
        assertEq(om.totalSupply(id), 5);
    }
}
