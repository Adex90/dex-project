// ⚠ DEV-ONLY VERIFICATION KEY — generated from a single-party local setup (scripts/setup-dev.mjs).
// Anyone holding that setup's toxic waste can forge proofs. TEST FIXTURE: NEVER DEPLOY.
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

contract JoinSplitGroth16Verifier {
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
    uint256 constant deltax1 = 15827353717049981575456096311302773013044296524423380783960995061070021464614;
    uint256 constant deltax2 = 17861781344704783786770134958860363265049294040501531319464051927317351588949;
    uint256 constant deltay1 = 7020946992225932558934455593329710185968053644952888030876731003903378229718;
    uint256 constant deltay2 = 11169400208552505720740906115877363366853732306764177952183026167782289829581;

    
    uint256 constant IC0x = 7339323034802071185220495877079306709712335000226918301256309468984454369956;
    uint256 constant IC0y = 21130680281549124703153462352544227198630205664393612687595440888167939593411;
    
    uint256 constant IC1x = 18411890841195674247690916813211311935394647724747142742321831535762515749867;
    uint256 constant IC1y = 3293980282454123098431628285475089376274160316109727485574963233290379376155;
    
    uint256 constant IC2x = 13109000647577469849547870745980756612063928041814130899370595477278120801928;
    uint256 constant IC2y = 12952175610379297444850297255894865145885437390831327961874144176578465092186;
    
    uint256 constant IC3x = 17860450372339458507199586390636968670686387052371022086002189430200489212183;
    uint256 constant IC3y = 1864108434789074110448276780465412883978289197590669330960059761685236819965;
    
    uint256 constant IC4x = 17121235438063288991144210174211917178776954047584067268853615240162481528910;
    uint256 constant IC4y = 9589399113045758128778774261485847118066515976452578119836375650388669309711;
    
    uint256 constant IC5x = 10790437913854344443909089966438806031939835323633461427260142990203655790683;
    uint256 constant IC5y = 11684407743163546934973652831715320617994275491162458443879337484333769808945;
    
    uint256 constant IC6x = 15215656839143565201322813804118664528519962682479939274777386978804241510082;
    uint256 constant IC6y = 14893495325971449788989846010313759953154444798590137303574485907656385243544;
    
    uint256 constant IC7x = 12126718230133052647268942519212411266130445205611748306420401866225079283921;
    uint256 constant IC7y = 11367680039879569909662810930950508581876283064741436785154222359539330043364;
    
    uint256 constant IC8x = 3197778511778769929086564697152498040673953413854103994483048540078724409927;
    uint256 constant IC8y = 11362186280289650260481910605359155270878225883082436811193655625144704029021;
    
    uint256 constant IC9x = 11876289498471989340552223110380021246686647854053750980861268336929768372665;
    uint256 constant IC9y = 13867753722808668310107933162047549976713719686179246651084319854709242255793;
    
 
    // Memory data
    uint16 constant pVk = 0;
    uint16 constant pPairing = 128;

    uint16 constant pLastMem = 896;

    function verifyProof(uint[2] calldata _pA, uint[2][2] calldata _pB, uint[2] calldata _pC, uint[9] calldata _pubSignals) public view returns (bool) {
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
            

            // Validate all evaluations
            let isValid := checkPairing(_pA, _pB, _pC, _pubSignals, pMem)

            mstore(0, isValid)
             return(0, 0x20)
         }
     }
 }
