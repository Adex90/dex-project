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
    uint256 constant deltax1 = 18237837541321273760639448876355946761826797190500981075540792810389794804674;
    uint256 constant deltax2 = 7941748528247543546615984719781311135719072498216739002575755191285760486601;
    uint256 constant deltay1 = 7985109467293302378602896066551581475796922157409161997758026213279167816859;
    uint256 constant deltay2 = 14283185445803356664800592258078376660048508037943640163294550054126710059271;

    
    uint256 constant IC0x = 15510929606146942504884564118525803222705656048922971593749094109475351978658;
    uint256 constant IC0y = 8381734127331586435425418065012278519634164723259815246547112346153576759125;
    
    uint256 constant IC1x = 19654746388016762101161795119074845018162588889423126161881359684638747938815;
    uint256 constant IC1y = 2452451129130745474825318790919940597668785623150681169617020459462584871240;
    
    uint256 constant IC2x = 17153389855004846893170378650300499062322741054982491020821724962500379289668;
    uint256 constant IC2y = 14264218760833702261858957587983139956099608255859819115671222439891446827514;
    
    uint256 constant IC3x = 21872736405595031946670407550248269065661207150405212310012464053670564552927;
    uint256 constant IC3y = 2607881179992074176999870294486115398821079777406158943939445840433572262463;
    
    uint256 constant IC4x = 12494773272919093038487170039565280043894493883015072033192920543696236839133;
    uint256 constant IC4y = 11770329340049539314531904739081494306786356360547667322610743503418553765190;
    
    uint256 constant IC5x = 20400360954677936068861341179844315874554189126266377243684696906461999328606;
    uint256 constant IC5y = 7907194331308529794941068411648247374668467208003421108630223575728942208733;
    
    uint256 constant IC6x = 8657281933718976085992356082054704879802821006631881065785733450049765716565;
    uint256 constant IC6y = 19089682210981033414853459993942513027866335070974020222956739769949136756128;
    
    uint256 constant IC7x = 2259859239640319830483484095565222865343982032777087520490452766975565892321;
    uint256 constant IC7y = 7896015926739274271970381354332861767820254043683399364743612233014004815358;
    
    uint256 constant IC8x = 16878686594142243379722546834770432374507757982255414805128400777010658949928;
    uint256 constant IC8y = 977867660565575102010189600539586529028662426620699685488254990913618720223;
    
    uint256 constant IC9x = 9105939648230695680459626065706288174393985861244251500458434383837399921291;
    uint256 constant IC9y = 15015317549868608186351422829930868375805050727316748030000799400880394880840;
    
    uint256 constant IC10x = 618502033812604359149360193407774982905969366327076183560101453028983045869;
    uint256 constant IC10y = 9638693770387733442086662320792039840409022568973469960018520089388305691211;
    
    uint256 constant IC11x = 7910274347701323172665129966185635570616303283548460428830784667914515832087;
    uint256 constant IC11y = 20317509789612253290317330155922945537903094335266882730800809429964101555814;
    
    uint256 constant IC12x = 5255627454870747967900589883769794333137761468044414527522869163232289944315;
    uint256 constant IC12y = 15001668597694027516011775214453843481369760577489154523866664580339126404675;
    
    uint256 constant IC13x = 18894235985084307465195862748477285945535732392508661625105103801288705295716;
    uint256 constant IC13y = 12139950129845508992372919193769616508693460656048605262442132137133943213682;
    
    uint256 constant IC14x = 16422961180185522424640726657966653508547111859611478067115120272725829993935;
    uint256 constant IC14y = 277514651468813247065982677559453828768097861751460447265614882378837718420;
    
    uint256 constant IC15x = 15869284548053591524389787946312695163969770667887750853108399901617661947652;
    uint256 constant IC15y = 13796077776145630832654215773290566010781224321738011269531490876419152406979;
    
    uint256 constant IC16x = 5828624750077506723374019530889859002916496473607580452335841141992370785351;
    uint256 constant IC16y = 9175428278652082938943141060760914226523525629024083434788160005099448096223;
    
 
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
