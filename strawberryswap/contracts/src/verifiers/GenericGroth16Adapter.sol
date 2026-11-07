// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IZKVerifier} from "../interfaces/IZKVerifier.sol";

/// @title GenericGroth16Adapter
/// @notice Connects ANY snarkjs-generated Groth16 verifier — whatever its number of public inputs — to the generic
///         `IZKVerifier`. Needed for `batch_swap`, whose input count is 8 + 2·N and so changes with the batch size.
///
/// snarkjs verifiers expose `verifyProof(uint256[2] a, uint256[2][2] b, uint256[2] c, uint256[K] pub)`. Every
/// argument is a STATIC type, so the ABI encoding is just the words concatenated: the adapter builds
/// `selector ‖ proof(256 B) ‖ pub[0] ‖ … ‖ pub[K-1]` with the selector derived from K at construction, and makes one
/// gas-capped STATICCALL. Same wire format and fail-closed behaviour as `JoinSplitVerifierAdapter`
/// (see its note on why the gas cap is mandatory).
contract GenericGroth16Adapter is IZKVerifier {
    address public immutable verifier;
    uint256 public immutable nPublic;
    uint256 public immutable gasCap;
    bytes4 public immutable selector;

    error BadConfig();

    constructor(address verifier_, uint256 nPublic_, uint256 gasCap_) {
        if (verifier_ == address(0) || nPublic_ == 0 || nPublic_ > 512 || gasCap_ < 300_000) revert BadConfig();
        verifier = verifier_;
        nPublic = nPublic_;
        gasCap = gasCap_;
        selector = bytes4(
            keccak256(abi.encodePacked("verifyProof(uint256[2],uint256[2][2],uint256[2],uint256[", _toString(nPublic_), "])"))
        );
    }

    function verifyProof(bytes calldata proof, uint256[] calldata pub) external view returns (bool) {
        if (proof.length != 256 || pub.length != nPublic) return false;
        (bool ok, bytes memory ret) = verifier.staticcall{gas: gasCap}(abi.encodePacked(selector, proof, pub));
        return ok && ret.length == 32 && abi.decode(ret, (bool));
    }

    function _toString(uint256 v) private pure returns (string memory) {
        if (v == 0) return "0";
        uint256 digits;
        for (uint256 t = v; t != 0; t /= 10) ++digits;
        bytes memory out = new bytes(digits);
        while (v != 0) {
            out[--digits] = bytes1(uint8(48 + (v % 10)));
            v /= 10;
        }
        return string(out);
    }
}
