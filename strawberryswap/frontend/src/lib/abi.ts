import { parseAbi } from "viem";

// Minimal human-readable ABIs: only what the UI calls. Keep in sync with contracts/src (Phase 1 & 3).

export const erc20Abi = parseAbi([
  "function symbol() view returns (string)",
  "function decimals() view returns (uint8)",
  "function balanceOf(address) view returns (uint256)",
  "function allowance(address owner, address spender) view returns (uint256)",
  "function approve(address spender, uint256 amount) returns (bool)",
]);

export const darkPoolAbi = parseAbi([
  "function token0() view returns (address)",
  "function token1() view returns (address)",
  "function reserve0() view returns (uint128)",
  "function reserve1() view returns (uint128)",
  "function virtual0() view returns (uint256)",
  "function virtual1() view returns (uint256)",
  "function spotPrice() view returns (uint256)",
  "function invariantK() view returns (uint256)",
  "function getAmountOut(uint256 amountIn, bool zeroForOne) view returns (uint256)",
  "function extractableValue() view returns (uint256)",
  "function RETAIL_FEE_BPS() view returns (uint256)",
  "function EXTRACTION_BPS() view returns (uint256)",
  "event BatchSettled(uint256 in0, uint256 in1, uint256 out0, uint256 out1, uint32 firstLeaf)",
  "event Rebalanced(address indexed searcher, uint256 harvest, uint256 treasuryPaid, uint256 priceBefore)",
]);

export const vaultAbi = parseAbi([
  "function asset() view returns (address)",
  "function manager() view returns (address)",
  "function managerDid() view returns (bytes32)",
  "function totalAssets() view returns (uint256)",
  "function totalShares() view returns (uint256)",
  "function sharePrice() view returns (uint256)",
  "function highWaterMark() view returns (uint256)",
  "function pendingFee() view returns (uint256)",
  "function sharesOf(address) view returns (uint256)",
  "function PERFORMANCE_FEE_BPS() view returns (uint256)",
  "function MAX_TRADE_BPS() view returns (uint256)",
  "function MAX_TRADE_LOSS_BPS() view returns (uint256)",
  "function deposit(uint256 assets, address to) returns (uint256 shares)",
  "function redeem(uint256 shares, address to) returns (uint256 assets)",
  "function settleFees()",
  "event TradeExecuted(address indexed adapter, uint256 amountIn, uint256 navBefore, uint256 navAfter)",
  "event FeeCrystallised(uint256 feeAssets, uint256 feeShares, uint256 newHighWaterMark, uint256 grossPrice)",
]);
