pragma circom 2.1.9;

include "circomlib/circuits/poseidon.circom";
include "circomlib/circuits/bitify.circom";
include "circomlib/circuits/comparators.circom";

/*
 * StrawberrySwap zk-DECO price-oracle circuit — SKELETON
 *
 * Proves, about a private HTTPS response body, that it contains a price and timestamp for a HIDDEN query,
 * and exposes only (price, observedAt) plus commitments. Public inputs, in on-chain order:
 *     [0] providerId        registry key of the data provider (hash of domain + TLS pubkey root)
 *     [1] queryCommitment   Poseidon(symbolPacked, salt): hides which instrument is being priced
 *     [2] price             decimal price scaled to 1e18, parsed from the body
 *     [3] observedAt        unix timestamp parsed from the body
 *     [4] bodyCommitment    Poseidon over the packed body bytes (the "notarised" plaintext commitment)
 *
 * ══ TRUST BOUNDARY — READ THIS ══════════════════════════════════════════════════════════════════════
 *  This circuit does NOT verify TLS. Establishing that `body` really is the plaintext that provider
 *  `providerId` sent over an authenticated TLS session needs, in a DECO / TLSNotary-style protocol, either
 *    (a) a notary that took part in the 3-party handshake and signs (providerId, bodyCommitment), or
 *    (b) in-circuit AES-GCM + SHA-256 + P-256 ECDSA + X.509 chain verification (millions of constraints).
 *  This skeleton implements the model (a): the on-chain ZkDecoOracle checks a registered NOTARY's ECDSA
 *  signature over (providerId, bodyCommitment), and THIS circuit proves everything that follows from the
 *  committed body. Therefore today's security = "the registered notary is honest" + "this SNARK is sound".
 *  Replacing the notary by (b) is the research milestone that makes the oracle trustless; the public-input
 *  interface below is designed so that swap needs no contract change.
 * ═══════════════════════════════════════════════════════════════════════════════════════════════════
 *
 * STATEMENT
 *   B1  Byte range    : every body byte is < 256 (so packing into field elements is injective).
 *   B2  Commitment    : bodyCommitment == Poseidon(pack24(body[0..23]), ..., pack24(body[72..95])).
 *   B3  Hidden query  : the response contains `"symbol":"<SYM>"` where SYM are the private symbol bytes and
 *                       queryCommitment == Poseidon(pack(SYM), salt). Verifier learns neither SYM nor salt.
 *   B4  Price field   : "price":<int>.<frac>  followed by ',', with 1..12 integer and 1..10 fractional ASCII
 *                       digits; `price` is that decimal scaled to 1e18, fits in 128 bits, and is non-zero.
 *   B5  Timestamp     : "ts":<10 digits>  followed by '}';  observedAt is that integer.
 *   B6  Structure     : the body IS the single object  {"symbol":"<SYM>","price":<I>.<F>,"ts":<10 digits>}
 *                       (zero-padded): every offset is pinned, so price and timestamp provably belong to the
 *                       object that carries the symbol. Without B6 a body holding several objects (other
 *                       instruments, bid/ask, history) let the prover pick any of their prices or timestamps
 *                       (Phase-8 audit C-3).
 *
 * KNOWN LIMITATIONS of the skeleton (each is a TODO with an owner in docs/):
 *   * One fixed layout and key order. A provider with another format needs its own pinned template; never
 *     relax B6 into a "find the key anywhere" search.
 *   * Prices without a decimal point (e.g. `2345`) are rejected, by design, for a single parse path.
 *   * BODY_LEN = 96 bytes. Larger bodies need more chunks (cost grows ~ BODY_LEN × slice width).
 */

// Extract K consecutive bytes of `in` starting at `off`: out[k] = in[off + k].
// If off+k is outside [0, N) the output is 0, which the callers' ASCII checks then reject.
template SliceAt(N, K) {
    signal input in[N];
    signal input off;
    signal output out[K];

    component eq[K][N];
    signal acc[K][N + 1];
    for (var k = 0; k < K; k++) {
        acc[k][0] <== 0;
        for (var i = 0; i < N; i++) {
            eq[k][i] = IsEqual();
            eq[k][i].in[0] <== i;
            eq[k][i].in[1] <== off + k;
            acc[k][i + 1] <== acc[k][i] + eq[k][i].out * in[i];
        }
        out[k] <== acc[k][N];
    }
}

// 1 iff 48 <= c <= 57 (ASCII '0'..'9'). Input must already be a byte.
template IsDigit() {
    signal input c;
    signal output out;
    component ge = LessEqThan(8);  ge.in[0] <== 48; ge.in[1] <== c;
    component le = LessEqThan(8);  le.in[0] <== c;  le.in[1] <== 57;
    out <== ge.out * le.out;
}

// Asserts byte c is one of the two terminators ',' (44) or '}' (125).
template AssertTerminator() {
    signal input c;
    (c - 44) * (c - 125) === 0;
}

template ZkDecoOracle(BODY_LEN, SYMLEN) {
    var CHUNK = 24;                       // bytes packed per field element (24 < 31: no overflow)
    var NCH = BODY_LEN \ CHUNK;
    var PW = 24;                          // price window after the key: 12 int + '.' + 10 frac + terminator

    // ---- public
    signal input providerId;
    signal input queryCommitment;
    signal input price;
    signal input observedAt;
    signal input bodyCommitment;

    // ---- private
    signal input body[BODY_LEN];
    signal input symbol[SYMLEN];
    signal input salt;
    signal input offSymKey;               // start of `"symbol":"`
    signal input offPriceKey;             // start of `"price":`
    signal input offTsKey;                // start of `"ts":`
    signal input intLen;
    signal input fracLen;

    // B1 + B2 ------------------------------------------------------------------------------------------
    component byteRange[BODY_LEN];
    for (var i = 0; i < BODY_LEN; i++) { byteRange[i] = Num2Bits(8); byteRange[i].in <== body[i]; }

    signal chunk[NCH];
    signal chunkAcc[NCH][CHUNK + 1];
    var pow256[CHUNK];
    pow256[0] = 1;
    for (var k = 1; k < CHUNK; k++) pow256[k] = pow256[k - 1] * 256;
    for (var j = 0; j < NCH; j++) {
        chunkAcc[j][0] <== 0;
        for (var k = 0; k < CHUNK; k++) chunkAcc[j][k + 1] <== chunkAcc[j][k] + body[j * CHUNK + k] * pow256[k];
        chunk[j] <== chunkAcc[j][CHUNK];
    }
    component bc = Poseidon(NCH);
    for (var j = 0; j < NCH; j++) bc.inputs[j] <== chunk[j];
    bc.out === bodyCommitment;

    // offsets are small naturals (no field wrap-around tricks)
    component r1 = Num2Bits(7); r1.in <== offSymKey;
    component r2 = Num2Bits(7); r2.in <== offPriceKey;
    component r3 = Num2Bits(7); r3.in <== offTsKey;

    // B3: hidden query --------------------------------------------------------------------------------
    var SK = 10 + SYMLEN + 1;             // "symbol":"  +  SYM  +  "
    component sSlice = SliceAt(BODY_LEN, SK);
    for (var i = 0; i < BODY_LEN; i++) sSlice.in[i] <== body[i];
    sSlice.off <== offSymKey;
    // "symbol":"  =  34 115 121 109 98 111 108 34 58 34
    var symKey[10] = [34, 115, 121, 109, 98, 111, 108, 34, 58, 34];
    for (var k = 0; k < 10; k++) sSlice.out[k] === symKey[k];
    var symPacked = 0;
    for (var j = 0; j < SYMLEN; j++) {
        sSlice.out[10 + j] === symbol[j];
        symPacked += symbol[j] * (256 ** j);
    }
    sSlice.out[10 + SYMLEN] === 34;       // closing quote: the symbol is EXACTLY these bytes, not a prefix
    component qc = Poseidon(2);
    qc.inputs[0] <== symPacked;
    qc.inputs[1] <== salt;
    qc.out === queryCommitment;

    // B4: price ----------------------------------------------------------------------------------------
    component pSlice = SliceAt(BODY_LEN, 8 + PW);
    for (var i = 0; i < BODY_LEN; i++) pSlice.in[i] <== body[i];
    pSlice.off <== offPriceKey;
    // "price":  =  34 112 114 105 99 101 34 58
    var priceKey[8] = [34, 112, 114, 105, 99, 101, 34, 58];
    for (var k = 0; k < 8; k++) pSlice.out[k] === priceKey[k];

    // 1 <= intLen <= 12, 1 <= fracLen <= 10
    component iLo = LessThan(6); iLo.in[0] <== 0;      iLo.in[1] <== intLen;  iLo.out === 1;
    component iHi = LessThan(6); iHi.in[0] <== intLen;  iHi.in[1] <== 13;      iHi.out === 1;
    component fLo = LessThan(6); fLo.in[0] <== 0;      fLo.in[1] <== fracLen; fLo.out === 1;
    component fHi = LessThan(6); fHi.in[0] <== fracLen; fHi.in[1] <== 11;      fHi.out === 1;

    component isInt[PW];  component isDot[PW];  component afterDot[PW];  component upToFrac[PW];
    component isTerm[PW]; component dig[PW];
    signal fracFlag[PW]; signal mask[PW]; signal dotDiff[PW]; signal digBad[PW]; signal termBad[PW]; signal termProd[PW];
    signal factor[PW]; signal digVal[PW]; signal acc[PW + 1];
    acc[0] <== 0;
    for (var k = 0; k < PW; k++) {
        var c = pSlice.out[8 + k];
        isInt[k] = LessThan(6);   isInt[k].in[0] <== k;      isInt[k].in[1] <== intLen;                  // k <  intLen
        isDot[k] = IsEqual();     isDot[k].in[0] <== k;      isDot[k].in[1] <== intLen;                  // k == intLen
        afterDot[k] = LessThan(6); afterDot[k].in[0] <== intLen; afterDot[k].in[1] <== k;                // k >  intLen
        upToFrac[k] = LessEqThan(6); upToFrac[k].in[0] <== k; upToFrac[k].in[1] <== intLen + fracLen;    // k <= intLen+fracLen
        isTerm[k] = IsEqual();    isTerm[k].in[0] <== k;     isTerm[k].in[1] <== intLen + fracLen + 1;   // terminator slot
        fracFlag[k] <== afterDot[k].out * upToFrac[k].out;
        mask[k] <== isInt[k].out + fracFlag[k];                                                          // digit positions

        dig[k] = IsDigit(); dig[k].c <== c;
        digBad[k] <== mask[k] * (1 - dig[k].out);  digBad[k] === 0;                                      // must be digits
        dotDiff[k] <== isDot[k].out * (c - 46);    dotDiff[k] === 0;                                     // must be '.'
        termProd[k] <== (c - 44) * 1;                                                                    // ',' only: "ts" follows
        termBad[k] <== isTerm[k].out * termProd[k];  termBad[k] === 0;

        factor[k] <== 1 + 9 * mask[k];
        digVal[k] <== mask[k] * (c - 48);
        acc[k + 1] <== acc[k] * factor[k] + digVal[k];                                                   // Horner over digits only
    }
    // scale by 10^(18 - fracLen) with a one-hot table (fracLen in 1..10)
    component fSel[11]; signal fTerm[11]; signal fAcc[12];
    fAcc[0] <== 0;
    var p10[11];
    p10[0] = 10 ** 18;
    for (var f = 1; f <= 10; f++) p10[f] = 10 ** (18 - f);
    for (var f = 0; f <= 10; f++) {
        fSel[f] = IsEqual(); fSel[f].in[0] <== fracLen; fSel[f].in[1] <== f;
        fTerm[f] <== fSel[f].out * p10[f];
        fAcc[f + 1] <== fAcc[f] + fTerm[f];
    }
    price === acc[PW] * fAcc[11];
    component pr = Num2Bits(128); pr.in <== price;          // fits uint128 on-chain
    component pz = IsZero();      pz.in <== price; pz.out === 0;

    // B5: timestamp ------------------------------------------------------------------------------------
    component tSlice = SliceAt(BODY_LEN, 5 + 10 + 1);
    for (var i = 0; i < BODY_LEN; i++) tSlice.in[i] <== body[i];
    tSlice.off <== offTsKey;
    // "ts":  =  34 116 115 34 58
    var tsKey[5] = [34, 116, 115, 34, 58];
    for (var k = 0; k < 5; k++) tSlice.out[k] === tsKey[k];
    component tDig[10]; signal tAcc[11];
    tAcc[0] <== 0;
    for (var k = 0; k < 10; k++) {
        tDig[k] = IsDigit(); tDig[k].c <== tSlice.out[5 + k]; tDig[k].out === 1;
        tAcc[k + 1] <== tAcc[k] * 10 + (tSlice.out[5 + k] - 48);
    }
    observedAt === tAcc[10];
    tSlice.out[15] === 125;                                   // '}' closes THE object

    // B6: canonical single-object layout  {"symbol":"<SYM>","price":<I>.<F>,"ts":<10 digits>}   [audit C-3]
    // Every slice is pinned to the object that carries the symbol: no second "price"/"ts" can be selected.
    body[0] === 123;                                          // '{'
    offSymKey === 1;
    body[1 + 10 + SYMLEN + 1] === 44;                         // ',' right after the closing quote of the symbol
    offPriceKey === 1 + 10 + SYMLEN + 2;
    offTsKey === offPriceKey + 8 + intLen + fracLen + 2;      // "price": + I + '.' + F + ','

    // Keep providerId in the constraint system (it is bound on-chain via the notary signature).
    signal providerSquare;
    providerSquare <== providerId * providerId;
}

component main { public [providerId, queryCommitment, price, observedAt, bodyCommitment] } = ZkDecoOracle(96, 6);
