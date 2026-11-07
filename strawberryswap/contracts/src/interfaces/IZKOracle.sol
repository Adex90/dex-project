// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title IZKOracle
/// @notice Price feed whose values are accepted ONLY with a valid zk-DECO / TLS-notary style proof that the
///         price was served by an authenticated HTTPS endpoint (selective disclosure: the API key and the
///         rest of the response never touch the chain). Verification happens inside the oracle contract.
interface IZKOracle {
    /// @return price1e18 token0 priced in token1, 18-decimals fixed point.
    /// @return updatedAt unix timestamp of the attested observation.
    function latestVerifiedPrice() external view returns (uint256 price1e18, uint64 updatedAt);
}
