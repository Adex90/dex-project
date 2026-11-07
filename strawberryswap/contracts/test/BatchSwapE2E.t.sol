// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {NoteTree} from "../src/privacy/NoteTree.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../src/interfaces/ITreasury.sol";
import {PoseidonHasherAdapter, IPoseidonT3, JoinSplitVerifierAdapter, IGroth16Verifier9} from "../src/verifiers/Groth16Adapters.sol";
import {GenericGroth16Adapter} from "../src/verifiers/GenericGroth16Adapter.sol";
import {JoinSplitGroth16Verifier} from "./fixtures/JoinSplitGroth16Verifier.dev.sol";
import {BatchSwapGroth16Verifier} from "./fixtures/BatchSwapGroth16Verifier.dev.sol";
import {PoseidonT3Code} from "./fixtures/PoseidonT3Code.sol";
import {BatchFixtures as F} from "./fixtures/BatchFixtures.sol";
import {MockERC20, MockOracle, MockTreasury} from "./mocks/Mocks.sol";

/// @notice THE closing-the-loop test for Module 2. No mocks anywhere on the privacy path:
///   users   : real JoinSplit proofs   deposit -> intent note (owned by the unspendable BATCH key)
///   Automator (Go): selects intents, computes the uniform clearing price, builds the witness and — through the
///             production prover wrapper — the real `batch_swap` Groth16 proof
///   chain   : real Poseidon tree, real verifiers, real DarkPool.settleBatch
///   users   : real JoinSplit proofs   withdraw the batch's output notes
/// and then checks that every user got exactly what the Go Automator computed, that K did not fall, and that the
/// pool is fully backed with ZERO custody left over.
contract BatchSwapE2ETest is Test {
    DarkPool pool;
    MockERC20 token0;
    MockERC20 token1;
    address lp = address(0x1111);
    MockOracle oracle;

    function setUp() public {
        MockERC20 a = new MockERC20("A");
        MockERC20 b = new MockERC20("B");
        (token0, token1) = address(a) < address(b) ? (a, b) : (b, a);

        bytes memory code = PoseidonT3Code.CODE;
        address poseidon;
        assembly {
            poseidon := create(0, add(code, 0x20), mload(code))
        }
        require(poseidon != address(0), "poseidon deploy failed");

        IZKVerifier joinSplit =
            new JoinSplitVerifierAdapter(IGroth16Verifier9(address(new JoinSplitGroth16Verifier())), 600_000);
        // batch_swap N=4 has 8 + 2·4 = 16 public inputs
        IZKVerifier batch = new GenericGroth16Adapter(address(new BatchSwapGroth16Verifier()), 16, 900_000);

        pool = new DarkPool(
            IERC20Min(address(token0)),
            IERC20Min(address(token1)),
            F.V0,
            F.V1,
            joinSplit,
            batch,
            IPoseidonHasher(address(new PoseidonHasherAdapter(IPoseidonT3(poseidon)))),
            IZKOracle(address(oracle = new MockOracle())),
            ITreasury(address(new MockTreasury())),
            4
        );
        vm.warp(F.T0);
        token0.mint(lp, F.R0);
        token1.mint(lp, F.R1);
        vm.startPrank(lp);
        token0.approve(address(pool), F.R0);
        token1.approve(address(pool), F.R1);
        pool.addLiquidity(F.R0, F.R1, lp);
        vm.stopPrank();
    }

    // ------------------------------------------------------------------ helpers
    function _js(F.Step memory s) internal pure returns (NoteTree.JoinSplit memory js) {
        js.proof = s.proof;
        js.root = s.root;
        js.nullifiers = [s.n0, s.n1];
        js.commitments = [s.c0, s.c1];
        js.encryptedNotes = [s.enc0, s.enc1];
    }

    function _token(uint256 assetId) internal view returns (MockERC20) {
        return assetId == 0 ? token0 : token1;
    }

    /// users deposit and move their notes into intent notes (8 real JoinSplit proofs)
    function _prepare() internal {
        F.Step[] memory deps = F.deposits();
        for (uint256 i; i < deps.length; ++i) {
            _token(deps[i].assetId).mint(deps[i].actor, deps[i].amount);
            vm.startPrank(deps[i].actor);
            _token(deps[i].assetId).approve(address(pool), deps[i].amount);
            pool.depositNote(deps[i].assetId, deps[i].amount, _js(deps[i]));
            vm.stopPrank();
        }
        F.Step[] memory xfers = F.transfers();
        for (uint256 i; i < xfers.length; ++i) {
            pool.transferNote(xfers[i].assetId, _js(xfers[i])); // any relayer may submit
        }
        assertEq(pool.currentRoot(), F.BATCH_ROOT, "on-chain tree differs from the root the batch was proven against");
    }

    function _batch() internal pure returns (DarkPool.Batch memory b) {
        b.proof = F.BATCH_PROOF;
        b.root = F.BATCH_ROOT;
        b.tNow = F.TNOW;
        b.nullifiers = F.batchNullifiers();
        b.commitments = F.batchCommitments();
        b.encryptedNotes = new bytes[](4);
        for (uint256 i; i < 4; ++i) b.encryptedNotes[i] = hex"aa"; // sealed blobs in production (see Go intent.Seal)
        (b.X, b.Y) = (F.R0 + F.V0, F.R1 + F.V1); // [audit H-5]: the state the Go Automator proved against
        b.in0 = F.NET_IN0;
        b.in1 = F.NET_IN1;
        b.out0 = F.NET_OUT0;
        b.out1 = F.NET_OUT1;
    }

    function _atSettlement() internal {
        vm.warp(uint256(F.TNOW) + 10); // tNow <= block.timestamp <= tNow + window
        oracle.set(2e18); // [audit C-1]: a fresh zk-oracle price (== launch spot) is required for value-moving batches
    }

    // ------------------------------------------------------------------ the full loop
    function test_endToEnd_privateBatchSettlement() public {
        _prepare();
        _atSettlement();
        uint256 kBefore = pool.invariantK();
        uint256 r0 = pool.reserve0();
        uint256 r1 = pool.reserve1();
        DarkPool.Batch memory b = _batch();

        uint256 g = gasleft();
        pool.settleBatch(b); // any relayer
        emit log_named_uint("gas: settleBatch (4 intents, real batch_swap proof)", g - gasleft());

        // aggregate economics: reserves moved by exactly the NET flow, and K did not fall
        assertEq(pool.reserve0(), r0 + F.NET_IN0 - F.NET_OUT0);
        assertEq(pool.reserve1(), r1 + F.NET_IN1 - F.NET_OUT1);
        assertGe(pool.invariantK(), kBefore, "K decreased");
        assertEq(F.NET_IN1, 0, "this batch is net token0-in");

        // users withdraw their output notes (4 real JoinSplit proofs)
        F.Step[] memory ws = F.withdraws();
        for (uint256 i; i < ws.length; ++i) {
            // withdraw i corresponds to batch slot i
            assertEq(ws[i].actor, F.expectedActor(i));
            assertEq(ws[i].assetId, F.expectedAsset(i));
            assertEq(ws[i].amount, F.expectedValue(i));
            uint256 before = _token(ws[i].assetId).balanceOf(ws[i].actor);
            pool.withdrawNote(ws[i].assetId, ws[i].amount, ws[i].actor, _js(ws[i]));
            assertEq(_token(ws[i].assetId).balanceOf(ws[i].actor) - before, F.expectedValue(i), "user received a different amount than the Automator computed");
        }

        // 100 % reserve: nothing left in custody, real balances == curve reserves exactly
        assertEq(pool.custody0(), 0, "custody0 not fully distributed");
        assertEq(pool.custody1(), 0, "custody1 not fully distributed");
        assertEq(token0.balanceOf(address(pool)), pool.reserve0());
        assertEq(token1.balanceOf(address(pool)), pool.reserve1());
        // every swapper received the OTHER asset; the expired intent got its own asset back in full
        for (uint256 i; i < 4; ++i) {
            if (F.isRefund(i)) {
                assertEq(F.expectedValue(i), 77e18, "refund must return the full deposit");
            }
        }
    }

    /// Where does the settleBatch gas go? (documented in docs/; asserted loosely so a regression is noticed)
    function test_gasBreakdown() public {
        _prepare();
        _atSettlement();
        DarkPool.Batch memory b = _batch();
        uint256[] memory pub = new uint256[](16);
        pub[0] = uint256(b.root);
        pub[1] = b.tNow;
        pub[2] = pool.reserve0() + F.V0;
        pub[3] = pool.reserve1() + F.V1;
        pub[4] = b.in0; pub[5] = b.in1; pub[6] = b.out0; pub[7] = b.out1;
        for (uint256 i; i < 4; ++i) {
            pub[8 + i] = uint256(b.nullifiers[i]);
            pub[12 + i] = uint256(b.commitments[i]);
        }
        IZKVerifier v = pool.batchVerifier();
        uint256 g = gasleft();
        assertTrue(v.verifyProof(b.proof, pub));
        uint256 verifyGas = g - gasleft();
        emit log_named_uint("gas: batch SNARK verification (16 public inputs)", verifyGas);

        g = gasleft();
        pool.settleBatch(b);
        uint256 total = g - gasleft();
        emit log_named_uint("gas: settleBatch total", total);
        emit log_named_uint("gas: everything else (4 nullifiers + 4 Merkle insertions + accounting)", total - verifyGas);
        assertLt(verifyGas, 900_000, "verifier exceeds its gas cap");
    }

    /// Uniform clearing price: every token0-seller got the same rate, both within the circuit's rounding.
    function test_everyUserGotTheSameUniformPrice() public pure {
        // A sells 1000 token0, C sells 250 token0 => payouts must be proportional (floor(value * pLo / 1e18))
        uint256 pLo = F.PRICE_LO;
        uint256 a;
        uint256 c;
        for (uint256 i; i < 4; ++i) {
            if (F.isRefund(i) || F.expectedAsset(i) != 1) continue;
            if (F.expectedActor(i) == address(0x000000000000000000000000000000000000a001)) a = F.expectedValue(i);
            if (F.expectedActor(i) == address(0x000000000000000000000000000000000000C003)) c = F.expectedValue(i);
        }
        assertEq(a, 1000e18 * pLo / 1e18);
        assertEq(c, 250e18 * pLo / 1e18);
    }

    // ------------------------------------------------------------------ attacks on the batch (real proof, mutated)
    function test_aggregatorCannotInflateNetFlow() public {
        _prepare();
        _atSettlement();
        DarkPool.Batch memory b = _batch();
        b.in0 += 1; // claim the pool received 1 wei more than proven
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.settleBatch(b);
        b = _batch();
        b.out1 -= 1; // ... or paid out 1 wei less
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.settleBatch(b);
    }

    function test_aggregatorCannotRedirectAnOutputNote() public {
        _prepare();
        _atSettlement();
        DarkPool.Batch memory b = _batch();
        b.commitments[0] = bytes32(uint256(b.commitments[0]) + 1); // steal user 0's payout note
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.settleBatch(b);
    }

    function test_aggregatorCannotReorderOrSwapSlots() public {
        _prepare();
        _atSettlement();
        DarkPool.Batch memory b = _batch();
        (b.nullifiers[0], b.nullifiers[1]) = (b.nullifiers[1], b.nullifiers[0]);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.settleBatch(b);
    }

    function test_aggregatorCannotDropAnIntentAndKeepTheRest() public {
        _prepare();
        _atSettlement();
        DarkPool.Batch memory b = _batch();
        b.nullifiers = new bytes32[](3);
        b.commitments = new bytes32[](3);
        b.encryptedNotes = new bytes[](3);
        for (uint256 i; i < 3; ++i) {
            b.nullifiers[i] = F.batchNullifiers()[i];
            b.commitments[i] = F.batchCommitments()[i];
            b.encryptedNotes[i] = hex"aa";
        }
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.settleBatch(b);
    }

    function test_proofIsBoundToPoolState() public {
        _prepare();
        // an LP deposits between proving and submission: X, Y change by even 1 wei => the proof is void
        token0.mint(lp, 2e18);
        token1.mint(lp, 4e18);
        vm.startPrank(lp);
        token0.approve(address(pool), 2e18);
        token1.approve(address(pool), 4e18);
        pool.addLiquidity(2e18, 4e18, lp);
        vm.stopPrank();
        _atSettlement();
        DarkPool.Batch memory b = _batch();
        // [audit H-5]: the proof stays valid for STATE_WINDOW_BLOCKS after the state changed ...
        uint256 snap = vm.snapshotState();
        pool.settleBatch(b);
        vm.revertToState(snap);
        // ... and expires afterwards
        vm.roll(block.number + pool.STATE_WINDOW_BLOCKS() + 1);
        vm.expectRevert(DarkPool.StaleState.selector);
        pool.settleBatch(b);
    }

    function test_batchCannotBeReplayed() public {
        _prepare();
        _atSettlement();
        pool.settleBatch(_batch());
        DarkPool.Batch memory b = _batch();
        vm.expectRevert(); // nullifiers spent (and the pool state moved)
        pool.settleBatch(b);
    }

    function test_clockWindow() public {
        _prepare();
        DarkPool.Batch memory b = _batch();
        vm.warp(uint256(F.TNOW) - 1); // proof claims a clock in the future
        vm.expectRevert(DarkPool.StaleBatchClock.selector);
        pool.settleBatch(b);
        vm.warp(uint256(F.TNOW) + pool.BATCH_CLOCK_WINDOW() + 1); // too old: deadlines could be stale
        vm.expectRevert(DarkPool.StaleBatchClock.selector);
        pool.settleBatch(b);
    }

    /// Flip any single bit of the batch proof: it must stop verifying (and costs a bounded amount of gas).
    function testFuzz_tamperedBatchProofRejected(uint256 pos, uint8 mask) public {
        _prepare();
        _atSettlement();
        mask = mask == 0 ? 1 : mask;
        DarkPool.Batch memory b = _batch();
        b.proof[pos % b.proof.length] ^= bytes1(mask);
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.settleBatch(b);
    }

    /// Withdrawal proofs are bound to the recipient: a front-runner cannot redirect a user's payout.
    function test_payoutCannotBeRedirected() public {
        _prepare();
        _atSettlement();
        pool.settleBatch(_batch());
        F.Step[] memory ws = F.withdraws();
        vm.expectRevert(NoteTree.InvalidProof.selector);
        pool.withdrawNote(ws[0].assetId, ws[0].amount, address(0xBAD), _js(ws[0]));
    }
}
