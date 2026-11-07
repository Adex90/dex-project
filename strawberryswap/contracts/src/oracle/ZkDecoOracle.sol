// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IZKDecoOracle} from "../interfaces/IZKDecoOracle.sol";

/// @dev ABI of a snarkjs-generated Groth16 verifier for `circuits/zkdeco_oracle` (5 public inputs).
interface IGroth16Verifier5 {
    function verifyProof(uint256[2] calldata a, uint256[2][2] calldata b, uint256[2] calldata c, uint256[5] calldata pub)
        external
        view
        returns (bool);
}

/// @title ZkDecoOracle
/// @notice On-chain verifier for zk-DECO price attestations; implements `IZKDecoOracle` for `SyntheticRWA`.
///
/// WHAT `verifyAttestation` CHECKS (both must hold)
///   1. SNARK: a Groth16 proof of `circuits/zkdeco_oracle` for the public inputs
///        [providerId, queryCommitment, price, observedAt, bodyCommitment]
///      i.e. "a body committed to by `bodyCommitment` contains the hidden query behind `queryCommitment`
///      and that price/timestamp".
///   2. NOTARY: the registered notary of `providerId` signed (providerId, bodyCommitment), i.e. the TLS
///      plaintext committed to was really served by that provider.
///
/// ⚠ TRUST MODEL (honest statement). Until in-circuit TLS verification exists (see the circuit header), the
///   authenticity of the TLS session rests on the notary registered for each provider. A compromised notary
///   key can attest arbitrary bodies for ITS provider and nothing else. Mitigations designed in: the
///   registry is IMMUTABLE (set in the constructor; no admin can swap a notary), one notary per provider, and
///   SyntheticRWA's earliest-observation / window rules still apply. Use a threshold (TSS) notary key.
///
/// Proof wire format (exactly 353 bytes):
///   abi.encode(uint256[2] a, uint256[2][2] b, uint256[2] c)  [256 B]  ++  bodyCommitment [32 B]
///   ++ notarySignature r [32 B] ++ s [32 B] ++ v [1 B]
contract ZkDecoOracle is IZKDecoOracle {
    IGroth16Verifier5 public immutable verifier;
    /// @dev Gas forwarded to the SNARK verifier. Bounds the burn caused by malformed proofs (a failed EC
    ///      precompile consumes all forwarded gas). See JoinSplitVerifierAdapter for the rationale.
    uint256 public immutable verifierGas;
    mapping(bytes32 => address) public notaryOf; // providerId => notary signer (immutable after construction)

    /// @dev secp256k1 group order / 2: reject malleable (high-s) signatures.
    uint256 private constant HALF_N = 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;
    bytes32 private constant DOMAIN = keccak256("StrawberrySwap.zkdeco.notary.v1");

    error BadRegistry();

    constructor(IGroth16Verifier5 verifier_, uint256 verifierGas_, bytes32[] memory providerIds, address[] memory notaries) {
        if (
            address(verifier_) == address(0) || verifierGas_ < 300_000 || providerIds.length == 0
                || providerIds.length != notaries.length
        ) {
            revert BadRegistry();
        }
        verifier = verifier_;
        verifierGas = verifierGas_;
        for (uint256 i; i < providerIds.length; ++i) {
            if (providerIds[i] == bytes32(0) || notaries[i] == address(0) || notaryOf[providerIds[i]] != address(0)) {
                revert BadRegistry();
            }
            notaryOf[providerIds[i]] = notaries[i];
        }
    }

    /// @notice The digest a notary signs for a given body commitment (EIP-191 personal-sign over it).
    function notaryDigest(bytes32 providerId, bytes32 bodyCommitment) public pure returns (bytes32) {
        bytes32 inner = keccak256(abi.encode(DOMAIN, providerId, bodyCommitment));
        return keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", inner));
    }

    function verifyAttestation(Attestation calldata a, bytes calldata proof) external view returns (bool ok) {
        if (proof.length != 353) return false;
        address notary = notaryOf[a.providerId];
        if (notary == address(0)) return false; // unknown / deregistered provider

        bytes32 bodyCommitment = bytes32(proof[256:288]);
        // ---- 2. notary signature over (providerId, bodyCommitment)
        bytes32 r = bytes32(proof[288:320]);
        bytes32 s = bytes32(proof[320:352]);
        uint8 v = uint8(proof[352]);
        if ((v != 27 && v != 28) || uint256(s) > HALF_N) return false;
        address signer = ecrecover(notaryDigest(a.providerId, bodyCommitment), v, r, s);
        if (signer == address(0) || signer != notary) return false;

        // ---- 1. SNARK
        (uint256[2] memory pa, uint256[2][2] memory pb, uint256[2] memory pc) =
            abi.decode(proof[0:256], (uint256[2], uint256[2][2], uint256[2]));
        uint256[5] memory pub = [uint256(a.providerId), uint256(a.queryCommitment), a.price, uint256(a.observedAt), uint256(bodyCommitment)];
        try verifier.verifyProof{gas: verifierGas}(pa, pb, pc, pub) returns (bool res) {
            return res;
        } catch {
            return false;
        }
    }
}
