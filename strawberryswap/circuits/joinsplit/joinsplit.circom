pragma circom 2.1.9;

include "circomlib/circuits/poseidon.circom";
include "circomlib/circuits/bitify.circom";
include "circomlib/circuits/comparators.circom";
include "circomlib/circuits/switcher.circom";

/*
 * StrawberrySwap JoinSplit — the single circuit behind shield / transferZK / unshield
 * (Berrycoin, OptionsMarket) and depositNote / withdrawNote / transferNote (DarkPool).
 *
 * NOTE MODEL (must match contracts/src/privacy/NoteTree.sol byte-for-byte)
 *     pubKey     = Poseidon(privKey)
 *     commitment = Poseidon(assetId, value, pubKey, blinding)          leaf of the Merkle tree
 *     nullifier  = Poseidon(commitment, leafIndex, privKey)            published when the note is spent
 *     tree       = incremental binary tree, Poseidon(left, right), depth LEVELS
 *
 * PUBLIC INPUTS — the ORDER below is the order the on-chain verifier receives them. Do not reorder.
 *     [0] root  [1] nullifier0  [2] nullifier1  [3] commitment0  [4] commitment1
 *     [5] publicIn  [6] publicOut  [7] extDataHash  [8] assetId
 *
 * STATEMENT (what a valid proof convinces the verifier of — and, by zero-knowledge, nothing more)
 *   S1  Ownership    : the prover knows privKey_i for every input note.
 *   S2  Membership   : every input note with value != 0 is a leaf under `root`.
 *   S3  Nullifiers   : nullifier_i is derived from that same note, its leaf position and privKey_i, so a note
 *                      can be spent only under ONE nullifier; nullifier0 != nullifier1.
 *   S4  Conservation : Σ in.value + publicIn == Σ out.value + publicOut, with every term range-checked to
 *                      128 bits so the equation cannot be satisfied by wrapping modulo the field order.
 *   S5  Binding      : output commitments are well-formed notes of the SAME assetId; extDataHash is a public
 *                      input constrained below so recipient/relayer/ciphertext data cannot be re-targeted.
 *   Hidden: privKey, input/output values, blindings, recipients' pubKeys, the Merkle path (hence which leaf).
 *
 * DUMMY NOTES: an input with value == 0 skips the membership check (S2), which lets shield (no real inputs)
 *   and 1-input transfers use 2-in/2-out. A dummy still yields a nullifier from its own random
 *   (privKey, blinding), so nullifiers stay unique. A dummy carries zero value, so it can never mint money.
 */

// Merkle inclusion. pathIndices[i] = 1 means the running node is the RIGHT child at level i.
// Matches NoteTree._insert: bit 0 => hash(node, sibling); bit 1 => hash(sibling, node).
template MerkleInclusion(levels) {
    signal input leaf;
    signal input pathElements[levels];
    signal input pathIndices[levels];
    signal output root;

    component sw[levels];
    component h[levels];
    for (var i = 0; i < levels; i++) {
        pathIndices[i] * (pathIndices[i] - 1) === 0;      // each index is a bit
        sw[i] = Switcher();
        sw[i].sel <== pathIndices[i];
        sw[i].L <== i == 0 ? leaf : h[i - 1].out;
        sw[i].R <== pathElements[i];
        h[i] = Poseidon(2);
        h[i].inputs[0] <== sw[i].outL;
        h[i].inputs[1] <== sw[i].outR;
    }
    root <== h[levels - 1].out;
}

template InputNote(levels) {
    signal input assetId;
    signal input root;
    signal input nullifier;               // public, checked against the derived value
    signal input privKey;
    signal input value;
    signal input blinding;
    signal input pathElements[levels];
    signal input pathIndices[levels];

    component rc = Num2Bits(128);         // S4: range
    rc.in <== value;

    component pk = Poseidon(1);           // S1: ownership
    pk.inputs[0] <== privKey;

    component cm = Poseidon(4);
    cm.inputs[0] <== assetId;
    cm.inputs[1] <== value;
    cm.inputs[2] <== pk.out;
    cm.inputs[3] <== blinding;

    component mp = MerkleInclusion(levels);
    mp.leaf <== cm.out;
    for (var i = 0; i < levels; i++) {
        mp.pathElements[i] <== pathElements[i];
        mp.pathIndices[i] <== pathIndices[i];
    }

    component isDummy = IsZero();         // S2: membership only when value != 0
    isDummy.in <== value;
    component member = ForceEqualIfEnabled();
    member.enabled <== 1 - isDummy.out;
    member.in[0] <== root;
    member.in[1] <== mp.root;

    component idx = Bits2Num(levels);     // leaf position = the path bits themselves
    for (var i = 0; i < levels; i++) { idx.in[i] <== pathIndices[i]; }

    component nf = Poseidon(3);           // S3: nullifier
    nf.inputs[0] <== cm.out;
    nf.inputs[1] <== idx.out;
    nf.inputs[2] <== privKey;
    nf.out === nullifier;
}

template OutputNote() {
    signal input assetId;
    signal input commitment;              // public, checked against the derived value
    signal input pubKey;                  // recipient's (stealth) public key
    signal input value;
    signal input blinding;

    component rc = Num2Bits(128);         // S4: range
    rc.in <== value;

    component cm = Poseidon(4);           // S5: well-formed note, same assetId
    cm.inputs[0] <== assetId;
    cm.inputs[1] <== value;
    cm.inputs[2] <== pubKey;
    cm.inputs[3] <== blinding;
    cm.out === commitment;
}

template JoinSplit(levels) {
    // ---- public (order = on-chain order)
    signal input root;
    signal input nullifier[2];
    signal input commitment[2];
    signal input publicIn;
    signal input publicOut;
    signal input extDataHash;
    signal input assetId;

    // ---- private
    signal input inPrivKey[2];
    signal input inValue[2];
    signal input inBlinding[2];
    signal input inPathElements[2][levels];
    signal input inPathIndices[2][levels];
    signal input outPubKey[2];
    signal input outValue[2];
    signal input outBlinding[2];

    component ins[2];
    for (var i = 0; i < 2; i++) {
        ins[i] = InputNote(levels);
        ins[i].assetId <== assetId;
        ins[i].root <== root;
        ins[i].nullifier <== nullifier[i];
        ins[i].privKey <== inPrivKey[i];
        ins[i].value <== inValue[i];
        ins[i].blinding <== inBlinding[i];
        for (var j = 0; j < levels; j++) {
            ins[i].pathElements[j] <== inPathElements[i][j];
            ins[i].pathIndices[j] <== inPathIndices[i][j];
        }
    }

    component outs[2];
    for (var i = 0; i < 2; i++) {
        outs[i] = OutputNote();
        outs[i].assetId <== assetId;
        outs[i].commitment <== commitment[i];
        outs[i].pubKey <== outPubKey[i];
        outs[i].value <== outValue[i];
        outs[i].blinding <== outBlinding[i];
    }

    // S3: the two nullifiers differ (prevents spending one note twice inside a single join-split)
    component same = IsEqual();
    same.in[0] <== nullifier[0];
    same.in[1] <== nullifier[1];
    same.out === 0;

    // S4: public amounts are range-checked and value is conserved
    component rIn = Num2Bits(128);
    rIn.in <== publicIn;
    component rOut = Num2Bits(128);
    rOut.in <== publicOut;
    inValue[0] + inValue[1] + publicIn === outValue[0] + outValue[1] + publicOut;

    // S5: constrain extDataHash so it can never be optimised out of the proof (Tornado-style square trick).
    signal extSquare;
    extSquare <== extDataHash * extDataHash;
}

component main { public [root, nullifier, commitment, publicIn, publicOut, extDataHash, assetId] } = JoinSplit(20);
