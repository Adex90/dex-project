// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20Min} from "../interfaces/IERC20Min.sol";
import {IZKOracle} from "../interfaces/IZKOracle.sol";

/// @notice The views of DarkPool the engine reads.
interface IDarkPoolLens {
    function oracle() external view returns (IZKOracle);
    function token0() external view returns (IERC20Min);
    function token1() external view returns (IERC20Min);
    function reserve0() external view returns (uint128);
    function reserve1() external view returns (uint128);
    function virtual0() external view returns (uint256);
    function virtual1() external view returns (uint256);
}

/// @title EngineLens
/// @notice The HFT engine's whole tick read, in one `eth_call` at one block. NEVER DEPLOYED: the engine sends this
///         contract's creation code, with its constructor arguments, as an `eth_call` without a `to`. The constructor
///         reads everything and returns the encoded snapshot as the would-be runtime code, which is what `eth_call`
///         returns. One execution means the oracle reading, the pool's reserves and the executor's balances all come
///         from the same block; a batch of separate `eth_call`s could straddle two (Arbitrum makes one every 250 ms).
/// @dev The snapshot is eleven words: block number, timestamp and base fee; the oracle's price and observation time,
///      read through the pool as `DarkPool` reads it; the pool's reserves and virtual reserves; the executor's
///      token0 and token1 balances. It starts with the block number, so its first byte is never 0xEF, which EIP-3541
///      would reject as code. The engine pins this contract's creation code (bots/hft-engine-rust/src/lens.hex), and
///      CI checks the pin against this source's build.
contract EngineLens {
    constructor(IDarkPoolLens pool, address executor) {
        (uint256 price, uint64 updatedAt) = pool.oracle().latestVerifiedPrice();
        bytes memory snapshot = abi.encode(
            block.number,
            block.timestamp,
            block.basefee,
            price,
            uint256(updatedAt),
            uint256(pool.reserve0()),
            uint256(pool.reserve1()),
            pool.virtual0(),
            pool.virtual1(),
            pool.token0().balanceOf(executor),
            pool.token1().balanceOf(executor)
        );
        assembly ("memory-safe") {
            return(add(snapshot, 32), mload(snapshot))
        }
    }
}
