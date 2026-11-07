// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {SafeTransfer} from "../utils/SafeTransfer.sol";
import {IERC20Min} from "../interfaces/IERC20Min.sol";
import {IFlashRebalancer} from "../interfaces/IFlashRebalancer.sol";

interface IDarkPoolSearcher {
    function flashRebalance(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
    function token0() external view returns (IERC20Min);
    function token1() external view returns (IERC20Min);
    function treasury() external view returns (address);
}

/// @title SearcherExecutor
/// @notice The on-chain half of the Searcher bot: the contract `DarkPool.flashRebalance` calls back.
///
/// INVENTORY MODEL (why this contract holds tokens): the bot does not need an external venue inside the
/// transaction. It rebalances the pool against its OWN pre-funded inventory (the pool hands out one token,
/// the executor returns the other at the curve price; the pool then pulls the Treasury's cut) and hedges the resulting
/// inventory off-chain. Managing that inventory is exactly what the Avellaneda–Stoikov module does.
///
/// SECURITY
///   * `rebalance` is operator-only (operator = MPC/TSS-controlled address; the bot never holds a hot key
///     with more authority than "trade within this contract").
///   * The callback is accepted ONLY from the pool, ONLY while a `rebalance` this contract started is in
///     flight, and ONLY if the pool says this contract is the initiator: nobody can invoke it to drain funds.
///   * Repayment amounts come from the operator's calldata, but the POOL enforces the invariant and the oracle
///     deviation rule, and PULLS its exact 80 % Treasury cut. `treasuryPay` is the allowance granted for that
///     pull (reset to 0 afterwards): a cap, so a stale plan reverts instead of over-paying.
///   * `sweep` lets the operator pull inventory out; there is no other exit and no third-party access.
contract SearcherExecutor is IFlashRebalancer, ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    address public immutable operator;
    IDarkPoolSearcher public immutable pool;
    IERC20Min public immutable token0;
    IERC20Min public immutable token1;
    address public immutable treasury;

    bool private _inFlight;

    error NotOperator();
    error NotPool();
    error NotInitiator();
    error NoActiveRebalance();
    error ZeroAddress();

    constructor(address operator_, IDarkPoolSearcher pool_) {
        if (operator_ == address(0) || address(pool_) == address(0)) revert ZeroAddress();
        operator = operator_;
        pool = pool_;
        token0 = pool_.token0();
        token1 = pool_.token1();
        treasury = pool_.treasury();
    }

    /// @param pay0 token0 returned to the pool;  @param pay1 token1 returned to the pool;
    /// @param treasuryPay token1 paid to the Treasury (must be >= 80 % of the harvest, else the pool reverts).
    function rebalance(uint256 amount0Out, uint256 amount1Out, uint256 pay0, uint256 pay1, uint256 treasuryPay)
        external
        nonReentrant
    {
        if (msg.sender != operator) revert NotOperator();
        _inFlight = true;
        token1.approve(address(pool), treasuryPay); // [audit H-1]: the pool pulls its cut; this caps it (max-pay guard)
        pool.flashRebalance(amount0Out, amount1Out, address(this), abi.encode(pay0, pay1, treasuryPay));
        token1.approve(address(pool), 0);
        _inFlight = false;
    }

    function onFlashRebalance(address initiator, uint256, uint256, bytes calldata data) external override {
        if (msg.sender != address(pool)) revert NotPool();
        if (!_inFlight) revert NoActiveRebalance();
        if (initiator != address(this)) revert NotInitiator();

        (uint256 pay0, uint256 pay1, uint256 treasuryPay) = abi.decode(data, (uint256, uint256, uint256));
        if (pay0 > 0) token0.safeTransfer(address(pool), pay0);
        if (pay1 > 0) token1.safeTransfer(address(pool), pay1);
    }

    /// @notice Operator withdraws inventory (their own capital).
    function sweep(IERC20Min token, address to, uint256 amount) external nonReentrant {
        if (msg.sender != operator) revert NotOperator();
        if (to == address(0)) revert ZeroAddress();
        token.safeTransfer(to, amount);
    }
}
