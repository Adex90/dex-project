// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title IZKDecoOracle
/// @notice Verifier of zero-knowledge proofs that a number was served over TLS by a specific, registered
///         data provider (DECO / zk-DECO / TLSNotary family). The consumer never trusts the relayer that
///         submits the data. What it DOES trust depends on the implementation: the proof system, the provider
///         registry and — in the current `ZkDecoOracle` — a registered notary per provider (see its trust
///         model). In-circuit TLS verification would remove the notary assumption.
///
/// HOW A PRICE REACHES THE CHAIN
///   1. A *prover* (any relayer, no permission needed) opens a normal HTTPS session to the provider's
///      public API. In a 3-party handshake the key material is secret-shared with a *verifier* so the
///      prover cannot forge the transcript; no cooperation from the provider is required or possible.
///   2. The prover builds a SNARK proving, WITHOUT revealing the transcript or credentials:
///        * the session's server certificate chains to the key registered under `providerId`;
///        * the request followed the template committed in `queryCommitment`
///            = Poseidon(ticker/endpoint template, salt);
///        * the authenticated response body contained `price` and `observedAt` at the claimed offsets.
///      Private witnesses: API key, full request, full response, TLS keys, commitment salt.
///   3. The proof + the public tuple below is submitted on-chain; `verifyAttestation` checks it.
///
/// WHAT IS (AND IS NOT) HIDDEN — precision matters:
///   * Hidden from the CHAIN / public observers: the query (ticker, endpoint, parameters), the API key
///     and the rest of the response. Only an opaque `queryCommitment` appears on-chain, so nobody can tell
///     which instrument a position tracks from contract data alone.
///   * Hidden from the PROVIDER: that the data is being used on-chain, who consumes it, and which
///     contract/address it feeds. The provider sees an ordinary client request.
///   * NOT hideable from the provider: the request itself. A server that answers a query necessarily sees
///     it. To limit metadata (IP, timing, query patterns) provers should use rotating relays/Tor and a
///     shared provider account; that is an off-chain operational requirement of the Automator network,
///     not something a smart contract can enforce.
interface IZKDecoOracle {
    /// @dev Public statement proven by the SNARK. `price` is 18-decimal fixed point (index units).
    struct Attestation {
        bytes32 providerId; // registry key: hash of (domain, TLS pubkey root) of the data provider
        bytes32 queryCommitment; // Poseidon commitment to the hidden request template
        uint256 price; // value extracted from the authenticated response, 1e18 scale
        uint64 observedAt; // timestamp the provider's response asserts (from the signed body / Date)
    }

    /// @return ok true iff `proof` is valid for `a` AND `a.providerId` is an active registered provider.
    /// MUST be `view` (no state change, no callbacks): consumers call it under a reentrancy guard and
    /// rely on it being a STATICCALL.
    function verifyAttestation(Attestation calldata a, bytes calldata proof) external view returns (bool ok);
}
