// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title IFlashRebalancer
/// @notice Callback implemented by searcher contracts that call `DarkPool.flashRebalance`.
interface IFlashRebalancer {
    /// @dev Called after the pool optimistically sent `amount0Out`/`amount1Out`. By the time this returns the
    ///      pool must hold enough tokens to satisfy the invariant AND the Treasury must have been paid.
    function onFlashRebalance(address initiator, uint256 amount0Out, uint256 amount1Out, bytes calldata data) external;
}
