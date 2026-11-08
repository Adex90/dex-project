// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {SyntheticRWA} from "../src/derivatives/SyntheticRWA.sol";
import {IZKDecoOracle} from "../src/interfaces/IZKDecoOracle.sol";
import {IERC20Min} from "../src/interfaces/IERC20Min.sol";
import {ZkDecoOracle, IGroth16Verifier5} from "../src/oracle/ZkDecoOracle.sol";
import {OracleGroth16Verifier} from "./fixtures/OracleGroth16Verifier.dev.sol";
import {Fixtures} from "./fixtures/Fixtures.sol";
import {MockERC20} from "./mocks/Mocks.sol";

/// @notice End-to-end: a REAL zk-DECO attestation (Groth16 proof from circuits/zkdeco_oracle + notary
///         signature) settles a SyntheticRWA series and pays out the capped-index payoff.
contract ZkDecoIntegrationTest is Test {
    uint256 constant NOTARY_PK = 0xA11CE5;
    uint256 constant SECP_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    SyntheticRWA rwa;
    ZkDecoOracle oracle;
    MockERC20 usd;
    bytes32 id;
    address longHolder = address(0x10);
    address shortHolder = address(0x20);
    uint256 constant C = 1_000e18;
    uint256 constant CAP = 4_000e18;

    function setUp() public {
        OracleGroth16Verifier v = new OracleGroth16Verifier();
        bytes32[] memory ids = new bytes32[](1);
        address[] memory notaries = new address[](1);
        ids[0] = Fixtures.ORACLE_PROVIDER_ID;
        notaries[0] = vm.addr(NOTARY_PK);
        oracle = new ZkDecoOracle(IGroth16Verifier5(address(v)), 600_000, ids, notaries);
        rwa = new SyntheticRWA(IZKDecoOracle(address(oracle)));
        usd = new MockERC20("USD");

        vm.warp(uint256(Fixtures.ORACLE_OBSERVED_AT) - 7 days);
        id = rwa.createSeries(
            SyntheticRWA.SeriesParams({
                collateral: IERC20Min(address(usd)),
                collateralPerContract: uint128(C),
                cap: CAP,
                expiry: Fixtures.ORACLE_OBSERVED_AT,
                providerId: Fixtures.ORACLE_PROVIDER_ID,
                queryCommitment: Fixtures.ORACLE_QUERY_COMMITMENT
            })
        );
        usd.mint(address(this), 10 * C);
        usd.approve(address(rwa), type(uint256).max);
        rwa.mintPair(id, 10, longHolder, shortHolder);
    }

    // ------------------------------------------------------------------ helpers
    function _sign(bytes32 providerId, bytes32 bodyCommitment, uint256 pk) internal view returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, oracle.notaryDigest(providerId, bodyCommitment));
        return abi.encodePacked(r, s, v);
    }

    function _attestation() internal pure returns (IZKDecoOracle.Attestation memory a) {
        a = IZKDecoOracle.Attestation({
            providerId: Fixtures.ORACLE_PROVIDER_ID,
            queryCommitment: Fixtures.ORACLE_QUERY_COMMITMENT,
            price: Fixtures.ORACLE_PRICE,
            observedAt: Fixtures.ORACLE_OBSERVED_AT
        });
    }

    function _proof() internal view returns (bytes memory) {
        return abi.encodePacked(
            Fixtures.ORACLE_SNARK_PROOF, Fixtures.ORACLE_BODY_COMMITMENT,
            _sign(Fixtures.ORACLE_PROVIDER_ID, Fixtures.ORACLE_BODY_COMMITMENT, NOTARY_PK)
        );
    }

    function _atSettlement() internal {
        vm.warp(uint256(Fixtures.ORACLE_OBSERVED_AT) + 1);
    }

    // ------------------------------------------------------------------ happy path
    function test_realAttestationSettlesAndPaysCappedPayoff() public {
        _atSettlement();
        rwa.settle(id, _attestation(), _proof());
        vm.warp(uint256(Fixtures.ORACLE_OBSERVED_AT) + rwa.SETTLE_WINDOW());

        // price 2345.678912 < cap 4000: long gets C·S/K, short the exact complement
        uint256 longPer = C * Fixtures.ORACLE_PRICE / CAP;
        assertEq(rwa.longPayoutPerContract(id), longPer);

        vm.prank(longHolder);
        rwa.redeemLong(id, 10, longHolder);
        vm.prank(shortHolder);
        rwa.redeemShort(id, 10, shortHolder);
        assertEq(usd.balanceOf(longHolder), 10 * longPer);
        assertEq(usd.balanceOf(shortHolder), 10 * (C - longPer));
        assertEq(usd.balanceOf(address(rwa)), 0, "every wei of collateral paid out, none left behind");
    }

    // ------------------------------------------------------------------ attacks on the attestation
    function test_priceOffByOneWeiRejected() public {
        _atSettlement();
        IZKDecoOracle.Attestation memory a = _attestation();
        a.price += 1; // SNARK public input no longer matches the proven price
        bytes memory p = _proof(); // external calls inside args would consume expectRevert
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, a, p);
    }

    function test_timestampTamperedRejected() public {
        _atSettlement();
        IZKDecoOracle.Attestation memory a = _attestation();
        a.observedAt += 1;
        bytes memory p = _proof(); // external calls inside args would consume expectRevert
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, a, p);
    }

    function test_wrongQueryCommitmentRejectedBySeriesBinding() public {
        _atSettlement();
        IZKDecoOracle.Attestation memory a = _attestation();
        a.queryCommitment = bytes32(uint256(a.queryCommitment) ^ 1);
        bytes memory p = _proof(); // external calls inside args would consume expectRevert
        vm.expectRevert(SyntheticRWA.AttestationMismatch.selector); // series pins its hidden query
        rwa.settle(id, a, p);
    }

    function test_notarySignatureFromWrongKeyRejected() public {
        _atSettlement();
        bytes memory bad = abi.encodePacked(
            Fixtures.ORACLE_SNARK_PROOF, Fixtures.ORACLE_BODY_COMMITMENT,
            _sign(Fixtures.ORACLE_PROVIDER_ID, Fixtures.ORACLE_BODY_COMMITMENT, 0xBADBAD)
        );
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, _attestation(), bad);
    }

    function test_bodyCommitmentSwapRejected() public {
        _atSettlement();
        bytes32 other = bytes32(uint256(Fixtures.ORACLE_BODY_COMMITMENT) ^ 1);
        // attacker re-signs? no key: reuses the honest signature over a DIFFERENT commitment -> sig fails
        bytes memory p = abi.encodePacked(
            Fixtures.ORACLE_SNARK_PROOF, other, _sign(Fixtures.ORACLE_PROVIDER_ID, Fixtures.ORACLE_BODY_COMMITMENT, NOTARY_PK)
        );
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, _attestation(), p);
    }

    function test_validSnarkWithoutNotarySignatureIsNotEnough() public {
        _atSettlement();
        bytes memory p = abi.encodePacked(Fixtures.ORACLE_SNARK_PROOF, Fixtures.ORACLE_BODY_COMMITMENT, new bytes(65));
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, _attestation(), p);
    }

    function test_highSSignatureMalleabilityRejected() public {
        _atSettlement();
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(NOTARY_PK, oracle.notaryDigest(Fixtures.ORACLE_PROVIDER_ID, Fixtures.ORACLE_BODY_COMMITMENT));
        bytes memory flipped = abi.encodePacked(r, bytes32(SECP_N - uint256(s)), v == 27 ? uint8(28) : uint8(27));
        bytes memory p = abi.encodePacked(Fixtures.ORACLE_SNARK_PROOF, Fixtures.ORACLE_BODY_COMMITMENT, flipped);
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, _attestation(), p);
    }

    function testFuzz_tamperedSnarkByteRejected(uint256 pos, uint8 mask) public {
        _atSettlement();
        mask = mask == 0 ? 1 : mask;
        bytes memory p = _proof();
        p[pos % 256] ^= bytes1(mask); // only inside the SNARK part
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, _attestation(), p);
    }

    function testFuzz_wrongLengthProofRejected(bytes calldata junk) public {
        _atSettlement();
        vm.assume(junk.length != 353);
        vm.expectRevert(SyntheticRWA.InvalidProof.selector);
        rwa.settle(id, _attestation(), junk);
    }

    function test_oracleRegistryIsImmutableAndRejectsUnknownProvider() public view {
        assertEq(oracle.notaryOf(Fixtures.ORACLE_PROVIDER_ID), vm.addr(NOTARY_PK));
        assertEq(oracle.notaryOf(bytes32(uint256(1))), address(0));
    }

    function test_settleOutsideWindowReverts() public {
        bytes memory p = _proof();
        uint256 window = rwa.SETTLE_WINDOW();
        IZKDecoOracle.Attestation memory a = _attestation();
        vm.warp(uint256(Fixtures.ORACLE_OBSERVED_AT) - 1);
        vm.expectRevert(SyntheticRWA.SettlementNotYet.selector);
        rwa.settle(id, a, p);
        vm.warp(uint256(Fixtures.ORACLE_OBSERVED_AT) + window);
        vm.expectRevert(SyntheticRWA.SettlementWindowClosed.selector);
        rwa.settle(id, a, p);
    }
}
