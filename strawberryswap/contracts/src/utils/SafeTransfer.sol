// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20Min} from "../interfaces/IERC20Min.sol";

/// @dev Handles tokens that return no value / return false. Reverts on failure.
library SafeTransfer {
    error TransferFailed();

    function safeTransfer(IERC20Min t, address to, uint256 amt) internal {
        (bool ok, bytes memory r) = address(t).call(abi.encodeCall(IERC20Min.transfer, (to, amt)));
        if (!ok || (r.length != 0 && !abi.decode(r, (bool)))) revert TransferFailed();
    }

    function safeTransferFrom(IERC20Min t, address from, address to, uint256 amt) internal {
        (bool ok, bytes memory r) = address(t).call(abi.encodeCall(IERC20Min.transferFrom, (from, to, amt)));
        if (!ok || (r.length != 0 && !abi.decode(r, (bool)))) revert TransferFailed();
    }

    /// @dev Sets an allowance; callers use it as approve(x) ... approve(0) around a single call.
    function safeApprove(IERC20Min t, address spender, uint256 amt) internal {
        (bool ok, bytes memory r) = address(t).call(abi.encodeCall(IERC20Min.approve, (spender, amt)));
        if (!ok || (r.length != 0 && !abi.decode(r, (bool)))) revert TransferFailed();
    }
}
