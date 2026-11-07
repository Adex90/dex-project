pragma circom 2.1.9;

include "circomlib/circuits/poseidon.circom";
include "circomlib/circuits/switcher.circom";

// Merkle inclusion for NoteTree (Poseidon(2), incremental binary tree).
// pathIndices[i] = 1 means the running node is the RIGHT child at level i  (bit 0 => hash(node, sibling)).
// Identical to the template inside joinsplit.circom; kept separate so joinsplit's audited artefacts
// (r1cs, dev keys, fixtures) stay byte-stable.
template MerkleInclusion(levels) {
    signal input leaf;
    signal input pathElements[levels];
    signal input pathIndices[levels];
    signal output root;

    component sw[levels];
    component h[levels];
    for (var i = 0; i < levels; i++) {
        pathIndices[i] * (pathIndices[i] - 1) === 0;
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
