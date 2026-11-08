// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {Berrycoin} from "../src/tokens/Berrycoin.sol";
import {NoteTree} from "../src/privacy/NoteTree.sol";
import {ReentrancyGuard} from "../src/utils/ReentrancyGuard.sol";
import {MockVerifier, MockHasher} from "./mocks/Mocks.sol";

/// @notice Unit + fuzz tests for Berrycoin. The theorem under test (A2/A3 in the contract header):
///           Σ publicBalance + shieldedSupply == MAX_SUPPLY   at every point in time,
///         INCLUDING when the verifier is broken and accepts forged proofs.
contract BerrycoinTest is Test {
    uint256 constant FIELD = 21888242871839275222246405745257275088548364400416034343698204186575808495617;

    Berrycoin berry;
    MockVerifier verifier;
    MockHasher hasher;
    address genesis = address(0xBEEF);
    address alice = address(0xA11CE);
    address bob = address(0xB0B);
    address mallory = address(0xBAD);
    uint256 nonce;

    function setUp() public {
        verifier = new MockVerifier(true);
        hasher = new MockHasher();
        berry = new Berrycoin(genesis, verifier, hasher);
        vm.startPrank(genesis);
        berry.transfer(alice, 1_000e18);
        berry.transfer(bob, 1_000e18);
        berry.transfer(mallory, 1_000e18);
        vm.stopPrank();
    }

    // ------------------------------------------------------------------ helpers
    function _fresh() internal returns (bytes32) {
        return bytes32(uint256(keccak256(abi.encode("fresh", ++nonce))) % FIELD);
    }

    function _js() internal returns (NoteTree.JoinSplit memory js) {
        js.proof = hex"01";
        js.root = berry.currentRoot();
        js.nullifiers = [_fresh(), _fresh()];
        js.commitments = [_fresh(), _fresh()];
        js.encryptedNotes = [bytes("enc0"), bytes("enc1")];
    }

    function _holders() internal view returns (uint256 s) {
        s = berry.balanceOf(genesis) + berry.balanceOf(alice) + berry.balanceOf(bob) + berry.balanceOf(mallory)
            + berry.balanceOf(address(berry));
    }

    function _assertConserved() internal view {
        assertEq(_holders() + berry.shieldedSupply(), berry.MAX_SUPPLY(), "A2: sum(balances)+shielded == MAX_SUPPLY");
        assertEq(berry.totalSupply(), berry.MAX_SUPPLY());
    }

    // ------------------------------------------------------------------ A1: genesis, no mint
    function test_genesisMintsEntireSupplyExactlyOnce() public view {
        assertEq(berry.MAX_SUPPLY(), 21_000_000e18);
        assertEq(berry.totalSupply(), berry.MAX_SUPPLY());
        assertEq(berry.genesisBlock(), block.number);
        assertEq(berry.shieldedSupply(), 0);
        _assertConserved();
    }

    function test_noMintBurnOwnerOrUpgradeEntrypointsExist() public {
        // Selectors that would constitute an issuance / admin / upgrade path. A contract with no fallback
        // reverts on every unknown selector, so each low-level call must fail.
        bytes[8] memory calls = [
            abi.encodeWithSignature("mint(address,uint256)", alice, 1),
            abi.encodeWithSignature("mint(uint256)", 1),
            abi.encodeWithSignature("burn(uint256)", 1),
            abi.encodeWithSignature("burnFrom(address,uint256)", alice, 1),
            abi.encodeWithSignature("owner()"),
            abi.encodeWithSignature("transferOwnership(address)", alice),
            abi.encodeWithSignature("upgradeTo(address)", alice),
            abi.encodeWithSignature("setMinter(address)", alice)
        ];
        for (uint256 i; i < calls.length; ++i) {
            (bool ok,) = address(berry).call(calls[i]);
            assertFalse(ok, "forbidden entrypoint exists");
        }
        // plain ETH / empty calldata must not be accepted either (no receive/fallback)
        (bool ok2,) = address(berry).call{value: 0}("");
        assertFalse(ok2);
    }

    // ------------------------------------------------------------------ public ERC-20 conservation
    function testFuzz_publicTransfersConserveSupply(uint256 amount, uint8 fromIdx, uint8 toIdx) public {
        address[4] memory a = [genesis, alice, bob, mallory];
        address from = a[fromIdx % 4];
        address to = a[toIdx % 4];
        amount = bound(amount, 0, berry.balanceOf(from));
        vm.prank(from);
        berry.transfer(to, amount);
        _assertConserved();
    }

    function testFuzz_transferMoreThanBalanceReverts(uint256 extra) public {
        extra = bound(extra, 1, type(uint128).max);
        uint256 tooMuch = berry.balanceOf(alice) + extra;
        vm.prank(alice);
        vm.expectRevert(Berrycoin.InsufficientBalance.selector);
        berry.transfer(bob, tooMuch);
    }

    function test_cannotTransferToZeroOrToContract() public {
        vm.startPrank(alice);
        vm.expectRevert(Berrycoin.InvalidRecipient.selector);
        berry.transfer(address(0), 1);
        vm.expectRevert(Berrycoin.InvalidRecipient.selector);
        berry.transfer(address(berry), 1);
        vm.stopPrank();
    }

    function testFuzz_transferFromHonoursAllowance(uint256 allowed, uint256 amount) public {
        allowed = bound(allowed, 0, 1_000e18);
        vm.prank(alice);
        berry.approve(bob, allowed);
        amount = bound(amount, 0, 1_000e18);
        vm.prank(bob);
        if (amount > allowed) {
            vm.expectRevert(Berrycoin.InsufficientAllowance.selector);
            berry.transferFrom(alice, bob, amount);
        } else {
            berry.transferFrom(alice, bob, amount);
            assertEq(berry.allowance(alice, bob), allowed - amount);
        }
        _assertConserved();
    }

    // ------------------------------------------------------------------ shielded flows (verifier accepts)
    function testFuzz_shieldUnshieldRoundTripConserves(uint256 amount) public {
        amount = bound(amount, 1, berry.balanceOf(alice));
        NoteTree.JoinSplit memory js0 = _js();
        vm.prank(alice);
        berry.shield(amount, js0);
        assertEq(berry.shieldedSupply(), amount);
        _assertConserved();

        berry.transferZK(_js()); // anyone may relay a private transfer; moves no public value
        assertEq(berry.shieldedSupply(), amount);

        berry.unshield(amount, bob, _js());
        assertEq(berry.shieldedSupply(), 0);
        assertEq(berry.balanceOf(bob), 1_000e18 + amount);
        _assertConserved();
    }

    function test_shieldMoreThanBalanceReverts() public {
        uint256 bal = berry.balanceOf(alice);
        NoteTree.JoinSplit memory js = _js();
        vm.prank(alice);
        vm.expectRevert(Berrycoin.InsufficientBalance.selector);
        berry.shield(bal + 1, js);
        assertEq(berry.shieldedSupply(), 0);
    }

    // ------------------------------------------------------------------ A3: forged proofs cannot inflate
    /// @dev The verifier here ACCEPTS EVERYTHING (forged / unsound proof system). An attacker may then
    ///      steal what is actually shielded, but can never obtain a single wei above it.
    function testFuzz_forgedUnshieldCannotInflateSupply(uint256 shielded, uint256 attempt) public {
        shielded = bound(shielded, 1, berry.balanceOf(alice));
        NoteTree.JoinSplit memory js0 = _js();
        vm.prank(alice);
        berry.shield(shielded, js0);

        attempt = bound(attempt, 1, type(uint256).max); // includes absurd, overflowing amounts
        uint256 malloryBefore = berry.balanceOf(mallory);
        NoteTree.JoinSplit memory forged = _js(); // arbitrary data, accepted by the broken verifier
        vm.prank(mallory);
        if (attempt > shielded) {
            vm.expectRevert(Berrycoin.ShieldedSupplyExceeded.selector);
            berry.unshield(attempt, mallory, forged);
            assertEq(berry.balanceOf(mallory), malloryBefore);
        } else {
            berry.unshield(attempt, mallory, forged);
            assertEq(berry.balanceOf(mallory), malloryBefore + attempt);
        }
        _assertConserved();
        assertLe(berry.shieldedSupply(), shielded);
    }

    function test_forgedUnshieldWithNothingShieldedReverts() public {
        NoteTree.JoinSplit memory forged = _js();
        vm.prank(mallory);
        vm.expectRevert(Berrycoin.ShieldedSupplyExceeded.selector);
        berry.unshield(1, mallory, forged);
    }

    function test_unshieldZeroAndBadRecipientRevert() public {
        NoteTree.JoinSplit memory js = _js();
        vm.expectRevert(Berrycoin.AmountZero.selector);
        berry.unshield(0, bob, js);
        vm.expectRevert(Berrycoin.InvalidRecipient.selector);
        berry.unshield(1, address(0), js);
        vm.expectRevert(Berrycoin.InvalidRecipient.selector);
        berry.unshield(1, address(berry), js);
    }

    // ------------------------------------------------------------------ rejecting verifier / replay
    function test_rejectingVerifierBlocksEveryShieldedEntrypoint() public {
        // put some value in the pool first (while proofs are accepted) so unshield reaches the proof check:
        // its supply-ceiling pre-check (A3) deliberately runs BEFORE the proof is even looked at.
        NoteTree.JoinSplit memory seed = _js();
        vm.prank(alice);
        berry.shield(10e18, seed);
        uint256 leaves = berry.nextLeafIndex();

        verifier.setAccept(false);
        NoteTree.JoinSplit memory js = _js();
        vm.prank(alice);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.shield(1e18, js);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.transferZK(js);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.unshield(1, bob, js);
        assertEq(berry.shieldedSupply(), 10e18);
        assertEq(berry.nextLeafIndex(), leaves, "failed proofs must not touch the tree");
        _assertConserved();
    }

    function test_nullifierReplayReverts() public {
        NoteTree.JoinSplit memory js = _js();
        berry.transferZK(js);
        vm.expectRevert(NoteTree.NullifierReuse.selector);
        berry.transferZK(js);
    }

    function test_duplicateNullifiersInOneJoinSplitRevert() public {
        NoteTree.JoinSplit memory js = _js();
        js.nullifiers[1] = js.nullifiers[0];
        vm.expectRevert(NoteTree.NullifierReuse.selector);
        berry.transferZK(js);
    }

    function testFuzz_unknownRootReverts(bytes32 root) public {
        NoteTree.JoinSplit memory js = _js();
        vm.assume(!berry.isKnownRoot(root));
        js.root = root;
        vm.expectRevert(NoteTree.UnknownRoot.selector);
        berry.transferZK(js);
    }

    function testFuzz_nonFieldElementsRevert(uint256 big) public {
        big = bound(big, FIELD, type(uint256).max);
        NoteTree.JoinSplit memory js = _js();
        js.commitments[0] = bytes32(big);
        vm.expectRevert(NoteTree.NotInField.selector);
        berry.transferZK(js);
        js = _js();
        js.nullifiers[1] = bytes32(big);
        vm.expectRevert(NoteTree.NotInField.selector);
        berry.transferZK(js);
    }

    function test_historicRootsRemainValidWithinWindow() public {
        bytes32 r0 = berry.currentRoot();
        for (uint256 i; i < 10; ++i) berry.transferZK(_js());
        assertTrue(berry.isKnownRoot(r0));
        NoteTree.JoinSplit memory js = _js();
        js.root = r0; // a proof built against an older (still known) root is accepted
        berry.transferZK(js);
    }
}
