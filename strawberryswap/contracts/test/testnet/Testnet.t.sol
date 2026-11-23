// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeployTestnet, CheckTestnet, Deployed} from "../../script/DeployTestnet.s.sol";
import {DarkPool} from "../../src/dex/DarkPool.sol";
import {SearcherExecutor} from "../../src/periphery/SearcherExecutor.sol";
import {TestToken} from "../../src/testnet/TestToken.sol";
import {Treasury} from "../../src/testnet/Treasury.sol";
import {TestnetDisclosure} from "../../src/testnet/TestnetDisclosure.sol";
import {IZKOracle} from "../../src/interfaces/IZKOracle.sol";
import {IERC20Min} from "../../src/interfaces/IERC20Min.sol";

contract TestTokenTest is Test {
    TestToken token;
    address minter = makeAddr("minter");
    address alice = makeAddr("alice");

    function setUp() public {
        token = new TestToken("StrawberrySwap Test USD", "tUSD", minter, 5_000e18, 1 days, 12_000e18);
    }

    function test_the_faucet_pays_once_a_day_until_its_cap() public {
        vm.warp(1_000_000);
        vm.prank(alice);
        token.faucet();
        assertEq(token.balanceOf(alice), 5_000e18);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(TestToken.FaucetCooldown.selector, 1_000_000 + 1 days));
        token.faucet();
        vm.warp(1_000_000 + 1 days);
        vm.prank(alice);
        token.faucet();
        assertEq(token.balanceOf(alice), 10_000e18);
        // A third 5,000 would pass the 12,000 cap, whoever asks.
        vm.prank(makeAddr("bob"));
        vm.expectRevert(abi.encodeWithSelector(TestToken.FaucetDry.selector, 10_000e18, 12_000e18));
        token.faucet();
        assertEq(token.totalSupply(), 10_000e18);
    }

    function test_only_the_minter_mints() public {
        vm.prank(alice);
        vm.expectRevert(TestToken.NotMinter.selector);
        token.mint(alice, 1);
        vm.prank(minter);
        token.mint(alice, 7e18);
        assertEq(token.balanceOf(alice), 7e18);
    }

    function test_transfers_and_allowances() public {
        vm.prank(minter);
        token.mint(alice, 10e18);
        address bob = makeAddr("bob");
        vm.prank(alice);
        token.approve(bob, 4e18);
        vm.prank(bob);
        token.transferFrom(alice, bob, 3e18);
        assertEq(token.allowance(alice, bob), 1e18);
        vm.prank(bob);
        vm.expectRevert(TestToken.InsufficientAllowance.selector);
        token.transferFrom(alice, bob, 2e18);
        vm.prank(alice);
        vm.expectRevert(TestToken.InsufficientBalance.selector);
        token.transfer(bob, 8e18);
        vm.prank(alice);
        vm.expectRevert(TestToken.ZeroAddress.selector);
        token.transfer(address(0), 1);
        assertEq(token.decimals(), 18);
    }

    function test_a_faucet_must_be_bounded() public {
        vm.expectRevert(TestToken.BadFaucet.selector);
        new TestToken("x", "x", minter, 2, 1 days, 1);
        vm.expectRevert(TestToken.BadFaucet.selector);
        new TestToken("x", "x", minter, 1, 0, 1);
        vm.expectRevert(TestToken.ZeroAddress.selector);
        new TestToken("x", "x", address(0), 1, 1, 1);
    }
}

contract TreasuryTest is Test {
    Treasury treasury;
    address admin = makeAddr("admin");
    address pool = makeAddr("pool");
    TestToken usd;

    function setUp() public {
        treasury = new Treasury(admin);
        usd = new TestToken("tUSD", "tUSD", address(this), 1e18, 1 days, 1e18);
    }

    function test_only_registered_pools_record() public {
        vm.expectRevert(Treasury.NotPool.selector);
        vm.prank(pool);
        treasury.recordExtraction(address(usd), 5e18, address(this));
        vm.expectRevert(Treasury.NotAdmin.selector);
        treasury.registerPool(pool);
        vm.prank(admin);
        treasury.registerPool(pool);
        vm.prank(pool);
        treasury.recordExtraction(address(usd), 5e18, address(this));
        assertEq(treasury.recorded(address(usd)), 5e18);
    }

    function test_only_the_admin_withdraws() public {
        usd.mint(address(treasury), 9e18);
        vm.expectRevert(Treasury.NotAdmin.selector);
        treasury.withdraw(IERC20Min(address(usd)), address(this), 1e18);
        vm.prank(admin);
        treasury.withdraw(IERC20Min(address(usd)), admin, 4e18);
        assertEq(usd.balanceOf(admin), 4e18);
        vm.prank(admin);
        vm.expectRevert(Treasury.ZeroAddress.selector);
        treasury.withdraw(IERC20Min(address(usd)), address(0), 1);
    }
}

contract DeployTestnetTest is Test {
    address deployer = makeAddr("deployer");
    address operator = makeAddr("operator (KMS)");
    address notary = makeAddr("notary (KMS)");
    address relayer = makeAddr("relayer (KMS)");
    uint256 constant PRICE = 3_456.78e18;

    function setUp() public {
        vm.deal(deployer, 10 ether);
    }

    /// The configuration `run` would read from the environment (tests pass it directly: they run in parallel).
    function cfg() internal view returns (DeployTestnet.Config memory c) {
        c.deployer = deployer;
        c.operator = operator;
        c.notary = notary;
        c.relayer = relayer;
        c.providerId = keccak256("provider");
        c.queryCommitment = keccak256("ETHUSD commitment");
        c.price = PRICE;
        c.liquidityWeth = 100e18;
        c.virtualWeth = 100e18;
        c.executorWeth = 20e18;
        c.executorUsd = 100_000e18;
        c.feedMaxDelay = 30;
        c.feedMinPrice = 100e18;
        c.feedMaxPrice = 100_000e18;
        c.botGasWei = 0.5 ether;
    }

    function deploy() internal returns (Deployed memory) {
        return new DeployTestnet().deploy(cfg());
    }

    function test_the_deployment_is_wired_and_seeded() public {
        Deployed memory d = deploy();
        assertLt(uint160(d.tWETH), uint160(d.tUSD), "tWETH is token0");
        DarkPool pool = DarkPool(d.pool);
        assertEq(pool.spotPrice(), PRICE);
        assertEq(TestToken(d.tWETH).balanceOf(d.executor), 20e18);
        assertEq(TestToken(d.tUSD).balanceOf(d.executor), 100_000e18);
        assertEq(TestToken(d.tWETH).symbol(), "tWETH");
        assertEq(TestToken(d.tUSD).symbol(), "tUSD");
        assertEq(SearcherExecutor(d.executor).operator(), operator);
        assertEq(relayer.balance, 0.5 ether, "the bots got their gas");
        assertEq(operator.balance, 0.5 ether);
        address[3] memory dev = TestnetDisclosure(d.disclosure).devKeyVerifiers();
        assertEq(dev[0], d.joinSplitVerifier);
        assertEq(dev[2], d.oracleVerifier);
        assertGt(bytes(TestnetDisclosure(d.disclosure).DISCLOSURE()).length, 200);
    }

    /// The pool trades and pays the real Treasury: the operator's rebalance toward a fresh oracle price is recorded.
    function test_a_rebalance_pays_the_treasury() public {
        Deployed memory d = deploy();
        vm.mockCall(
            d.feed,
            abi.encodeWithSelector(IZKOracle.latestVerifiedPrice.selector),
            abi.encode(uint256(3_500e18), uint64(block.timestamp))
        );
        Treasury treasury = Treasury(d.treasury);
        vm.prank(operator);
        SearcherExecutor(d.executor).rebalance(1e18, 0, 0, 3_475e18, 25e18);
        // At 3,500 the pool's value falls by 1 tWETH·3,500 − 3,475 tUSD = 25 tUSD: the treasury pulls 80 % of it.
        uint256 paid = TestToken(d.tUSD).balanceOf(d.treasury);
        assertEq(paid, 20e18, "80 % of the 25 tUSD harvest");
        assertEq(treasury.recorded(d.tUSD), paid);
        assertEq(TestToken(d.tUSD).balanceOf(d.executor), 100_000e18 - 3_475e18 - 20e18);
        assertEq(TestToken(d.tWETH).balanceOf(d.executor), 21e18);
        assertGt(DarkPool(d.pool).spotPrice(), PRICE, "the pool moved toward the oracle");
        // Nobody but the operator rebalances.
        vm.expectRevert(SearcherExecutor.NotOperator.selector);
        SearcherExecutor(d.executor).rebalance(1e18, 0, 0, 3_475e18, 25e18);
    }

    function test_never_on_another_chain() public {
        DeployTestnet script = new DeployTestnet();
        DeployTestnet.Config memory c = cfg();
        vm.chainId(1);
        vm.expectRevert(
            bytes("testnet only: Arbitrum Sepolia (421614) or anvil (31337); the dev-key verifiers never go to mainnet")
        );
        script.deploy(c);
    }

    function test_one_key_per_role() public {
        DeployTestnet script = new DeployTestnet();
        DeployTestnet.Config memory c = cfg();
        c.notary = operator;
        vm.expectRevert(bytes("one key per role"));
        script.deploy(c);
        c = cfg();
        c.relayer = deployer;
        vm.expectRevert(bytes("one key per role"));
        script.deploy(c);
        c = cfg();
        c.automator = relayer;
        vm.expectRevert(bytes("one key per role"));
        script.deploy(c);
        c = cfg();
        c.price = 99e18;
        vm.expectRevert(bytes("INITIAL_PRICE within the feed's bounds"));
        script.deploy(c);
    }

    function test_the_manifest_is_checked_back() public {
        string memory path = string.concat("deployments/test-", vm.toString(uint256(uint160(address(this)))), ".json");
        DeployTestnet.Config memory c = cfg();
        c.manifest = path;
        Deployed memory d = new DeployTestnet().deploy(c);
        Deployed memory checked = new CheckTestnet().checkManifest(path);
        assertEq(checked.pool, d.pool);
        assertEq(checked.executor, d.executor);
        assertEq(vm.parseJsonUint(vm.readFile(path), ".batchSize"), 4);
        assertEq(vm.parseJsonBool(vm.readFile(path), ".devKeyVerifiers"), true);
        vm.removeFile(path);
    }
}
