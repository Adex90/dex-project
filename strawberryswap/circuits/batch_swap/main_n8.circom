pragma circom 2.1.9;
include "batch_swap.circom";
// public inputs: 8 + 2*8 = 24
component main { public [root, tNow, X, Y, netIn0, netIn1, netOut0, netOut1, nullifier, commitment] } = BatchSwap(20, 8);
