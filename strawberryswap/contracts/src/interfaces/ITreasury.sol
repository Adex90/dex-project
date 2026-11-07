// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title ITreasury
/// @notice Sink of MEV-internalization revenue. Revenue funds (a) gas subsidies for retail relayers and
///         (b) the protocol reserve. The pool measures payments by balance delta, this hook is informational.
interface ITreasury {
    function recordExtraction(address token, uint256 amount, address searcher) external;
}
