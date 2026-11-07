pragma circom 2.1.9;

include "circomlib/circuits/poseidon.circom";
include "circomlib/circuits/bitify.circom";
include "circomlib/circuits/comparators.circom";
include "../lib/merkle.circom";

/*
 * StrawberrySwap BatchSwap — aggregates N shielded swap intents into ONE net state transition for
 * DarkPool.settleBatch, at a single uniform clearing price derived from the AMM curve.
 *
 * ═══ THE INTENT NOTE (how users stay in custody of their keys) ═══════════════════════════════════
 *  A user first spends one of their own notes with an ordinary JoinSplit (client side, with their key) into an
 *  INTENT NOTE:
 *        commitment = Poseidon(assetIn, value, BATCH_PUBKEY, intentBlinding)
 *        intentBlinding = Poseidon(destPubKey, refundPubKey, destBlinding, minOut, deadline)
 *  BATCH_PUBKEY is a constant with no known Poseidon preimage, so NO ONE can spend an intent note with a JoinSplit
 *  (that circuit requires Poseidon(privKey) == pubKey). The ONLY way to spend it is this circuit, which can only
 *  pay `destPubKey` at least `minOut` (before the deadline) or return the funds to `refundPubKey` (after it).
 *  The Automator therefore learns the intent but can never redirect or keep the funds.
 *  Nullifier of an intent note: Poseidon(commitment, leafIndex, destBlinding). destBlinding is bound inside the
 *  commitment (through intentBlinding), so each note has exactly one nullifier; it is known only to the user and to
 *  whoever holds the opening, so an observer cannot link a spent intent to the transaction that created it. (A
 *  PUBLIC nullifier key made every batch slot linkable to its creator: Phase-8 audit H-7.)
 *
 * ═══ PRICE: closed form, no solver ═════════════════════════════════════════════════════════════════
 *  Let X, Y be the effective reserves, A0/A1 the gross amounts of token0/token1 SOLD into the pool, and
 *      q = (Y + A1) / (X + A0).
 *  If every token0-seller is paid s·q token1 and every token1-seller s/q token0, the pool ends at
 *      X' = Y/q,  Y' = q·X   ⇒   X'·Y' = X·Y   EXACTLY (zero fee, K preserved).
 *  With integers: pLo = ⌊WAD·(Y+A1)/(X+A0)⌋ pays token0-sellers; pHi = pLo+1 pays token1-sellers. Both round
 *  AGAINST the user (≤ 1e-18 relative), so K' >= K always holds and DarkPool's on-chain K check never trips on a
 *  well-formed batch. The price is a FUNCTION OF THE PUBLIC DATA (X, Y, nets): the prover has no freedom.
 *
 * ═══ PRIVACY BOUNDARY ═════════════════════════════════════════════════════════════════════════════
 *  Public: root, tNow, X, Y, the NET flow per asset, one opaque nullifier and one opaque output commitment per
 *  intent. Private: every individual size, owner, destination, minOut, deadline, and the GROSS volumes A0/A1/O0/O1
 *  (only their per-asset net is revealed). Leaks that remain, by arithmetic: the net imbalance; and in a batch with
 *  a single real intent, that intent's size. The Automator enforces a minimum batch size.
 *
 * ═══ PUBLIC INPUTS — ORDER = DarkPool.settleBatch ═════════════════════════════════════════════════════
 *  [0] root  [1] tNow  [2] X  [3] Y  [4] netIn0  [5] netIn1  [6] netOut0  [7] netOut1
 *  [8 .. 8+N)  nullifier[i]      [8+N .. 8+2N)  commitment[i]       (0 in padding slots)
 *  netInK = value the POOL receives of asset K, netOutK = value the pool pays (at most one is non-zero).
 *  tNow is a coarse clock supplied by the prover; the contract requires tNow <= block.timestamp <= tNow + window.
 *
 * ═══ STATEMENT per slot ═══════════════════════════════════════════════════════════════════════════
 *  mode ∈ {padding, swap, refund} (one-hot bits).  Active slots prove:
 *   S1 the intent note exists under `root` (Merkle) and its nullifier is correct;
 *   S2 swap  : tNow <= deadline, payout o = ⌊value·num/den⌋ (division verified with remainder), o >= minOut,
 *              output = Poseidon(1-dir, o, destPubKey, destBlinding);
 *   S3 refund: tNow > deadline,  output = Poseidon(dir, value, refundPubKey, destBlinding)  (same asset, full value);
 *   S4 padding: nullifier = commitment = 0.
 *  Refund slots contribute nothing to the aggregates (custody <-> custody), so they cannot move the price.
 *
 * KNOWN LIMITS: duplicate use of one intent note inside a batch is rejected by the contract's nullifier set, not here
 * (an N² in-circuit comparison would cost more than the whole price logic).
 */

// ---- constants (keccak256(label) mod BN254 field — nothing-up-my-sleeve; see circuits/test/batch.lib.mjs)
function BATCH_PUBKEY() { return 133414396539581031856283141735652113376156359968225558936463889054923919661; }
function WAD()          { return 1000000000000000000; }

template BatchSlot(levels) {
    // ---- shared context
    signal input root;
    signal input tNow;
    signal input pLo;
    signal input pHi;
    // ---- public per-slot values
    signal input nullifier;
    signal input commitment;
    // ---- private witness
    signal input isSwap;
    signal input isRefund;
    signal input dir;             // asset sold into the pool: 0 = token0, 1 = token1
    signal input value;
    signal input outValue;        // payout o (swap mode)
    signal input rem;             // division remainder
    signal input destPubKey;
    signal input refundPubKey;
    signal input destBlinding;
    signal input minOut;
    signal input deadline;
    signal input pathElements[levels];
    signal input pathIndices[levels];
    // ---- gross contributions to the aggregates (private; swap slots only)
    signal output a0;             // token0 sold
    signal output a1;             // token1 sold
    signal output o0;             // token0 paid out
    signal output o1;             // token1 paid out

    // mode bits
    isSwap * (isSwap - 1) === 0;
    isRefund * (isRefund - 1) === 0;
    dir * (dir - 1) === 0;
    isSwap * isRefund === 0;
    signal active;
    active <== isSwap + isRefund;

    // ranges (keep every product far below the field order)
    component rV = Num2Bits(112);  rV.in <== value;
    component rO = Num2Bits(112);  rO.in <== outValue;
    component rM = Num2Bits(112);  rM.in <== minOut;
    component rD = Num2Bits(64);   rD.in <== deadline;
    component rR = Num2Bits(128);  rR.in <== rem;
    component vz = IsZero();       vz.in <== value;
    active * vz.out === 0;        // an active slot moves a non-zero amount

    // S1: the intent note
    component ib = Poseidon(5);
    ib.inputs[0] <== destPubKey;
    ib.inputs[1] <== refundPubKey;
    ib.inputs[2] <== destBlinding;
    ib.inputs[3] <== minOut;
    ib.inputs[4] <== deadline;
    component cmIn = Poseidon(4);
    cmIn.inputs[0] <== dir;
    cmIn.inputs[1] <== value;
    cmIn.inputs[2] <== BATCH_PUBKEY();
    cmIn.inputs[3] <== ib.out;
    component mp = MerkleInclusion(levels);
    mp.leaf <== cmIn.out;
    for (var i = 0; i < levels; i++) { mp.pathElements[i] <== pathElements[i]; mp.pathIndices[i] <== pathIndices[i]; }
    component member = ForceEqualIfEnabled();
    member.enabled <== active;
    member.in[0] <== root;
    member.in[1] <== mp.root;
    component idx = Bits2Num(levels);
    for (var i = 0; i < levels; i++) { idx.in[i] <== pathIndices[i]; }
    component nf = Poseidon(3);
    nf.inputs[0] <== cmIn.out;
    nf.inputs[1] <== idx.out;
    nf.inputs[2] <== destBlinding;                       // secret nullifier key [audit H-7]
    nullifier === active * nf.out;                       // S4: padding => 0

    // deadline vs clock
    component expired = LessThan(64);
    expired.in[0] <== deadline;
    expired.in[1] <== tNow;                               // expired = deadline < tNow
    isSwap * expired.out === 0;                           // S2: swaps only before the deadline
    isRefund * (1 - expired.out) === 0;                   // S3: refunds only after it

    // S2: o = floor(value * num / den) with (num, den) = dir ? (WAD, pHi) : (pLo, WAD)
    signal num;
    signal den;
    num <== pLo + dir * (WAD() - pLo);
    den <== WAD() + dir * (pHi - WAD());
    signal lhs;
    signal rhs;
    lhs <== value * num;
    rhs <== outValue * den;
    isSwap * (lhs - rhs - rem) === 0;                     // value*num = o*den + rem
    component remLt = LessThan(128);
    remLt.in[0] <== rem;
    remLt.in[1] <== den;
    isSwap * (1 - remLt.out) === 0;                       // 0 <= rem < den  ⇒  o is exactly the floor
    component ge = LessEqThan(112);
    ge.in[0] <== minOut;
    ge.in[1] <== outValue;
    isSwap * (1 - ge.out) === 0;                          // user's slippage bound

    // output note
    signal outAsset;
    outAsset <== dir + isSwap * (1 - 2 * dir);            // swap: 1-dir, refund: dir
    signal sv;
    signal rv;
    sv <== isSwap * outValue;
    rv <== isRefund * value;
    signal pd;
    signal pr;
    pd <== isSwap * destPubKey;
    pr <== isRefund * refundPubKey;
    component cmOut = Poseidon(4);
    cmOut.inputs[0] <== outAsset;
    cmOut.inputs[1] <== sv + rv;
    cmOut.inputs[2] <== pd + pr;
    cmOut.inputs[3] <== destBlinding;
    commitment === active * cmOut.out;

    // aggregates
    signal f0;
    signal f1;
    f0 <== isSwap * (1 - dir);
    f1 <== isSwap * dir;
    a0 <== f0 * value;
    a1 <== f1 * value;
    o1 <== f0 * outValue;
    o0 <== f1 * outValue;
}

template BatchSwap(levels, N) {
    // ---- public (order = on-chain order)
    signal input root;
    signal input tNow;
    signal input X;
    signal input Y;
    signal input netIn0;
    signal input netIn1;
    signal input netOut0;
    signal input netOut1;
    signal input nullifier[N];
    signal input commitment[N];

    // ---- private
    signal input isSwap[N];
    signal input isRefund[N];
    signal input dir[N];
    signal input value[N];
    signal input outValue[N];
    signal input rem[N];
    signal input destPubKey[N];
    signal input refundPubKey[N];
    signal input destBlinding[N];
    signal input minOut[N];
    signal input deadline[N];
    signal input pathElements[N][levels];
    signal input pathIndices[N][levels];
    signal input pLo;
    signal input pRem;

    // ---- public-value ranges
    component rT = Num2Bits(64);  rT.in <== tNow;
    component rX = Num2Bits(112); rX.in <== X;
    component rY = Num2Bits(112); rY.in <== Y;
    component r1 = Num2Bits(112); r1.in <== netIn0;
    component r2 = Num2Bits(112); r2.in <== netIn1;
    component r3 = Num2Bits(112); r3.in <== netOut0;
    component r4 = Num2Bits(112); r4.in <== netOut1;
    netIn0 * netOut0 === 0;                         // net flow has one direction per asset
    netIn1 * netOut1 === 0;

    // ---- slots + private gross sums
    component slot[N];
    signal sA0[N + 1]; signal sA1[N + 1]; signal sO0[N + 1]; signal sO1[N + 1];
    sA0[0] <== 0; sA1[0] <== 0; sO0[0] <== 0; sO1[0] <== 0;
    signal pHi;
    pHi <== pLo + 1;
    for (var i = 0; i < N; i++) {
        slot[i] = BatchSlot(levels);
        slot[i].root <== root;
        slot[i].tNow <== tNow;
        slot[i].pLo <== pLo;
        slot[i].pHi <== pHi;
        slot[i].nullifier <== nullifier[i];
        slot[i].commitment <== commitment[i];
        slot[i].isSwap <== isSwap[i];
        slot[i].isRefund <== isRefund[i];
        slot[i].dir <== dir[i];
        slot[i].value <== value[i];
        slot[i].outValue <== outValue[i];
        slot[i].rem <== rem[i];
        slot[i].destPubKey <== destPubKey[i];
        slot[i].refundPubKey <== refundPubKey[i];
        slot[i].destBlinding <== destBlinding[i];
        slot[i].minOut <== minOut[i];
        slot[i].deadline <== deadline[i];
        for (var j = 0; j < levels; j++) {
            slot[i].pathElements[j] <== pathElements[i][j];
            slot[i].pathIndices[j] <== pathIndices[i][j];
        }
        sA0[i + 1] <== sA0[i] + slot[i].a0;
        sA1[i + 1] <== sA1[i] + slot[i].a1;
        sO0[i + 1] <== sO0[i] + slot[i].o0;
        sO1[i + 1] <== sO1[i] + slot[i].o1;
    }

    // ---- net flow = gross in - gross out (only the NET is public)
    netIn0 - netOut0 === sA0[N] - sO0[N];
    netIn1 - netOut1 === sA1[N] - sO1[N];

    // ---- clearing price: pLo = floor(WAD * (Y + A1) / (X + A0)), remainder verified
    signal D;
    signal Nn;
    D <== X + sA0[N];
    Nn <== Y + sA1[N];
    component dz = IsZero();       dz.in <== D;   dz.out === 0;
    component rPl = Num2Bits(128); rPl.in <== pLo;
    component rPr = Num2Bits(120); rPr.in <== pRem;
    component rDd = Num2Bits(120); rDd.in <== D;               // D < 2^120 (X,A0 < 2^112 each, N <= 2^8)
    signal prod;
    prod <== pLo * D;
    prod + pRem === WAD() * Nn;                                 // WAD*Nn = pLo*D + pRem
    component pRemLt = LessThan(120);
    pRemLt.in[0] <== pRem;
    pRemLt.in[1] <== D;
    pRemLt.out === 1;                                           // 0 <= pRem < D  ⇒  pLo is exactly the floor
}
