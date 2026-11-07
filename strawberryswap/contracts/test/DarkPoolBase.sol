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
import {Math} from "../src/utils/Math.sol";
import {MockVerifier, MockHasher, MockERC20, MockOracle, MockTreasury, MockSearcher} from "./mocks/Mocks.sol";

/// @dev Shared fixture: a DarkPool wired to mock proof systems (accept-all by default: the pool's own
///      economic checks, not the SNARK, are what these suites exercise).
abstract contract DarkPoolBase is Test {
    uint256 constant FIELD = 21888242871839275222246405745257275088548364400416034343698204186575808495617;
    uint256 constant WAD = 1e18;
    uint256 constant V0 = 1_000_000e18;
    uint256 constant V1 = 2_000_000e18;
    uint256 constant BATCH_SIZE = 16; // mock-verifier suites; the real-proof suite uses the circuit's N

    MockERC20 token0;
    MockERC20 token1;
    MockOracle oracle;
    MockTreasury treasury;
    MockVerifier joinSplitV;
    MockVerifier batchV;
    MockHasher hasher;
    DarkPool pool;
    address lp = address(0x1111);
    address trader = address(0x2222);
    uint256 nonce;

    function _deploy() internal {
        MockERC20 a = new MockERC20("A");
        MockERC20 b = new MockERC20("B");
        (token0, token1) = address(a) < address(b) ? (a, b) : (b, a); // pool requires token0 < token1
        oracle = new MockOracle();
        treasury = new MockTreasury();
        joinSplitV = new MockVerifier(true);
        batchV = new MockVerifier(true);
        hasher = new MockHasher();
        pool = new DarkPool(
            IERC20Min(address(token0)),
            IERC20Min(address(token1)),
            V0,
            V1,
            IZKVerifier(address(joinSplitV)),
            IZKVerifier(address(batchV)),
            IPoseidonHasher(address(hasher)),
            IZKOracle(address(oracle)),
            ITreasury(address(treasury)),
            BATCH_SIZE
        );
    }

    function _fresh() internal returns (bytes32) {
        return bytes32(uint256(keccak256(abi.encode("fresh", ++nonce))) % FIELD);
    }

    function _js() internal returns (NoteTree.JoinSplit memory js) {
        js.proof = hex"01";
        js.root = pool.currentRoot();
        js.nullifiers = [_fresh(), _fresh()];
        js.commitments = [_fresh(), _fresh()];
        js.encryptedNotes = [bytes(""), bytes("")];
    }

    function _addLiquidity(address who, uint256 a0, uint256 a1) internal returns (uint256 minted) {
        token0.mint(who, a0);
        token1.mint(who, a1);
        vm.startPrank(who);
        token0.approve(address(pool), a0);
        token1.approve(address(pool), a1);
        minted = pool.addLiquidity(a0, a1, who);
        vm.stopPrank();
    }

    /// @dev puts `amount` of asset into note custody (what users hold privately and swap with).
    function _custody(uint256 assetId, uint256 amount) internal {
        MockERC20 t = assetId == 0 ? token0 : token1;
        t.mint(trader, amount);
        NoteTree.JoinSplit memory js = _js();
        vm.startPrank(trader);
        t.approve(address(pool), amount);
        pool.depositNote(assetId, amount, js);
        vm.stopPrank();
    }

    function _batch(uint256 in0, uint256 in1, uint256 out0, uint256 out1) internal returns (DarkPool.Batch memory b) {
        b.proof = hex"01";
        b.root = pool.currentRoot();
        b.tNow = uint64(block.timestamp);
        b.nullifiers = new bytes32[](1);
        b.nullifiers[0] = _fresh();
        b.commitments = new bytes32[](1);
        b.commitments[0] = _fresh();
        b.encryptedNotes = new bytes[](1);
        b.encryptedNotes[0] = "";
        (b.in0, b.in1, b.out0, b.out1) = (in0, in1, out0, out1);
    }

    function _k() internal view returns (uint256) {
        return pool.invariantK();
    }
}
