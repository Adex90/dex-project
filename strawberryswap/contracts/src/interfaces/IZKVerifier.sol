// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title IZKVerifier
/// @notice Thin adapter over an auto-generated Groth16 / PLONK verifier (snarkjs, gnark, Noir…).
/// @dev The adapter MUST be `view`/`pure` (no state, no callbacks) and MUST revert-or-return-false on
///      any malformed proof. Every public input MUST already be reduced into the BN254 scalar field;
///      callers in this repo enforce that before calling.
interface IZKVerifier {
    function verifyProof(bytes calldata proof, uint256[] calldata publicInputs) external view returns (bool);
}
