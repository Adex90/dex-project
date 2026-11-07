// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IZKVerifier} from "../../src/interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../../src/interfaces/IPoseidonHasher.sol";
import {IZKOracle} from "../../src/interfaces/IZKOracle.sol";
import {ITreasury} from "../../src/interfaces/ITreasury.sol";
import {IFlashRebalancer} from "../../src/interfaces/IFlashRebalancer.sol";
import {IERC20Min} from "../../src/interfaces/IERC20Min.sol";

// =====================================================================================================
//  MOCK VERIFIERS — connect the Phase-1 contracts to a *controllable* proof system.
//
//  `MockVerifier` in ACCEPT mode models the WORST CASE for soundness: a verifier that accepts ANY proof
//  (broken trusted setup, circuit bug, or an attacker who can forge proofs). The protocol's second line of
//  defence (supply cap, custody underflow, curve invariant) must hold even then — that is what the
//  Foundry invariant suites prove. In REJECT mode it models "no valid proof exists".
//  For genuine proofs see test/JoinSplitIntegration.t.sol (real Groth16 verifier, real Poseidon).
// =====================================================================================================
contract MockVerifier is IZKVerifier {
    bool public accept;

    constructor(bool accept_) {
        accept = accept_;
    }

    function setAccept(bool a) external {
        accept = a;
    }

    function verifyProof(bytes calldata, uint256[] calldata) external view returns (bool) {
        return accept;
    }
}

/// @dev Accepts a proof only if the public inputs equal an expected vector set by the test: pins down the EXACT
///      layout DarkPool passes to the batch verifier (must equal the order declared in circuits/batch_swap).
contract ExpectingVerifier is IZKVerifier {
    uint256[] public expected;

    function expect(uint256[] calldata e) external {
        delete expected;
        for (uint256 i; i < e.length; ++i) expected.push(e[i]);
    }

    function verifyProof(bytes calldata, uint256[] calldata pub) external view returns (bool) {
        if (pub.length != expected.length) return false;
        for (uint256 i; i < pub.length; ++i) if (pub[i] != expected[i]) return false;
        return true;
    }
}

/// @dev Poseidon stand-in for fast fuzzing: keccak mod BN254 field. NOT the circuit's hash.
contract MockHasher is IPoseidonHasher {
    uint256 internal constant FIELD =
        21888242871839275222246405745257275088548364400416034343698204186575808495617;

    function poseidon(bytes32 l, bytes32 r) external pure returns (bytes32) {
        return bytes32(uint256(keccak256(abi.encode(l, r))) % FIELD);
    }
}

contract MockERC20 is IERC20Min {
    string public name;
    uint8 public override decimals = 18;
    mapping(address => uint256) public override balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    constructor(string memory n) {
        name = n;
    }

    function mint(address to, uint256 a) external {
        balanceOf[to] += a;
    }

    function approve(address s, uint256 a) external override returns (bool) {
        allowance[msg.sender][s] = a;
        return true;
    }

    function transfer(address to, uint256 a) external override returns (bool) {
        balanceOf[msg.sender] -= a;
        balanceOf[to] += a;
        return true;
    }

    function transferFrom(address f, address to, uint256 a) external override returns (bool) {
        uint256 al = allowance[f][msg.sender];
        if (al != type(uint256).max) allowance[f][msg.sender] = al - a;
        balanceOf[f] -= a;
        balanceOf[to] += a;
        return true;
    }
}

contract MockOracle is IZKOracle {
    uint256 public price;
    uint64 public updatedAt;

    function set(uint256 p) external {
        price = p;
        updatedAt = uint64(block.timestamp);
    }

    function latestVerifiedPrice() external view returns (uint256, uint64) {
        return (price, updatedAt);
    }
}

contract MockTreasury is ITreasury {
    uint256 public recorded;

    function recordExtraction(address, uint256 amount, address) external {
        recorded += amount;
    }
}

/// @dev Searcher stand-in: in the callback it sends fixed amounts back to the pool / Treasury from its own balance.
contract MockSearcher is IFlashRebalancer {
    IERC20Min public t0;
    IERC20Min public t1;
    address public pool;
    address public treasury;
    uint256 public pay0;
    uint256 public pay1;
    uint256 public payTreasury;

    constructor(IERC20Min a, IERC20Min b, address pool_, address treasury_) {
        (t0, t1, pool, treasury) = (a, b, pool_, treasury_);
    }

    function configure(uint256 p0, uint256 p1, uint256 pt) external {
        (pay0, pay1, payTreasury) = (p0, p1, pt);
    }

    function onFlashRebalance(address, uint256, uint256, bytes calldata) external {
        if (pay0 > 0) t0.transfer(pool, pay0);
        if (pay1 > 0) t1.transfer(pool, pay1);
        if (payTreasury > 0) t1.transfer(treasury, payTreasury);
    }

    function go(bytes calldata callData) external {
        (bool ok, bytes memory ret) = pool.call(callData);
        if (!ok) {
            assembly { revert(add(ret, 32), mload(ret)) }
        }
    }
}
