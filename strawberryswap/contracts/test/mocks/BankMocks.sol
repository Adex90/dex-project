// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20Min} from "../../src/interfaces/IERC20Min.sol";
import {IYieldAdapter} from "../../src/banking/SovereignBank.sol";
import {MockERC20} from "./Mocks.sol";

/// A token whose holders can lose balance without a transfer (negative rebase, slashing, a bridge haircut).
contract RebasingToken is MockERC20 {
    constructor() MockERC20("RB") {}

    function rebaseDown(address holder, uint256 bps) external {
        balanceOf[holder] -= balanceOf[holder] * bps / 10_000;
    }
}

/// Charges `feeBps` on every transfer (taken from the amount sent): what fee-on-transfer tokens do.
contract FeeToken is MockERC20 {
    uint256 public feeBps;

    constructor(uint256 bps) MockERC20("FEE") {
        feeBps = bps;
    }

    function transfer(address to, uint256 a) external override returns (bool) {
        balanceOf[msg.sender] -= a;
        balanceOf[to] += a - a * feeBps / 10_000;
        return true;
    }

    function transferFrom(address f, address to, uint256 a) external override returns (bool) {
        uint256 al = allowance[f][msg.sender];
        if (al != type(uint256).max) allowance[f][msg.sender] = al - a;
        balanceOf[f] -= a;
        balanceOf[to] += a - a * feeBps / 10_000;
        return true;
    }
}

/// A compliant ROUTED adapter: only the depositor may recall; recall APPROVES the amount due and returns it (no push).
/// `yieldBps` is added at recall (a negative value would model a credit loss).
contract PullAdapter is IYieldAdapter {
    struct Pos {
        address depositor;
        MockERC20 token;
        uint256 amount;
        uint64 maturity;
    }

    mapping(bytes32 => Pos) public pos;
    int256 public yieldBps;
    uint256 n;

    function setYieldBps(int256 y) external {
        yieldBps = y;
    }

    function deploy(IERC20Min token, uint256 amount, uint64 maturity, bytes calldata) external returns (bytes32 h) {
        h = bytes32(++n);
        pos[h] = Pos(msg.sender, MockERC20(address(token)), amount, maturity);
    }

    function recall(bytes32 h) external returns (uint256 due) {
        Pos memory p = pos[h];
        require(msg.sender == p.depositor && p.amount > 0 && block.timestamp >= p.maturity, "not recallable");
        delete pos[h];
        due = uint256(int256(p.amount) + int256(p.amount) * yieldBps / 10_000);
        if (due > p.amount) p.token.mint(address(this), due - p.amount); // the strategy's profit
        p.token.approve(msg.sender, due);
    }
}

/// NON-compliant adapter (the H-6 victim shape): anyone may trigger recall, which PUSHES to the depositor.
contract OpenPushAdapter is IYieldAdapter {
    struct Pos {
        address depositor;
        IERC20Min token;
        uint256 amount;
        uint64 maturity;
    }

    mapping(bytes32 => Pos) public pos;
    uint256 n;

    function deploy(IERC20Min token, uint256 amount, uint64 maturity, bytes calldata) external returns (bytes32 h) {
        h = bytes32(++n);
        pos[h] = Pos(msg.sender, token, amount, maturity);
    }

    function recall(bytes32 h) external returns (uint256) {
        Pos memory p = pos[h];
        require(p.amount > 0 && block.timestamp >= p.maturity, "nothing due");
        delete pos[h];
        p.token.transfer(p.depositor, p.amount);
        return 0;
    }
}

/// The attacker's adapter: its recall triggers somebody else's repayment inside TimeDeposits.settle.
contract ThiefAdapter is IYieldAdapter {
    IYieldAdapter immutable victimAdapter;
    bytes32 public victimHandle;

    constructor(IYieldAdapter v) {
        victimAdapter = v;
    }

    function setTarget(bytes32 h) external {
        victimHandle = h;
    }

    function deploy(IERC20Min, uint256, uint64, bytes calldata) external pure returns (bytes32) {
        return bytes32(uint256(1));
    }

    function recall(bytes32) external returns (uint256) {
        victimAdapter.recall(victimHandle);
        return 0;
    }
}

/// An adapter whose recall always fails (a defaulted strategy).
contract DeadAdapter is IYieldAdapter {
    function deploy(IERC20Min, uint256, uint64, bytes calldata) external pure returns (bytes32) {
        return bytes32(uint256(1));
    }

    function recall(bytes32) external pure returns (uint256) {
        revert("defaulted");
    }
}
