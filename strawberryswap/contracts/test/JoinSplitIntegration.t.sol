// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {Berrycoin} from "../src/tokens/Berrycoin.sol";
import {NoteTree} from "../src/privacy/NoteTree.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";
import {JoinSplitVerifierAdapter, IGroth16Verifier9, PoseidonHasherAdapter, IPoseidonT3} from "../src/verifiers/Groth16Adapters.sol";
import {JoinSplitGroth16Verifier} from "./fixtures/JoinSplitGroth16Verifier.dev.sol";
import {PoseidonT3Code} from "./fixtures/PoseidonT3Code.sol";
import {Fixtures} from "./fixtures/Fixtures.sol";

/// @notice The "no mocks" suite: genuine Groth16 proofs, produced by circuits/joinsplit, verified on-chain by
///         the snarkjs verifier and a Poseidon tree whose bytecode comes from circomlibjs. Shows that
///           (1) circuit and contract agree on hashing, tree layout and public-input order;
///           (2) proofs are bound to caller / recipient / contents; replays and tampering fail;
///           (3) supply is conserved through a real shield → private transfer → unshield cycle.
contract JoinSplitIntegrationTest is Test {
    uint256 constant GAS_CAP = 600_000;
    Berrycoin berry;
    address adapterAddr;
    address genesis = address(0xBEEF);

    function setUp() public {
        bytes memory code = PoseidonT3Code.CODE;
        address poseidon;
        assembly {
            poseidon := create(0, add(code, 0x20), mload(code))
        }
        require(poseidon != address(0), "poseidon deploy failed");
        PoseidonHasherAdapter hasher = new PoseidonHasherAdapter(IPoseidonT3(poseidon));
        JoinSplitGroth16Verifier raw = new JoinSplitGroth16Verifier();
        JoinSplitVerifierAdapter adapter = new JoinSplitVerifierAdapter(IGroth16Verifier9(address(raw)), GAS_CAP);
        adapterAddr = address(adapter);
        berry = new Berrycoin(genesis, IZKVerifier(address(adapter)), IPoseidonHasher(address(hasher)));
        vm.prank(genesis);
        berry.transfer(Fixtures.ALICE, 1_000e18);
    }

    function _js(Fixtures.Case memory c) internal pure returns (NoteTree.JoinSplit memory js) {
        js.proof = c.proof;
        js.root = c.root;
        js.nullifiers = [c.n0, c.n1];
        js.commitments = [c.c0, c.c1];
        js.encryptedNotes = [c.enc0, c.enc1];
    }

    function _conserved() internal view {
        uint256 sum = berry.balanceOf(genesis) + berry.balanceOf(Fixtures.ALICE) + berry.balanceOf(Fixtures.RECIPIENT)
            + berry.balanceOf(address(this));
        assertEq(sum + berry.shieldedSupply(), berry.MAX_SUPPLY());
    }

    function _shield() internal {
        Fixtures.Case memory s = Fixtures.shield();
        NoteTree.JoinSplit memory js = _js(s);
        vm.prank(Fixtures.ALICE);
        berry.shield(s.amount, js);
    }

    // ------------------------------------------------------------------ (1) the full cycle with real proofs
    function test_realProofs_shield_privateTransfer_unshield() public {
        // the empty-tree root computed in JS (circomlibjs Poseidon) is the root the shield proof was made for
        assertTrue(berry.isKnownRoot(Fixtures.shield().root), "on-chain Poseidon tree != circuit tree");

        _shield();
        assertEq(berry.shieldedSupply(), 100e18);
        assertEq(berry.balanceOf(Fixtures.ALICE), 900e18);
        _conserved();

        // after the shield's two insertions the root must equal the one the NEXT proof was built against
        assertEq(berry.currentRoot(), Fixtures.transfer().root, "tree layout / Poseidon mismatch after insertions");

        // private transfer Alice -> Bob (60) + change (40): relayed by anyone; moves no public value
        berry.transferZK(_js(Fixtures.transfer()));
        assertEq(berry.shieldedSupply(), 100e18);
        assertEq(berry.currentRoot(), Fixtures.unshield().root);
        _conserved();

        // Bob exits 60 to a fresh address
        Fixtures.Case memory u = Fixtures.unshield();
        berry.unshield(u.amount, Fixtures.RECIPIENT, _js(u));
        assertEq(berry.balanceOf(Fixtures.RECIPIENT), 60e18);
        assertEq(berry.shieldedSupply(), 40e18, "Alice's 40 change note is still shielded");
        _conserved();
    }

    // ------------------------------------------------------------------ (2) binding, replay, tampering
    function test_replayedProofReverts() public {
        _shield();
        Fixtures.Case memory s = Fixtures.shield();
        NoteTree.JoinSplit memory js = _js(s);
        vm.prank(Fixtures.ALICE);
        vm.expectRevert(NoteTree.NullifierReuse.selector);
        berry.shield(s.amount, js);
    }

    function test_shieldProofIsBoundToTheCaller() public {
        Fixtures.Case memory s = Fixtures.shield();
        NoteTree.JoinSplit memory js = _js(s);
        // a mempool observer copies Alice's proof and submits it as themselves: extDataHash differs
        vm.deal(address(this), 1 ether);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.shield(s.amount, js);
    }

    function test_unshieldProofIsBoundToTheRecipient() public {
        _shield();
        berry.transferZK(_js(Fixtures.transfer()));
        Fixtures.Case memory u = Fixtures.unshield();
        NoteTree.JoinSplit memory js = _js(u);
        vm.expectRevert(NoteTree.InvalidProof.selector); // front-runner tries to redirect the funds
        berry.unshield(u.amount, address(0xBAD), js);
    }

    function test_publicAmountIsBoundToTheProof() public {
        _shield();
        berry.transferZK(_js(Fixtures.transfer()));
        Fixtures.Case memory u = Fixtures.unshield();
        NoteTree.JoinSplit memory js = _js(u);
        vm.expectRevert(NoteTree.InvalidProof.selector); // proof is for 60; claiming 61 must fail
        berry.unshield(u.amount + 1, Fixtures.RECIPIENT, js);
        vm.expectRevert(NoteTree.InvalidProof.selector); // and claiming less is just as invalid
        berry.unshield(u.amount - 1, Fixtures.RECIPIENT, js);
    }

    function test_shieldAmountIsBoundToTheProof() public {
        Fixtures.Case memory s = Fixtures.shield();
        NoteTree.JoinSplit memory js = _js(s);
        vm.prank(Fixtures.ALICE);
        vm.expectRevert(NoteTree.InvalidProof.selector); // proven: 100. Claimed: 101 (inflation attempt)
        berry.shield(s.amount + 1, js);
    }

    function test_tamperedOutputCommitmentRejected() public {
        _shield();
        NoteTree.JoinSplit memory js = _js(Fixtures.transfer());
        js.commitments[0] = bytes32(uint256(js.commitments[0]) + 1);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.transferZK(js);
    }

    function test_tamperedNullifierRejected() public {
        _shield();
        NoteTree.JoinSplit memory js = _js(Fixtures.transfer());
        js.nullifiers[0] = bytes32(uint256(js.nullifiers[0]) + 1); // would let the same note be spent twice
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.transferZK(js);
    }

    function test_ciphertextSwapRejected() public {
        _shield();
        NoteTree.JoinSplit memory js = _js(Fixtures.transfer());
        js.encryptedNotes[0] = hex"dead"; // relayer swaps the ciphertext: bound through extDataHash
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.transferZK(js);
    }

    function test_proofCannotBeUsedBeforeItsRootExists() public {
        vm.expectRevert(NoteTree.UnknownRoot.selector);
        berry.transferZK(_js(Fixtures.transfer()));
    }

    /// Flip any single bit of any byte of a genuine proof: it must stop verifying.
    function testFuzz_anyTamperedProofByteRejected(uint256 pos, uint8 mask) public {
        mask = mask == 0 ? 1 : mask;
        Fixtures.Case memory s = Fixtures.shield();
        NoteTree.JoinSplit memory js = _js(s);
        js.proof[pos % js.proof.length] ^= bytes1(mask);
        vm.prank(Fixtures.ALICE);
        vm.expectRevert(); // InvalidProof (or a decoding failure inside the verifier): never success
        berry.shield(s.amount, js);
    }

    /// A malformed proof must not be able to burn more than the verifier gas cap, however much gas the caller
    /// supplies (regression test for the all-gas-consumed behaviour of failed EC precompiles).
    function testFuzz_malformedProofCannotBurnMoreThanGasCap(bytes32 a, bytes32 b) public {
        bytes memory junk = abi.encodePacked(a, b, a, b, a, b, a, b); // 256 bytes, almost surely off-curve
        uint256[] memory pub = new uint256[](9);
        uint256 g = gasleft();
        bool ok = IZKVerifier(adapterAddr).verifyProof(junk, pub);
        uint256 used = g - gasleft();
        assertFalse(ok);
        assertLt(used, GAS_CAP + 100_000, "verifier gas not bounded");
    }

    function test_validProofCostFitsInsideTheGasCap() public {
        Fixtures.Case memory s = Fixtures.shield();
        uint256[] memory pub = new uint256[](9);
        // reconstruct the public inputs the contract would pass for this shield (root,n0,n1,c0,c1,in,out,ext,asset)
        pub[0] = uint256(s.root); pub[1] = uint256(s.n0); pub[2] = uint256(s.n1);
        pub[3] = uint256(s.c0); pub[4] = uint256(s.c1); pub[5] = s.amount; pub[6] = 0;
        pub[7] = uint256(keccak256(abi.encode(Fixtures.ALICE, s.enc0, s.enc1))) % 21888242871839275222246405745257275088548364400416034343698204186575808495617;
        pub[8] = 0;
        uint256 g = gasleft();
        assertTrue(IZKVerifier(adapterAddr).verifyProof(s.proof, pub));
        emit log_named_uint("gas for one genuine JoinSplit verification", g - gasleft());
        assertLt(g - gasleft(), GAS_CAP, "cap too tight for a valid proof");
    }

    // ------------------------------------------------------------------ (3) forged proofs vs the REAL verifier
    function testFuzz_randomProofBytesNeverVerify(bytes32 a, bytes32 b, bytes32 c, bytes32 d) public {
        bytes memory junk = abi.encodePacked(a, b, c, d, a ^ b, c ^ d, b ^ c, a ^ d);
        assertEq(junk.length, 256);
        NoteTree.JoinSplit memory js = _js(Fixtures.shield());
        js.proof = junk;
        vm.prank(Fixtures.ALICE);
        vm.expectRevert();
        berry.shield(100e18, js);
    }

    function testFuzz_wrongLengthProofRejected(bytes calldata junk) public {
        vm.assume(junk.length != 256);
        NoteTree.JoinSplit memory js = _js(Fixtures.shield());
        js.proof = junk;
        vm.prank(Fixtures.ALICE);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        berry.shield(100e18, js);
    }

    /// The headline attack: an adversary with NO valid proof tries to unshield. Real verifier + supply cap.
    function testFuzz_forgedUnshieldAlwaysFails(uint256 amount, bytes32 seed) public {
        _shield(); // 100 shielded: gives the attacker something to aim at
        amount = bound(amount, 1, type(uint256).max);
        NoteTree.JoinSplit memory js = _js(Fixtures.unshield());
        js.nullifiers = [bytes32(uint256(seed) % 1e70), bytes32(uint256(seed) % 1e60)];
        js.root = berry.currentRoot();
        address attacker = address(0xBAD);
        vm.prank(attacker);
        vm.expectRevert(); // ShieldedSupplyExceeded for big amounts, InvalidProof otherwise
        berry.unshield(amount, attacker, js);
        assertEq(berry.balanceOf(attacker), 0);
        assertEq(berry.shieldedSupply(), 100e18);
        _conserved();
    }

    function test_unshieldWithNothingShieldedIsBlockedByTheSupplyCeiling() public {
        Fixtures.Case memory u = Fixtures.unshield();
        NoteTree.JoinSplit memory js = _js(u);
        vm.expectRevert(Berrycoin.ShieldedSupplyExceeded.selector);
        berry.unshield(u.amount, Fixtures.RECIPIENT, js);
    }
}
