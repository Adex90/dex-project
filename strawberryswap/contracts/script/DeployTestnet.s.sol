// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {DarkPool} from "../src/dex/DarkPool.sol";
import {SearcherExecutor, IDarkPoolSearcher} from "../src/periphery/SearcherExecutor.sol";
import {ZkDecoOracle, IGroth16Verifier5} from "../src/oracle/ZkDecoOracle.sol";
import {ZkPriceFeed} from "../src/oracle/ZkPriceFeed.sol";
import {IZKDecoOracle} from "../src/interfaces/IZKDecoOracle.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {IZKVerifier} from "../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../src/interfaces/ITreasury.sol";
import {
    PoseidonHasherAdapter,
    IPoseidonT3,
    JoinSplitVerifierAdapter,
    IGroth16Verifier9
} from "../src/verifiers/Groth16Adapters.sol";
import {GenericGroth16Adapter} from "../src/verifiers/GenericGroth16Adapter.sol";
import {TestToken} from "../src/testnet/TestToken.sol";
import {Treasury} from "../src/testnet/Treasury.sol";
import {TestnetDisclosure} from "../src/testnet/TestnetDisclosure.sol";
// DEV-KEY verifiers (single-party setup): testnet only, named in TestnetDisclosure and the frontend's banner.
import {JoinSplitGroth16Verifier} from "../test/fixtures/JoinSplitGroth16Verifier.dev.sol";
import {BatchSwapGroth16Verifier} from "../test/fixtures/BatchSwapGroth16Verifier.dev.sol";
import {OracleGroth16Verifier} from "../test/fixtures/OracleGroth16Verifier.dev.sol";
import {PoseidonT3Code} from "../test/fixtures/PoseidonT3Code.sol";

/// @notice What a testnet deployment is: every contract, and the inputs it was built from.
struct Deployed {
    uint256 chainId;
    uint256 deployBlock;
    address deployer;
    address operator;
    address notary;
    bytes32 providerId;
    bytes32 queryCommitment;
    uint256 price;
    address tWETH;
    address tUSD;
    address poseidon;
    address hasher;
    address joinSplitGroth16;
    address joinSplitVerifier;
    address batchGroth16;
    address batchVerifier;
    address oracleVerifier;
    address notaryRegistry;
    address feed;
    address treasury;
    address pool;
    address executor;
    address disclosure;
}

/// @notice The checks a deployment must pass, shared by DeployTestnet (right after deploying) and CheckTestnet (any
///         time later, from the manifest).
abstract contract TestnetChecks is Script {
    uint256 internal constant ARBITRUM_SEPOLIA = 421_614;
    uint256 internal constant ANVIL = 31_337;
    /// @dev The dev batch verifier is batch_swap with N = 4: 8 + 2·4 public inputs.
    uint256 internal constant BATCH_SIZE = 4;
    /// @dev circomlib's Poseidon(2) of [1, 2]: the deployed hasher must be the circuits' hash.
    bytes32 internal constant POSEIDON_1_2 = 0x115cc0f5e7d690413df64c6b9662e9cf2a3617f2743245519e19607a4417189a;

    function testnetOnly() internal view {
        require(
            block.chainid == ARBITRUM_SEPOLIA || block.chainid == ANVIL,
            "testnet only: Arbitrum Sepolia (421614) or anvil (31337); the dev-key verifiers never go to mainnet"
        );
    }

    function check(Deployed memory d) internal view {
        DarkPool pool = DarkPool(d.pool);
        require(d.tWETH < d.tUSD, "tWETH must be token0: the oracle prices token0 (ETH) in token1 (USD)");
        require(address(pool.token0()) == d.tWETH && address(pool.token1()) == d.tUSD, "the pool's tokens");
        require(address(pool.oracle()) == d.feed, "the pool reads the feed");
        require(address(pool.treasury()) == d.treasury, "the pool pays the treasury");
        require(pool.batchSize() == BATCH_SIZE, "the pool's batch size is the dev batch verifier's");
        require(address(pool.batchVerifier()) == d.batchVerifier, "the pool's batch verifier");
        require(address(pool.joinSplitVerifier()) == d.joinSplitVerifier, "the pool's join-split verifier");
        require(IPoseidonHasher(d.hasher).poseidon(bytes32(uint256(1)), bytes32(uint256(2))) == POSEIDON_1_2, "Poseidon");

        ZkPriceFeed feed = ZkPriceFeed(d.feed);
        require(address(feed.verifier()) == d.notaryRegistry, "the feed's notary registry");
        require(feed.providerId() == d.providerId, "the feed's provider");
        require(feed.queryCommitment() == d.queryCommitment, "the feed's instrument");
        ZkDecoOracle registry = ZkDecoOracle(d.notaryRegistry);
        require(registry.notaryOf(d.providerId) == d.notary, "the registry's notary is the notary's KMS address");
        require(address(registry.verifier()) == d.oracleVerifier, "the registry's verifier");

        SearcherExecutor executor = SearcherExecutor(d.executor);
        require(executor.operator() == d.operator, "the executor's operator is the operator's KMS address");
        require(address(executor.pool()) == d.pool, "the executor's pool");

        Treasury treasury = Treasury(d.treasury);
        require(treasury.isPool(d.pool), "the treasury records the pool's extractions");
        require(treasury.admin() == d.deployer, "the treasury's admin");

        TestnetDisclosure disclosure = TestnetDisclosure(d.disclosure);
        require(
            disclosure.pool() == d.pool && disclosure.feed() == d.feed && disclosure.executor() == d.executor
                && disclosure.joinSplitVerifier() == d.joinSplitVerifier && disclosure.batchVerifier() == d.batchVerifier
                && disclosure.oracleVerifier() == d.oracleVerifier,
            "the disclosure names this deployment"
        );
        require(TestToken(d.tWETH).decimals() == 18 && TestToken(d.tUSD).decimals() == 18, "18-decimal tokens");
    }
}

/// @title DeployTestnet
/// @notice TESTNET ONLY: the StrawberrySwap deployment on Arbitrum Sepolia (also runs on anvil, for CI).
///         * tWETH and tUSD: faucet-capped 18-decimal test tokens; tWETH is token0 (its address is the lower one).
///         * The DEV-key verifiers (join-split, batch, oracle) behind their production adapters, and Poseidon.
///         * ZkDecoOracle, the notary registry, with the notary's KMS address; ZkPriceFeed for ETHUSD.
///         * A minimal Treasury; DarkPool at the starting price; SearcherExecutor bound to the operator's KMS address.
///         * TestnetDisclosure, the on-chain trusted-bootstrap notice.
///         It seeds the pool's liquidity and the executor's inventory, checks every wiring, and writes the manifest.
///
/// Inputs (environment): OPERATOR_ADDRESS, NOTARY_ADDRESS, PROVIDER_ID, QUERY_COMMITMENT, INITIAL_PRICE (tUSD per
/// tWETH, 1e18-scaled) are required; the rest have defaults (see `config`). The deployer is DEPLOYER (default: the
/// script's sender, e.g. `--sender` with `--aws` for a KMS deployer key).
///
///   forge script script/DeployTestnet.s.sol --rpc-url arbitrum_sepolia --aws --sender <deployer> --broadcast --verify
contract DeployTestnet is TestnetChecks {
    struct Config {
        address deployer;
        address operator;
        address notary;
        address relayer;
        address automator;
        bytes32 providerId;
        bytes32 queryCommitment;
        uint256 price;
        uint256 liquidityWeth;
        uint256 virtualWeth;
        uint256 executorWeth;
        uint256 executorUsd;
        uint64 feedMaxDelay;
        uint256 feedMinPrice;
        uint256 feedMaxPrice;
        uint256 botGasWei;
        string manifest;
    }

    /// @dev Faucets: 1 tWETH and 5,000 tUSD a day per address, up to 100,000 tWETH and 500,000,000 tUSD in all.
    uint256 internal constant FAUCET_COOLDOWN = 1 days;
    uint256 internal constant WETH_FAUCET = 1e18;
    uint256 internal constant WETH_FAUCET_CAP = 100_000e18;
    uint256 internal constant USD_FAUCET = 5_000e18;
    uint256 internal constant USD_FAUCET_CAP = 500_000_000e18;
    /// @dev Gas forwarded to the SNARK verifiers (bounds the burn of a malformed proof).
    uint256 internal constant JOINSPLIT_GAS = 600_000;
    uint256 internal constant BATCH_GAS = 900_000;
    uint256 internal constant ORACLE_GAS = 600_000;

    function config() public view returns (Config memory c) {
        c.deployer = vm.envOr("DEPLOYER", msg.sender);
        c.operator = vm.envAddress("OPERATOR_ADDRESS");
        c.notary = vm.envAddress("NOTARY_ADDRESS");
        c.relayer = vm.envOr("RELAYER_ADDRESS", address(0));
        c.automator = vm.envOr("AUTOMATOR_ADDRESS", address(0));
        c.providerId = vm.envBytes32("PROVIDER_ID");
        c.queryCommitment = vm.envBytes32("QUERY_COMMITMENT");
        c.price = vm.envUint("INITIAL_PRICE");
        c.liquidityWeth = vm.envOr("LIQUIDITY_WETH", uint256(100e18));
        c.virtualWeth = vm.envOr("VIRTUAL_WETH", c.liquidityWeth);
        c.executorWeth = vm.envOr("EXECUTOR_WETH", uint256(20e18));
        c.executorUsd = vm.envOr("EXECUTOR_TUSD", uint256(100_000e18));
        c.feedMaxDelay = uint64(vm.envOr("FEED_MAX_DELAY", uint256(30)));
        c.feedMinPrice = vm.envOr("FEED_MIN_PRICE", uint256(100e18));
        c.feedMaxPrice = vm.envOr("FEED_MAX_PRICE", uint256(100_000e18));
        c.botGasWei = vm.envOr("BOT_GAS_WEI", uint256(0));
        c.manifest = vm.envOr("MANIFEST", string.concat("deployments/", vm.toString(block.chainid), ".json"));
    }

    function validate(Config memory c) internal pure {
        require(c.operator != address(0) && c.notary != address(0), "OPERATOR_ADDRESS and NOTARY_ADDRESS");
        // Every role its own KMS key: deployer, operator, notary, and the relayer and automator when given.
        address[5] memory keys = [c.deployer, c.operator, c.notary, c.relayer, c.automator];
        for (uint256 i; i < keys.length; ++i) {
            for (uint256 j = i + 1; j < keys.length; ++j) {
                require(keys[j] == address(0) || keys[i] != keys[j], "one key per role");
            }
        }
        require(c.providerId != bytes32(0) && c.queryCommitment != bytes32(0), "PROVIDER_ID and QUERY_COMMITMENT");
        require(c.price >= c.feedMinPrice && c.price <= c.feedMaxPrice, "INITIAL_PRICE within the feed's bounds");
        require(c.liquidityWeth >= 1e18 && c.virtualWeth >= 1e18, "at least 1 tWETH of liquidity");
    }

    function run() external returns (Deployed memory) {
        return deploy(config());
    }

    /// @notice The deployment itself, from an explicit configuration (`run` reads it from the environment).
    function deploy(Config memory c) public returns (Deployed memory d) {
        testnetOnly();
        validate(c);
        d.chainId = block.chainid;
        d.deployBlock = block.number;
        d.deployer = c.deployer;
        d.operator = c.operator;
        d.notary = c.notary;
        d.providerId = c.providerId;
        d.queryCommitment = c.queryCommitment;
        d.price = c.price;

        vm.startBroadcast(c.deployer);
        deployTokens(c, d);
        deployVerifiers(d);
        deployProtocol(c, d);
        seed(c, d);
        d.disclosure = address(
            new TestnetDisclosure(
                TestnetDisclosure.Deployment({
                    pool: d.pool,
                    executor: d.executor,
                    treasury: d.treasury,
                    feed: d.feed,
                    notaryRegistry: d.notaryRegistry,
                    token0: d.tWETH,
                    token1: d.tUSD,
                    joinSplitVerifier: d.joinSplitVerifier,
                    batchVerifier: d.batchVerifier,
                    oracleVerifier: d.oracleVerifier,
                    hasher: d.hasher
                })
            )
        );
        vm.stopBroadcast();

        check(d);
        uint256 spot = DarkPool(d.pool).spotPrice();
        uint256 gap = spot > c.price ? spot - c.price : c.price - spot;
        require(gap <= c.price / 1e9, "the pool starts at INITIAL_PRICE");
        if (bytes(c.manifest).length != 0) writeManifest(d, c.manifest);
        console2.log("DEPLOYED pool", d.pool);
        console2.log("DEPLOYED executor", d.executor);
        console2.log("DEPLOYED feed", d.feed);
        console2.log("DEPLOYED disclosure", d.disclosure);
    }

    /// @dev tWETH must be token0, so its address must be the lower one. The deployer's next two addresses are known
    ///      (CREATE: deployer and nonce), so whichever is lower gets tWETH: no salt mining, no factory.
    function deployTokens(Config memory c, Deployed memory d) internal {
        uint64 nonce = vm.getNonce(c.deployer);
        bool wethFirst = vm.computeCreateAddress(c.deployer, nonce) < vm.computeCreateAddress(c.deployer, nonce + 1);
        if (wethFirst) {
            d.tWETH = address(newWeth(c));
            d.tUSD = address(newUsd(c));
        } else {
            d.tUSD = address(newUsd(c));
            d.tWETH = address(newWeth(c));
        }
        require(d.tWETH < d.tUSD, "token order");
    }

    function newWeth(Config memory c) internal returns (TestToken) {
        return new TestToken(
            "StrawberrySwap Test WETH", "tWETH", c.deployer, WETH_FAUCET, FAUCET_COOLDOWN, WETH_FAUCET_CAP
        );
    }

    function newUsd(Config memory c) internal returns (TestToken) {
        return new TestToken("StrawberrySwap Test USD", "tUSD", c.deployer, USD_FAUCET, FAUCET_COOLDOWN, USD_FAUCET_CAP);
    }

    function deployVerifiers(Deployed memory d) internal {
        // Poseidon has no Solidity source (circomlibjs generates its bytecode), so it is deployed as raw creation code.
        bytes memory code = PoseidonT3Code.CODE;
        address poseidon;
        assembly {
            poseidon := create(0, add(code, 0x20), mload(code))
        }
        require(poseidon != address(0), "Poseidon deployment failed");
        d.poseidon = poseidon;
        d.hasher = address(new PoseidonHasherAdapter(IPoseidonT3(poseidon)));
        d.joinSplitGroth16 = address(new JoinSplitGroth16Verifier());
        d.joinSplitVerifier =
            address(new JoinSplitVerifierAdapter(IGroth16Verifier9(d.joinSplitGroth16), JOINSPLIT_GAS));
        d.batchGroth16 = address(new BatchSwapGroth16Verifier());
        d.batchVerifier = address(new GenericGroth16Adapter(d.batchGroth16, 8 + 2 * BATCH_SIZE, BATCH_GAS));
        d.oracleVerifier = address(new OracleGroth16Verifier());
    }

    function deployProtocol(Config memory c, Deployed memory d) internal {
        bytes32[] memory providers = new bytes32[](1);
        providers[0] = c.providerId;
        address[] memory notaries = new address[](1);
        notaries[0] = c.notary;
        d.notaryRegistry =
            address(new ZkDecoOracle(IGroth16Verifier5(d.oracleVerifier), ORACLE_GAS, providers, notaries));
        d.feed = address(
            new ZkPriceFeed(
                ZkPriceFeed.Config({
                    verifier: IZKDecoOracle(d.notaryRegistry),
                    providerId: c.providerId,
                    queryCommitment: c.queryCommitment,
                    maxSubmissionDelay: c.feedMaxDelay,
                    minPrice: c.feedMinPrice,
                    maxPrice: c.feedMaxPrice
                })
            )
        );
        Treasury treasury = new Treasury(c.deployer);
        d.treasury = address(treasury);
        d.pool = address(
            new DarkPool(
                IERC20Min(d.tWETH),
                IERC20Min(d.tUSD),
                c.virtualWeth,
                c.virtualWeth * c.price / 1e18,
                IZKVerifier(d.joinSplitVerifier),
                IZKVerifier(d.batchVerifier),
                IPoseidonHasher(d.hasher),
                IZKOracle(d.feed),
                ITreasury(d.treasury),
                BATCH_SIZE
            )
        );
        treasury.registerPool(d.pool);
        d.executor = address(new SearcherExecutor(c.operator, IDarkPoolSearcher(d.pool)));
    }

    /// @dev The pool's liquidity at the starting price, the executor's inventory, and (optionally) gas for the bots.
    function seed(Config memory c, Deployed memory d) internal {
        TestToken weth = TestToken(d.tWETH);
        TestToken usd = TestToken(d.tUSD);
        uint256 liquidityUsd = c.liquidityWeth * c.price / 1e18;
        weth.mint(c.deployer, c.liquidityWeth);
        usd.mint(c.deployer, liquidityUsd);
        weth.approve(d.pool, c.liquidityWeth);
        usd.approve(d.pool, liquidityUsd);
        DarkPool(d.pool).addLiquidity(c.liquidityWeth, liquidityUsd, c.deployer);
        weth.mint(d.executor, c.executorWeth);
        usd.mint(d.executor, c.executorUsd);
        if (c.botGasWei != 0) {
            address[3] memory bots = [c.operator, c.relayer, c.automator];
            for (uint256 i; i < bots.length; ++i) {
                if (bots[i] == address(0)) continue;
                (bool ok,) = bots[i].call{value: c.botGasWei}("");
                require(ok, "gas transfer");
            }
        }
    }

    function writeManifest(Deployed memory d, string memory path) internal {
        string memory k = "contracts";
        vm.serializeAddress(k, "tWETH", d.tWETH);
        vm.serializeAddress(k, "tUSD", d.tUSD);
        vm.serializeAddress(k, "poseidon", d.poseidon);
        vm.serializeAddress(k, "hasher", d.hasher);
        vm.serializeAddress(k, "joinSplitGroth16", d.joinSplitGroth16);
        vm.serializeAddress(k, "joinSplitVerifier", d.joinSplitVerifier);
        vm.serializeAddress(k, "batchGroth16", d.batchGroth16);
        vm.serializeAddress(k, "batchVerifier", d.batchVerifier);
        vm.serializeAddress(k, "oracleVerifier", d.oracleVerifier);
        vm.serializeAddress(k, "notaryRegistry", d.notaryRegistry);
        vm.serializeAddress(k, "feed", d.feed);
        vm.serializeAddress(k, "treasury", d.treasury);
        vm.serializeAddress(k, "pool", d.pool);
        vm.serializeAddress(k, "executor", d.executor);
        string memory contracts = vm.serializeAddress(k, "disclosure", d.disclosure);

        string memory r = "root";
        vm.serializeString(r, "network", d.chainId == ARBITRUM_SEPOLIA ? "arbitrum-sepolia" : "anvil");
        vm.serializeUint(r, "chainId", d.chainId);
        vm.serializeUint(r, "deployBlock", d.deployBlock);
        vm.serializeAddress(r, "deployer", d.deployer);
        vm.serializeAddress(r, "operator", d.operator);
        vm.serializeAddress(r, "notary", d.notary);
        vm.serializeBytes32(r, "providerId", d.providerId);
        vm.serializeBytes32(r, "queryCommitment", d.queryCommitment);
        vm.serializeString(r, "initialPrice", vm.toString(d.price));
        vm.serializeUint(r, "batchSize", BATCH_SIZE);
        vm.serializeBool(r, "devKeyVerifiers", true);
        string memory root = vm.serializeString(r, "contracts", contracts);
        vm.writeJson(root, path);
        console2.log("MANIFEST", path);
    }
}

/// @title CheckTestnet
/// @notice Re-checks a deployment from its manifest, read-only: MANIFEST=deployments/421614.json forge script
///         script/CheckTestnet... (any RPC; no key).
contract CheckTestnet is TestnetChecks {
    function run() external view returns (Deployed memory) {
        return checkManifest(vm.envOr("MANIFEST", string.concat("deployments/", vm.toString(block.chainid), ".json")));
    }

    function checkManifest(string memory path) public view returns (Deployed memory d) {
        testnetOnly();
        string memory json = vm.readFile(path);
        require(vm.parseJsonUint(json, ".chainId") == block.chainid, "the manifest is for another chain");
        d.chainId = block.chainid;
        d.deployer = vm.parseJsonAddress(json, ".deployer");
        d.operator = vm.parseJsonAddress(json, ".operator");
        d.notary = vm.parseJsonAddress(json, ".notary");
        d.providerId = vm.parseJsonBytes32(json, ".providerId");
        d.queryCommitment = vm.parseJsonBytes32(json, ".queryCommitment");
        d.tWETH = vm.parseJsonAddress(json, ".contracts.tWETH");
        d.tUSD = vm.parseJsonAddress(json, ".contracts.tUSD");
        d.poseidon = vm.parseJsonAddress(json, ".contracts.poseidon");
        d.hasher = vm.parseJsonAddress(json, ".contracts.hasher");
        d.joinSplitGroth16 = vm.parseJsonAddress(json, ".contracts.joinSplitGroth16");
        d.joinSplitVerifier = vm.parseJsonAddress(json, ".contracts.joinSplitVerifier");
        d.batchGroth16 = vm.parseJsonAddress(json, ".contracts.batchGroth16");
        d.batchVerifier = vm.parseJsonAddress(json, ".contracts.batchVerifier");
        d.oracleVerifier = vm.parseJsonAddress(json, ".contracts.oracleVerifier");
        d.notaryRegistry = vm.parseJsonAddress(json, ".contracts.notaryRegistry");
        d.feed = vm.parseJsonAddress(json, ".contracts.feed");
        d.treasury = vm.parseJsonAddress(json, ".contracts.treasury");
        d.pool = vm.parseJsonAddress(json, ".contracts.pool");
        d.executor = vm.parseJsonAddress(json, ".contracts.executor");
        d.disclosure = vm.parseJsonAddress(json, ".contracts.disclosure");
        check(d);
        console2.log("CHECKED", d.pool);
    }
}
