// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DarkPool} from "../../src/dex/DarkPool.sol";
import {NoteTree} from "../../src/privacy/NoteTree.sol";
import {IERC20Min} from "../../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../../src/interfaces/ITreasury.sol";
import {PoseidonHasherAdapter, IPoseidonT3, JoinSplitVerifierAdapter, IGroth16Verifier9} from "../../src/verifiers/Groth16Adapters.sol";
import {GenericGroth16Adapter} from "../../src/verifiers/GenericGroth16Adapter.sol";
import {JoinSplitGroth16Verifier} from "../fixtures/JoinSplitGroth16Verifier.dev.sol";
import {BatchSwapGroth16Verifier} from "../fixtures/BatchSwapGroth16Verifier.dev.sol";
import {PoseidonT3Code} from "../fixtures/PoseidonT3Code.sol";
import {BatchFixtures as F} from "../fixtures/BatchFixtures.sol";
import {MockERC20, MockOracle, MockTreasury} from "../mocks/Mocks.sol";

/// @notice Phase-8 regressions with NO mocks on the proof path: real JoinSplit + batch_swap Groth16 proofs (dev keys),
///         real Poseidon, real DarkPool. The batch is the Go Automator's (circuits/scripts/gen-batch-fixtures.mjs).
contract RealProofRegressionTest is Test {
    DarkPool pool;
    MockERC20 token0;
    MockERC20 token1;
    MockOracle oracle;
    MockTreasury treasury;
    address lp = address(0x1111);

    function setUp() public {
        MockERC20 a = new MockERC20("A");
        MockERC20 b = new MockERC20("B");
        (token0, token1) = address(a) < address(b) ? (a, b) : (b, a);
        bytes memory code = PoseidonT3Code.CODE;
        address poseidon;
        assembly {
            poseidon := create(0, add(code, 0x20), mload(code))
        }
        oracle = new MockOracle();
        treasury = new MockTreasury();
        pool = new DarkPool(
            IERC20Min(address(token0)), IERC20Min(address(token1)), F.V0, F.V1,
            new JoinSplitVerifierAdapter(IGroth16Verifier9(address(new JoinSplitGroth16Verifier())), 600_000),
            new GenericGroth16Adapter(address(new BatchSwapGroth16Verifier()), 16, 900_000),
            IPoseidonHasher(address(new PoseidonHasherAdapter(IPoseidonT3(poseidon)))),
            IZKOracle(address(oracle)), ITreasury(address(treasury)), 4
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

    function _js(F.Step memory s) internal pure returns (NoteTree.JoinSplit memory js) {
        js.proof = s.proof;
        js.root = s.root;
        js.nullifiers = [s.n0, s.n1];
        js.commitments = [s.c0, s.c1];
        js.encryptedNotes = [s.enc0, s.enc1];
    }

    function _prepare() internal {
        F.Step[] memory deps = F.deposits();
        for (uint256 i; i < deps.length; ++i) {
            MockERC20 t = deps[i].assetId == 0 ? token0 : token1;
            t.mint(deps[i].actor, deps[i].amount);
            vm.startPrank(deps[i].actor);
            t.approve(address(pool), deps[i].amount);
            pool.depositNote(deps[i].assetId, deps[i].amount, _js(deps[i]));
            vm.stopPrank();
        }
        F.Step[] memory xs = F.transfers();
        for (uint256 i; i < xs.length; ++i) pool.transferNote(xs[i].assetId, _js(xs[i]));
        vm.warp(uint256(F.TNOW) + 10);
    }

    function _batch() internal pure returns (DarkPool.Batch memory b) {
        b.proof = F.BATCH_PROOF;
        b.root = F.BATCH_ROOT;
        b.tNow = F.TNOW;
        b.nullifiers = F.batchNullifiers();
        b.commitments = F.batchCommitments();
        b.encryptedNotes = new bytes[](4);
        for (uint256 i; i < 4; ++i) b.encryptedNotes[i] = hex"aa";
        (b.in0, b.in1, b.out0, b.out1) = (F.NET_IN0, F.NET_IN1, F.NET_OUT0, F.NET_OUT1);
        (b.X, b.Y) = (F.R0 + F.V0, F.R1 + F.V1);
    }

    /// C-1: a value-moving batch needs a fresh oracle (it used to trade at the curve with a dead oracle) ...
    function test_C1_real_valueMovingBatchNeedsTheOracle() public {
        _prepare();
        vm.expectRevert(DarkPool.StaleOracle.selector);
        pool.settleBatch(_batch());
    }

    /// ... and with the oracle at the pool's price the users' batch (net token0 in, at the curve) settles.
    function test_C1_real_retailBatchAtTheOraclePriceSettles() public {
        _prepare();
        oracle.set(2e18);
        pool.settleBatch(_batch());
        assertEq(pool.reserve0(), F.R0 + F.NET_IN0);
    }

    /// H-5: the 74 k-gas grief that used to void the real proof (and with it the refund slot) is tolerated now.
    function test_H5_real_tinyReserveChangeNoLongerVoidsTheProof() public {
        _prepare();
        vm.prank(lp);
        pool.removeLiquidity(1e6, lp); // moves reserves by ~1 wei: below the batch's rounding slack
        oracle.set(2e18);
        pool.settleBatch(_batch());
        bool refunded;
        for (uint256 i; i < 4; ++i) refunded = refunded || F.isRefund(i);
        assertTrue(refunded, "the batch carried (and settled) a refund slot");
    }
}
