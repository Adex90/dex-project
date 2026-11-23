// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title TestnetDisclosure
/// @notice TESTNET ONLY: the on-chain record of what this deployment is, and is not. Deployed last by
///         script/DeployTestnet.s.sol with every address of the deployment; the frontend's banner says the same and
///         links here.
contract TestnetDisclosure {
    string public constant DISCLOSURE = "StrawberrySwap on Arbitrum Sepolia is a TESTNET with a trusted bootstrap. "
        "Rothbard operates the ETHUSD price endpoint and the notary, so a price on the feed is only as honest as both. "
        "The zero-knowledge verifiers (joinSplitVerifier, batchVerifier, oracleVerifier) use DEVELOPMENT keys from a "
        "single-party setup: whoever holds that setup's secret could forge proofs. tWETH and tUSD are faucet tokens "
        "with no value. Not for mainnet value.";

    struct Deployment {
        address pool;
        address executor;
        address treasury;
        address feed;
        address notaryRegistry;
        address token0;
        address token1;
        address joinSplitVerifier;
        address batchVerifier;
        address oracleVerifier;
        address hasher;
    }

    address public immutable pool;
    address public immutable executor;
    address public immutable treasury;
    address public immutable feed;
    address public immutable notaryRegistry;
    address public immutable token0;
    address public immutable token1;
    /// @notice The three verifiers built from DEVELOPMENT keys (single-party setup).
    address public immutable joinSplitVerifier;
    address public immutable batchVerifier;
    address public immutable oracleVerifier;
    address public immutable hasher;

    constructor(Deployment memory d) {
        pool = d.pool;
        executor = d.executor;
        treasury = d.treasury;
        feed = d.feed;
        notaryRegistry = d.notaryRegistry;
        token0 = d.token0;
        token1 = d.token1;
        joinSplitVerifier = d.joinSplitVerifier;
        batchVerifier = d.batchVerifier;
        oracleVerifier = d.oracleVerifier;
        hasher = d.hasher;
    }

    /// @notice The verifiers whose keys come from a single-party development setup: never trust them with value.
    function devKeyVerifiers() external view returns (address[3] memory) {
        return [joinSplitVerifier, batchVerifier, oracleVerifier];
    }
}
