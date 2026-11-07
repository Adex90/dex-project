// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {Berrycoin} from "../src/tokens/Berrycoin.sol";
import {NoteTree} from "../src/privacy/NoteTree.sol";
import {MockVerifier, MockHasher} from "./mocks/Mocks.sol";

/// @dev Random-walk handler. The verifier ACCEPTS EVERY PROOF, so every call below behaves as if the
///      attacker could forge proofs at will. Unshield amounts are drawn from [1, 2·MAX_SUPPLY] so most
///      attempts are overdrafts that MUST revert.
contract BerryHandler is Test {
    uint256 constant FIELD = 21888242871839275222246405745257275088548364400416034343698204186575808495617;
    Berrycoin public berry;
    address[] public actors;
    uint256 public ghostShielded;
    uint256 public overdraftSucceeded; // must remain 0
    uint256 public nonce;
    uint256 public shields;
    uint256 public unshields;
    uint256 public overdraftsBlocked;

    constructor(Berrycoin b, address[] memory a) {
        berry = b;
        actors = a;
    }

    function _actor(uint256 s) internal view returns (address) {
        return actors[s % actors.length];
    }

    function _js() internal returns (NoteTree.JoinSplit memory js) {
        js.proof = hex"01";
        js.root = berry.currentRoot();
        js.nullifiers = [bytes32(uint256(keccak256(abi.encode("n", ++nonce))) % FIELD), bytes32(uint256(keccak256(abi.encode("n", ++nonce))) % FIELD)];
        js.commitments = [bytes32(uint256(keccak256(abi.encode("c", ++nonce))) % FIELD), bytes32(uint256(keccak256(abi.encode("c", ++nonce))) % FIELD)];
        js.encryptedNotes = [bytes(""), bytes("")];
    }

    function transfer(uint256 fromSeed, uint256 toSeed, uint256 amt) external {
        address from = _actor(fromSeed);
        amt = bound(amt, 0, berry.balanceOf(from));
        vm.prank(from);
        berry.transfer(_actor(toSeed), amt);
    }

    function shield(uint256 seed, uint256 amt) external {
        address a = _actor(seed);
        uint256 bal = berry.balanceOf(a);
        if (bal == 0) return;
        amt = bound(amt, 1, bal);
        vm.prank(a);
        berry.shield(amt, _js());
        ghostShielded += amt;
        shields++;
    }

    function transferZK() external {
        berry.transferZK(_js());
    }

    function unshield(uint256 seed, uint256 amt) external {
        amt = bound(amt, 1, berry.MAX_SUPPLY() * 2);
        uint256 before = berry.shieldedSupply();
        address to = _actor(seed);
        vm.prank(to);
        try berry.unshield(amt, to, _js()) {
            if (amt > before) overdraftSucceeded++;
            ghostShielded -= amt;
            unshields++;
        } catch {
            if (amt > before) overdraftsBlocked++;
        }
    }
}

contract BerrycoinInvariantTest is StdInvariant, Test {
    Berrycoin berry;
    BerryHandler handler;
    address[] actors;

    function setUp() public {
        MockVerifier verifier = new MockVerifier(true); // forged proofs accepted
        MockHasher hasher = new MockHasher();
        address genesis = address(0xBEEF);
        berry = new Berrycoin(genesis, verifier, hasher);
        actors.push(genesis);
        actors.push(address(0xA11CE));
        actors.push(address(0xB0B));
        actors.push(address(0xCA401));
        handler = new BerryHandler(berry, actors);
        targetContract(address(handler));
    }

    /// THE THEOREM: no sequence of calls — even with a verifier that accepts forged proofs — can make
    /// the sum of public balances plus shielded value differ from MAX_SUPPLY.
    function invariant_supplyConservation() public view {
        uint256 sum;
        for (uint256 i; i < actors.length; ++i) sum += berry.balanceOf(actors[i]);
        sum += berry.balanceOf(address(berry));
        assertEq(sum + berry.shieldedSupply(), berry.MAX_SUPPLY());
    }

    function invariant_totalSupplyIsConstantAndCapped() public view {
        assertEq(berry.totalSupply(), berry.MAX_SUPPLY());
        assertLe(berry.shieldedSupply(), berry.MAX_SUPPLY());
    }

    /// No unshield larger than what was shielded ever succeeded.
    function invariant_noOverdraftEverSucceeds() public view {
        assertEq(handler.overdraftSucceeded(), 0);
    }

    /// The contract's shielded counter equals the handler's independent bookkeeping.
    function invariant_shieldedLedgerMatchesGhost() public view {
        assertEq(berry.shieldedSupply(), handler.ghostShielded());
    }
}
