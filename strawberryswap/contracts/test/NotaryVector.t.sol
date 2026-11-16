// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {ZkDecoOracle, IGroth16Verifier5} from "../src/oracle/ZkDecoOracle.sol";
import {OracleGroth16Verifier} from "./fixtures/OracleGroth16Verifier.dev.sol";
import {FeedFixtures} from "./fixtures/FeedFixtures.sol";

/// @notice The off-chain notary (strawberryswap/oracle, src/notary.rs) signs `ZkDecoOracle.notaryDigest`. This pins the
///         digest the Rust code computes for the first ETHUSD fixture, so that the two implementations cannot drift
///         apart: the same vector is asserted in `notary::tests::the_digest_is_zkdeco_oracles`.
contract NotaryVectorTest is Test {
    bytes32 constant RUST_DIGEST = 0x9d0788e643dfc235b812b96b689a3652ead96de3c8e94c5bb80ec9ee8ad2ee3b;

    function test_theRustNotaryDigestIsTheContracts() public {
        bytes32[] memory ids = new bytes32[](1);
        address[] memory notaries = new address[](1);
        ids[0] = FeedFixtures.PROVIDER_ID;
        notaries[0] = address(0xA11CE);
        ZkDecoOracle oracle =
            new ZkDecoOracle(IGroth16Verifier5(address(new OracleGroth16Verifier())), 600_000, ids, notaries);
        assertEq(oracle.notaryDigest(FeedFixtures.PROVIDER_ID, FeedFixtures.ethusd(0).bodyCommitment), RUST_DIGEST);
    }
}
