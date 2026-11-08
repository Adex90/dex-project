// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DarkPool} from "../../src/dex/DarkPool.sol";
import {NoteTree} from "../../src/privacy/NoteTree.sol";
import {Math} from "../../src/utils/Math.sol";
import {IERC20Min} from "../../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../../src/interfaces/ITreasury.sol";
import {MockERC20, MockSearcher, MockVerifier, MockHasher, MockOracle, MockTreasury} from "../mocks/Mocks.sol";

/// @dev Harness for the Phase-8 regression suites (docs/AUDIT_PHASE8.md). Proof systems are mocked (accept-all) ONLY
///      for statements an honest prover can produce: batch flows use the circuit's own clearing rule
///      (pLo = ⌊WAD·(Y+A1)/(X+A0)⌋, see circuits/test/batch.lib.mjs) and the proving keys are public, so each attack
///      step is something a real attacker can prove. RealProofRegression repeats the critical ones with real proofs.
abstract contract SecurityBase is Test {
    uint256 constant FIELD = 21888242871839275222246405745257275088548364400416034343698204186575808495617;
    uint256 constant WAD = 1e18;
    uint256 constant N = 16;

    MockERC20 t0;
    MockERC20 t1;
    MockOracle oracle;
    MockTreasury treasury;
    MockVerifier acceptAll;
    MockHasher hasher;
    uint256 nonce;

    address lp = address(0x1111);
    address victim = address(0x2222);
    address attacker = address(0xA77AC);

    function _tokens() internal {
        MockERC20 a = new MockERC20("A");
        MockERC20 b = new MockERC20("B");
        (t0, t1) = address(a) < address(b) ? (a, b) : (b, a);
        oracle = new MockOracle();
        treasury = new MockTreasury();
        acceptAll = new MockVerifier(true);
        hasher = new MockHasher();
    }

    function _pool(MockERC20 x, MockERC20 y, uint256 v0, uint256 v1) internal returns (DarkPool) {
        return new DarkPool(
            IERC20Min(address(x)), IERC20Min(address(y)), v0, v1,
            IZKVerifier(address(acceptAll)), IZKVerifier(address(acceptAll)), IPoseidonHasher(address(hasher)),
            IZKOracle(address(oracle)), ITreasury(address(treasury)), N
        );
    }

    function _fresh() internal returns (bytes32) {
        return bytes32(uint256(keccak256(abi.encode("security", ++nonce))) % FIELD);
    }

    function _js(DarkPool p) internal returns (NoteTree.JoinSplit memory js) {
        js.proof = hex"01";
        js.root = p.currentRoot();
        js.nullifiers = [_fresh(), _fresh()];
        js.commitments = [_fresh(), _fresh()];
        js.encryptedNotes = [bytes(""), bytes("")];
    }

    function _addLiq(DarkPool p, address who, uint256 a0, uint256 a1) internal returns (uint256 minted) {
        MockERC20 x = MockERC20(address(p.token0()));
        MockERC20 y = MockERC20(address(p.token1()));
        x.mint(who, a0);
        y.mint(who, a1);
        vm.startPrank(who);
        x.approve(address(p), a0);
        y.approve(address(p), a1);
        minted = p.addLiquidity(a0, a1, who);
        vm.stopPrank();
    }

    // ---- the batch_swap clearing rule for a ONE-intent batch
    function _quoteSell0(DarkPool p, uint256 a0) internal view returns (uint256) {
        (uint256 x, uint256 y) = p.effectiveReserves();
        return a0 * (WAD * y / (x + a0)) / WAD;
    }

    function _quoteSell1(DarkPool p, uint256 a1) internal view returns (uint256) {
        (uint256 x, uint256 y) = p.effectiveReserves();
        return a1 * WAD / (WAD * (y + a1) / x + 1);
    }

    function _oneIntentBatch(DarkPool p, bool sell0, uint256 amountIn, uint256 out) internal returns (DarkPool.Batch memory b) {
        b.proof = hex"01";
        b.root = p.currentRoot();
        b.tNow = uint64(block.timestamp);
        b.nullifiers = new bytes32[](1);
        b.nullifiers[0] = _fresh();
        b.commitments = new bytes32[](1);
        b.commitments[0] = _fresh();
        b.encryptedNotes = new bytes[](1);
        (b.in0, b.in1, b.out0, b.out1) = sell0 ? (amountIn, uint256(0), uint256(0), out) : (uint256(0), amountIn, out, uint256(0));
        (b.X, b.Y) = p.effectiveReserves();
    }

    /// The full private-swap path of `who`: depositNote -> transferNote (intent) -> settleBatch -> withdrawNote.
    /// Reverts (from settleBatch) bubble up unchanged.
    function _swapViaBatch(DarkPool p, address who, bool sell0, uint256 amountIn) internal returns (uint256 out) {
        MockERC20 tin = MockERC20(address(sell0 ? p.token0() : p.token1()));
        out = sell0 ? _quoteSell0(p, amountIn) : _quoteSell1(p, amountIn);
        tin.mint(who, amountIn);
        vm.startPrank(who);
        tin.approve(address(p), amountIn);
        p.depositNote(sell0 ? 0 : 1, amountIn, _js(p));
        p.transferNote(sell0 ? 0 : 1, _js(p));
        p.settleBatch(_oneIntentBatch(p, sell0, amountIn, out));
        p.withdrawNote(sell0 ? 1 : 0, out, who, _js(p));
        vm.stopPrank();
    }

    /// Amount of token0 to sell into the pool so that its spot reaches P (pool overprices token0).
    function _sell0ToReach(DarkPool p, uint256 P) internal view returns (uint256) {
        (uint256 x, uint256 y) = p.effectiveReserves();
        return Math.sqrt(x * y * WAD / P) - x;
    }

    function _valueAt(int256 d0, int256 d1, uint256 P) internal pure returns (int256) {
        return d0 * int256(P) / int256(WAD) + d1;
    }
}
