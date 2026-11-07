// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @dev Storage-based guard (no transient storage: not every ZK-rollup supports EIP-1153 yet).
abstract contract ReentrancyGuard {
    uint256 private _status = 1;

    error Reentrancy();

    modifier nonReentrant() {
        if (_status == 2) revert Reentrancy();
        _status = 2;
        _;
        _status = 1;
    }
}
