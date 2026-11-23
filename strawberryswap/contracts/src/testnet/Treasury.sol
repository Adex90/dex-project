// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ITreasury} from "../interfaces/ITreasury.sol";
import {IERC20Min} from "../interfaces/IERC20Min.sol";
import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {SafeTransfer} from "../utils/SafeTransfer.sol";

/// @title Treasury (minimal)
/// @notice The sink of MEV-internalization revenue. DarkPool.flashRebalance pulls the Treasury's 80 % cut from the
///         searcher straight into this contract, then calls `recordExtraction`.
///         * Only pools the admin registered may record, so the books count real pool revenue only.
///         * Only the admin may withdraw: on the testnet the deployer, before mainnet an MPC/TSS vault. Revenue funds
///           retail gas subsidies and the protocol reserve.
///         Nothing else: no upgrade, no admin change, no arbitrary call.
contract Treasury is ITreasury, ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    address public immutable admin;
    mapping(address => bool) public isPool;
    /// @notice Revenue recorded per token, all pools together.
    mapping(address => uint256) public recorded;

    event PoolRegistered(address indexed pool);
    event Extraction(address indexed pool, address indexed token, uint256 amount, address indexed searcher);
    event Withdrawal(address indexed token, address indexed to, uint256 amount);

    error NotAdmin();
    error NotPool();
    error ZeroAddress();

    constructor(address admin_) {
        if (admin_ == address(0)) revert ZeroAddress();
        admin = admin_;
    }

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    function registerPool(address pool) external onlyAdmin {
        if (pool == address(0)) revert ZeroAddress();
        isPool[pool] = true;
        emit PoolRegistered(pool);
    }

    /// @inheritdoc ITreasury
    /// @dev The pool has already moved `amount` here (it measures by pulling, not by this call); this only records it.
    function recordExtraction(address token, uint256 amount, address searcher) external {
        if (!isPool[msg.sender]) revert NotPool();
        recorded[token] += amount;
        emit Extraction(msg.sender, token, amount, searcher);
    }

    function withdraw(IERC20Min token, address to, uint256 amount) external onlyAdmin nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        token.safeTransfer(to, amount);
        emit Withdrawal(address(token), to, amount);
    }
}
