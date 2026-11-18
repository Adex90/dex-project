// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20Min} from "../interfaces/IERC20Min.sol";

/// @title TestToken
/// @notice TESTNET ONLY: an 18-decimal ERC-20 with no value (tWETH and tUSD on Arbitrum Sepolia).
///         * `faucet()`: anyone may mint `faucetAmount` once per `faucetCooldown`, until the faucet has handed out
///           `faucetCap` in all; then it is dry. The cap bounds what any number of addresses can draw.
///         * `mint()`: the minter (the deployer) seeds the pool's liquidity and the SearcherExecutor's inventory.
///         Nothing else: no burn, no pause, no owner change, no upgrade.
contract TestToken is IERC20Min {
    string public name;
    string public symbol;
    uint256 public totalSupply;
    mapping(address => uint256) public override balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    /// @notice The only address that may mint outside the faucet.
    address public immutable minter;
    uint256 public immutable faucetAmount;
    uint256 public immutable faucetCooldown;
    /// @notice The most the faucet ever hands out, all addresses together.
    uint256 public immutable faucetCap;
    uint256 public faucetMinted;
    mapping(address => uint256) public nextFaucetAt;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event Faucet(address indexed to, uint256 amount);

    error NotMinter();
    error FaucetCooldown(uint256 nextAt);
    error FaucetDry(uint256 minted, uint256 cap);
    error InsufficientBalance();
    error InsufficientAllowance();
    error ZeroAddress();
    error BadFaucet();

    constructor(
        string memory name_,
        string memory symbol_,
        address minter_,
        uint256 faucetAmount_,
        uint256 faucetCooldown_,
        uint256 faucetCap_
    ) {
        if (minter_ == address(0)) revert ZeroAddress();
        if (faucetAmount_ == 0 || faucetAmount_ > faucetCap_ || faucetCooldown_ == 0) revert BadFaucet();
        name = name_;
        symbol = symbol_;
        minter = minter_;
        faucetAmount = faucetAmount_;
        faucetCooldown = faucetCooldown_;
        faucetCap = faucetCap_;
    }

    function decimals() external pure returns (uint8) {
        return 18;
    }

    /// @notice Mints `faucetAmount` to the caller, at most once per `faucetCooldown`, while the cap lasts.
    function faucet() external {
        uint256 nextAt = nextFaucetAt[msg.sender];
        // forge-lint: disable-next-line(block-timestamp)
        if (block.timestamp < nextAt) revert FaucetCooldown(nextAt);
        uint256 minted = faucetMinted + faucetAmount;
        if (minted > faucetCap) revert FaucetDry(faucetMinted, faucetCap);
        faucetMinted = minted;
        nextFaucetAt[msg.sender] = block.timestamp + faucetCooldown;
        _mint(msg.sender, faucetAmount);
        emit Faucet(msg.sender, faucetAmount);
    }

    function mint(address to, uint256 amount) external {
        if (msg.sender != minter) revert NotMinter();
        _mint(to, amount);
    }

    function approve(address spender, uint256 amount) external override returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transfer(address to, uint256 amount) external override returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external override returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed != type(uint256).max) {
            if (allowed < amount) revert InsufficientAllowance();
            allowance[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function _transfer(address from, address to, uint256 amount) private {
        if (to == address(0)) revert ZeroAddress();
        uint256 balance = balanceOf[from];
        if (balance < amount) revert InsufficientBalance();
        unchecked {
            balanceOf[from] = balance - amount;
        }
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }

    function _mint(address to, uint256 amount) private {
        if (to == address(0)) revert ZeroAddress();
        totalSupply += amount;
        balanceOf[to] += amount;
        emit Transfer(address(0), to, amount);
    }
}
