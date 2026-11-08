// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../src/interfaces/ITreasury.sol";
import {PoseidonHasherAdapter, IPoseidonT3, JoinSplitVerifierAdapter, IGroth16Verifier9} from "../src/verifiers/Groth16Adapters.sol";
import {GenericGroth16Adapter} from "../src/verifiers/GenericGroth16Adapter.sol";
import {JoinSplitGroth16Verifier} from "../test/fixtures/JoinSplitGroth16Verifier.dev.sol";
import {BatchSwapGroth16Verifier} from "../test/fixtures/BatchSwapGroth16Verifier.dev.sol";
import {MockERC20, DevLiveOracle, MockTreasury} from "../test/mocks/Mocks.sol";

/// @notice LOCAL DEVELOPMENT / E2E ONLY. Deploys a complete DarkPool (real Poseidon, real Groth16 verifiers built
///         from DEV keys, batch size 4) plus open-mint dev tokens, an always-fresh dev oracle at the seeded spot price
///         (2.0), a mock treasury, seeds liquidity, and funds a user. Never run against a live network: the tokens are freely mintable and the keys are insecure.
///         The Poseidon contract's bytecode cannot be deployed with `new`, so e2e/run.mjs deploys it first and passes
///         its address in $POSEIDON.
/// Usage: POSEIDON=0x… USER=0x… forge script script/DeployLocal.s.sol --rpc-url … --private-key … --broadcast
contract DeployLocal is Script {
    function run() external {
        address poseidon = vm.envAddress("POSEIDON");
        address user = vm.envAddress("USER");

        vm.startBroadcast();
        MockERC20 a = new MockERC20("BERRY");
        MockERC20 b = new MockERC20("USDB");
        (MockERC20 t0, MockERC20 t1) = address(a) < address(b) ? (a, b) : (b, a); // the pool requires token0 < token1

        IZKVerifier joinSplit = new JoinSplitVerifierAdapter(IGroth16Verifier9(address(new JoinSplitGroth16Verifier())), 600_000);
        IZKVerifier batch = new GenericGroth16Adapter(address(new BatchSwapGroth16Verifier()), 16, 900_000); // 8 + 2*4 public inputs
        IPoseidonHasher hasher = new PoseidonHasherAdapter(IPoseidonT3(poseidon));

        DarkPool pool = new DarkPool(
            IERC20Min(address(t0)), IERC20Min(address(t1)), 2_000_000e18, 4_000_000e18, joinSplit, batch, hasher,
            IZKOracle(address(new DevLiveOracle(2e18))), ITreasury(address(new MockTreasury())), 4
        );

        t0.mint(msg.sender, 1_000_000e18);
        t1.mint(msg.sender, 2_000_000e18);
        t0.approve(address(pool), type(uint256).max);
        t1.approve(address(pool), type(uint256).max);
        pool.addLiquidity(1_000_000e18, 2_000_000e18, msg.sender);

        t0.mint(user, 10_000e18);
        t1.mint(user, 10_000e18);
        vm.stopBroadcast();

        console2.log("ADDR_POOL", address(pool));
        console2.log("ADDR_TOKEN0", address(t0));
        console2.log("ADDR_TOKEN1", address(t1));
    }
}
