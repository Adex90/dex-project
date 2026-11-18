// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {EngineLens, IDarkPoolLens} from "../src/periphery/EngineLens.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../src/interfaces/ITreasury.sol";
import {MockVerifier, MockHasher, MockERC20, MockOracle, MockTreasury} from "./mocks/Mocks.sol";

/// @notice The engine's one-call snapshot (src/periphery/EngineLens.sol) is exactly what the separate views return.
contract EngineLensTest is Test {
    struct Snapshot {
        uint256 blockNumber;
        uint256 timestamp;
        uint256 baseFee;
        uint256 price;
        uint256 updatedAt;
        uint256 reserve0;
        uint256 reserve1;
        uint256 virtual0;
        uint256 virtual1;
        uint256 balance0;
        uint256 balance1;
    }

    DarkPool internal pool;
    MockOracle internal oracle;
    MockERC20 internal t0;
    MockERC20 internal t1;
    address internal constant EXECUTOR = address(0xE1);

    function setUp() public {
        vm.warp(1_767_225_600);
        vm.roll(4_242);
        oracle = new MockOracle();
        oracle.set(2e18);
        MockERC20 a = new MockERC20("WETH");
        MockERC20 b = new MockERC20("USD");
        (t0, t1) = address(a) < address(b) ? (a, b) : (b, a);
        pool = new DarkPool(
            IERC20Min(address(t0)),
            IERC20Min(address(t1)),
            2_000_000e18,
            4_000_000e18,
            IZKVerifier(address(new MockVerifier(true))),
            IZKVerifier(address(new MockVerifier(true))),
            IPoseidonHasher(address(new MockHasher())),
            IZKOracle(address(oracle)),
            ITreasury(address(new MockTreasury())),
            4
        );
        t0.mint(address(this), 1_000_000e18);
        t1.mint(address(this), 2_000_000e18);
        t0.approve(address(pool), type(uint256).max);
        t1.approve(address(pool), type(uint256).max);
        pool.addLiquidity(1_000_000e18, 2_000_000e18, address(this));
        t0.mint(EXECUTOR, 20_000e18);
        t1.mint(EXECUTOR, 40_123e18);
    }

    /// @dev What the engine gets from its eth_call: the creation's returned bytes, here the "code" `new` leaves behind.
    function _snapshot() internal returns (bytes memory) {
        return address(new EngineLens(IDarkPoolLens(address(pool)), EXECUTOR)).code;
    }

    function test_theSnapshotIsEveryViewAtOneBlock() public {
        vm.fee(7 gwei);
        bytes memory raw = _snapshot();
        assertEq(raw.length, 11 * 32, "eleven words");
        assertEq(uint8(raw[0]), 0, "starts with a zero byte: never 0xEF, so valid as returned code");
        Snapshot memory s = abi.decode(raw, (Snapshot));

        assertEq(s.blockNumber, 4_242);
        assertEq(s.timestamp, 1_767_225_600);
        assertEq(s.baseFee, 7 gwei);
        (uint256 price, uint64 updatedAt) = oracle.latestVerifiedPrice();
        assertEq(s.price, price);
        assertEq(s.updatedAt, updatedAt);
        assertEq(s.reserve0, pool.reserve0());
        assertEq(s.reserve1, pool.reserve1());
        assertEq(s.virtual0, pool.virtual0());
        assertEq(s.virtual1, pool.virtual1());
        assertEq(s.balance0, 20_000e18);
        assertEq(s.balance1, 40_123e18);
    }

    function test_itFollowsTheChain() public {
        bytes memory before = _snapshot();
        vm.warp(block.timestamp + 15);
        vm.roll(block.number + 60);
        oracle.set(2.5e18);
        t1.mint(EXECUTOR, 1e18);
        Snapshot memory s = abi.decode(_snapshot(), (Snapshot));
        assertTrue(keccak256(before) != keccak256(abi.encode(s)));
        assertEq(s.blockNumber, 4_302);
        assertEq(s.price, 2.5e18);
        assertEq(s.updatedAt, 1_767_225_615);
        assertEq(s.balance1, 40_124e18);
    }

    /// @notice An oracle that has never been pushed reads (0, 0): the engine sees it as stale, never as a price.
    function test_aFeedWithNoPriceYetReadsAsZero() public {
        MockOracle empty = new MockOracle();
        DarkPool fresh = new DarkPool(
            IERC20Min(address(t0)),
            IERC20Min(address(t1)),
            2_000_000e18,
            4_000_000e18,
            IZKVerifier(address(new MockVerifier(true))),
            IZKVerifier(address(new MockVerifier(true))),
            IPoseidonHasher(address(new MockHasher())),
            IZKOracle(address(empty)),
            ITreasury(address(new MockTreasury())),
            4
        );
        Snapshot memory s =
            abi.decode(address(new EngineLens(IDarkPoolLens(address(fresh)), EXECUTOR)).code, (Snapshot));
        assertEq(s.price, 0);
        assertEq(s.updatedAt, 0);
        assertEq(s.reserve0, 0);
    }
}
