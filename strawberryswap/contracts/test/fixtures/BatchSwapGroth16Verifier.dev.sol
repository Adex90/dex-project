// ⚠ DEV-ONLY VERIFICATION KEY (single-party local setup). TEST FIXTURE: NEVER DEPLOY.
// SPDX-License-Identifier: GPL-3.0
/*
    Copyright 2021 0KIMS association.

    This file is generated with [snarkJS](https://github.com/iden3/snarkjs).

    snarkJS is a free software: you can redistribute it and/or modify it
    under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    snarkJS is distributed in the hope that it will be useful, but WITHOUT
    ANY WARRANTY; without even the implied warranty of MERCHANTABILITY
    or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public
    License for more details.

    You should have received a copy of the GNU General Public License
    along with snarkJS. If not, see <https://www.gnu.org/licenses/>.
*/

pragma solidity >=0.7.0 <0.9.0;

contract BatchSwapGroth16Verifier {
    // Scalar field size
    uint256 constant r    = 21888242871839275222246405745257275088548364400416034343698204186575808495617;
    // Base field size
    uint256 constant q   = 21888242871839275222246405745257275088696311157297823662689037894645226208583;

    // Verification Key data
    uint256 constant alphax  = 17797791978804452900812952468407169528283938105815801010010027552465778339613;
    uint256 constant alphay  = 4973403868078346063530254020530722112205191039836140120150419776417093202577;
    uint256 constant betax1  = 18110322898032832337711309359803514252472053214561689995482932928545229986085;
    uint256 constant betax2  = 16630406438243160427265512201378126984980991571911869197273412437253650841157;
    uint256 constant betay1  = 5480896496252746083165873090916788946301299037514564323784923595008274045780;
    uint256 constant betay2  = 6288676218715958251644816223162964620186590616121634655470374927360493949849;
    uint256 constant gammax1 = 11559732032986387107991004021392285783925812861821192530917403151452391805634;
    uint256 constant gammax2 = 10857046999023057135944570762232829481370756359578518086990519993285655852781;
    uint256 constant gammay1 = 4082367875863433681332203403145435568316851327593401208105741076214120093531;
    uint256 constant gammay2 = 8495653923123431417604973247489272438418190587263600148770280649306958101930;
    uint256 constant deltax1 = 5553704723033090783183460472871360016949331739404786926901812407226450890478;
    uint256 constant deltax2 = 14274574367879227357521982177437514952721648260554769105536542256442229611179;
    uint256 constant deltay1 = 10880579727417847570492250763134533451594293980132422758187474969056101557598;
    uint256 constant deltay2 = 19148571883793957174380123962705470589146536299416614960351818351191949748812;

    
    uint256 constant IC0x = 17465082545185959280378910870592167486149373120024585220440379786890114821030;
    uint256 constant IC0y = 12290636932874279941684151337095415285723426716735419632904418190085239454557;
    
    uint256 constant IC1x = 2210817313876917023500868944761640596042631789647493668928944560246524801175;
    uint256 constant IC1y = 17893095680185373702074285934635494410064012962520485282387582338515060648769;
    
    uint256 constant IC2x = 2176171776813664142623301179259155355950894352797533757445063382865885763338;
    uint256 constant IC2y = 16920791058675109938495385376283894488787075599786559465766983088833579812031;
    
    uint256 constant IC3x = 16214291547571268108434525936094141927856975919061740219016977861941627504711;
    uint256 constant IC3y = 5898512157151352138230572211422848038844861893424592017719153368266353660485;
    
    uint256 constant IC4x = 1743879684976754693031666168649172879417997275069459400383937085790826856479;
    uint256 constant IC4y = 2465214277537802460160346512317121846485963410110031251265314785025165947359;
    
    uint256 constant IC5x = 3386636409617292025146215119324805888396696088132502360694299670115211131958;
    uint256 constant IC5y = 7951048577608369637537535434697857147872213454923077446371503919960718139053;
    
    uint256 constant IC6x = 1060182045374933019374800264337579194565550715160247923974532848795952109265;
    uint256 constant IC6y = 18591187492974242249756458566105498865898158695939537822231093891668230639405;
    
    uint256 constant IC7x = 16164444096199488384857408953119380610377171345663540282281163019331574270426;
    uint256 constant IC7y = 14958708844719832269828505882299854649003272351269869406539213343418660644078;
    
    uint256 constant IC8x = 20732936357251208612210073938254863183212559893908924193285346524507246710116;
    uint256 constant IC8y = 15303076701639011297487210455226129104101806786655182977140919477100996386775;
    
    uint256 constant IC9x = 6907936453475900416036502998683603785036526733086920704489798512244748803212;
    uint256 constant IC9y = 16934138935987503991175312896026069661325022283810959059685272658365656873167;
    
    uint256 constant IC10x = 19206355761389363950953306890076932278079792085197919284069599122537325556547;
    uint256 constant IC10y = 17016234131701421398742655028071095610723916068280392100517479760076592594748;
    
    uint256 constant IC11x = 13241530997765605402586985464744000211252825385178906793697145876545567820384;
    uint256 constant IC11y = 19628699356648354155974312679277902900328053553039297997617153988069986878851;
    
    uint256 constant IC12x = 12942575633009702914563911683541637384675450565379749026880829056164365755589;
    uint256 constant IC12y = 11155203125454522690847312336531173574374611936043089948701771254112015930634;
    
    uint256 constant IC13x = 4168669425725929885459341135599333291868081268460235993018188448704591436953;
    uint256 constant IC13y = 11957867154800296540249874165166082287334725156564349732462864016751046925639;
    
    uint256 constant IC14x = 21560187167535193077772068303184230735967799676928260102052999914905498214254;
    uint256 constant IC14y = 8750718389899150835462991831473359028099057303292234162551966552500929076587;
    
    uint256 constant IC15x = 3753883227525706289547505587964519819414650026296828772729681069603652137098;
    uint256 constant IC15y = 6780816084776399524303151435576242647368339534148565050126792484202099281757;
    
    uint256 constant IC16x = 11419963375212813419890809921569719711831818445721290060804545882616840870765;
    uint256 constant IC16y = 1972383860714723365646796338422694541737921841662487002026532416766961821710;
    
 
    // Memory data
    uint16 constant pVk = 0;
    uint16 constant pPairing = 128;

    uint16 constant pLastMem = 896;

    function verifyProof(uint[2] calldata _pA, uint[2][2] calldata _pB, uint[2] calldata _pC, uint[16] calldata _pubSignals) public view returns (bool) {
        assembly {
            function checkField(v) {
                if iszero(lt(v, r)) {
                    mstore(0, 0)
                    return(0, 0x20)
                }
            }
            
            // G1 function to multiply a G1 value(x,y) to value in an address
            function g1_mulAccC(pR, x, y, s) {
                let success
                let mIn := mload(0x40)
                mstore(mIn, x)
                mstore(add(mIn, 32), y)
                mstore(add(mIn, 64), s)

                success := staticcall(sub(gas(), 2000), 7, mIn, 96, mIn, 64)

                if iszero(success) {
                    mstore(0, 0)
                    return(0, 0x20)
                }

                mstore(add(mIn, 64), mload(pR))
                mstore(add(mIn, 96), mload(add(pR, 32)))

                success := staticcall(sub(gas(), 2000), 6, mIn, 128, pR, 64)

                if iszero(success) {
                    mstore(0, 0)
                    return(0, 0x20)
                }
            }

            function checkPairing(pA, pB, pC, pubSignals, pMem) -> isOk {
                let _pPairing := add(pMem, pPairing)
                let _pVk := add(pMem, pVk)

                mstore(_pVk, IC0x)
                mstore(add(_pVk, 32), IC0y)

                // Compute the linear combination vk_x
                
                g1_mulAccC(_pVk, IC1x, IC1y, calldataload(add(pubSignals, 0)))
                
                g1_mulAccC(_pVk, IC2x, IC2y, calldataload(add(pubSignals, 32)))
                
                g1_mulAccC(_pVk, IC3x, IC3y, calldataload(add(pubSignals, 64)))
                
                g1_mulAccC(_pVk, IC4x, IC4y, calldataload(add(pubSignals, 96)))
                
                g1_mulAccC(_pVk, IC5x, IC5y, calldataload(add(pubSignals, 128)))
                
                g1_mulAccC(_pVk, IC6x, IC6y, calldataload(add(pubSignals, 160)))
                
                g1_mulAccC(_pVk, IC7x, IC7y, calldataload(add(pubSignals, 192)))
                
                g1_mulAccC(_pVk, IC8x, IC8y, calldataload(add(pubSignals, 224)))
                
                g1_mulAccC(_pVk, IC9x, IC9y, calldataload(add(pubSignals, 256)))
                
                g1_mulAccC(_pVk, IC10x, IC10y, calldataload(add(pubSignals, 288)))
                
                g1_mulAccC(_pVk, IC11x, IC11y, calldataload(add(pubSignals, 320)))
                
                g1_mulAccC(_pVk, IC12x, IC12y, calldataload(add(pubSignals, 352)))
                
                g1_mulAccC(_pVk, IC13x, IC13y, calldataload(add(pubSignals, 384)))
                
                g1_mulAccC(_pVk, IC14x, IC14y, calldataload(add(pubSignals, 416)))
                
                g1_mulAccC(_pVk, IC15x, IC15y, calldataload(add(pubSignals, 448)))
                
                g1_mulAccC(_pVk, IC16x, IC16y, calldataload(add(pubSignals, 480)))
                

                // -A
                mstore(_pPairing, calldataload(pA))
                mstore(add(_pPairing, 32), mod(sub(q, calldataload(add(pA, 32))), q))

                // B
                mstore(add(_pPairing, 64), calldataload(pB))
                mstore(add(_pPairing, 96), calldataload(add(pB, 32)))
                mstore(add(_pPairing, 128), calldataload(add(pB, 64)))
                mstore(add(_pPairing, 160), calldataload(add(pB, 96)))

                // alpha1
                mstore(add(_pPairing, 192), alphax)
                mstore(add(_pPairing, 224), alphay)

                // beta2
                mstore(add(_pPairing, 256), betax1)
                mstore(add(_pPairing, 288), betax2)
                mstore(add(_pPairing, 320), betay1)
                mstore(add(_pPairing, 352), betay2)

                // vk_x
                mstore(add(_pPairing, 384), mload(add(pMem, pVk)))
                mstore(add(_pPairing, 416), mload(add(pMem, add(pVk, 32))))


                // gamma2
                mstore(add(_pPairing, 448), gammax1)
                mstore(add(_pPairing, 480), gammax2)
                mstore(add(_pPairing, 512), gammay1)
                mstore(add(_pPairing, 544), gammay2)

                // C
                mstore(add(_pPairing, 576), calldataload(pC))
                mstore(add(_pPairing, 608), calldataload(add(pC, 32)))

                // delta2
                mstore(add(_pPairing, 640), deltax1)
                mstore(add(_pPairing, 672), deltax2)
                mstore(add(_pPairing, 704), deltay1)
                mstore(add(_pPairing, 736), deltay2)


                let success := staticcall(sub(gas(), 2000), 8, _pPairing, 768, _pPairing, 0x20)

                isOk := and(success, mload(_pPairing))
            }

            let pMem := mload(0x40)
            mstore(0x40, add(pMem, pLastMem))

            // Validate that all evaluations ∈ F
            
            checkField(calldataload(add(_pubSignals, 0)))
            
            checkField(calldataload(add(_pubSignals, 32)))
            
            checkField(calldataload(add(_pubSignals, 64)))
            
            checkField(calldataload(add(_pubSignals, 96)))
            
            checkField(calldataload(add(_pubSignals, 128)))
            
            checkField(calldataload(add(_pubSignals, 160)))
            
            checkField(calldataload(add(_pubSignals, 192)))
            
            checkField(calldataload(add(_pubSignals, 224)))
            
            checkField(calldataload(add(_pubSignals, 256)))
            
            checkField(calldataload(add(_pubSignals, 288)))
            
            checkField(calldataload(add(_pubSignals, 320)))
            
            checkField(calldataload(add(_pubSignals, 352)))
            
            checkField(calldataload(add(_pubSignals, 384)))
            
            checkField(calldataload(add(_pubSignals, 416)))
            
            checkField(calldataload(add(_pubSignals, 448)))
            
            checkField(calldataload(add(_pubSignals, 480)))
            

            // Validate all evaluations
            let isValid := checkPairing(_pA, _pB, _pC, _pubSignals, pMem)

            mstore(0, isValid)
             return(0, 0x20)
         }
     }
 }
