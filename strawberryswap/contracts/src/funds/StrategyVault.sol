// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {SafeTransfer} from "../utils/SafeTransfer.sol";
import {IERC20Min} from "../interfaces/IERC20Min.sol";

/// @title IStrategyAdapter
/// @notice Audited venue connector (DarkPool swap, OptionsMarket write/buy, SyntheticRWA pair, …) that
///         holds the fund's positions in a per-vault sub-account and reports their value.
/// @dev    Adapters are fixed at vault deployment. Investors judge them BEFORE depositing; the manager can
///         never add or swap one. An adapter MUST: (1) pull at most `amountIn` of the vault's asset using
///         the allowance the vault grants for this call only; (2) never send value to any address except
///         back to `vault` or into positions it accounts for in `valueOf(vault)`; (3) value positions
///         conservatively (mark-to-zk-oracle / TWAP, haircut for illiquidity).
interface IStrategyAdapter {
    function execute(address vault, uint256 amountIn, bytes calldata data) external;
    /// @dev Convert ALL of `vault`'s positions to the base asset and send it to `vault`.
    function unwind(address vault) external;
    /// @dev Base-asset value of `vault`'s positions held inside this adapter. MUST be a pure view.
    function valueOf(address vault) external view returns (uint256);
}

/// @title StrategyVault — Module 9: non-custodial hedge-fund vault with a High-Water-Mark performance fee
///
/// ROLES
///   Investors  deposit the base asset, receive non-transferable shares, redeem any time liquidity exists.
///   Manager    (an address, plus a `managerDid` identifier for off-chain/DID resolution) holds exactly ONE
///              power: `executeTrade`. Both are immutable: no owner, no admin, no upgrade, no key rotation.
///
/// WHY THE MANAGER CANNOT RUG (what is — and is not — guaranteed)
///   Guaranteed by this contract's code:
///    G1  There is no function that transfers, approves or withdraws vault assets to the manager or to any
///        address chosen by the manager. `executeTrade` takes an adapter from an IMMUTABLE allowlist and
///        opaque `data` that only that adapter interprets; the vault never makes an arbitrary call.
///    G2  Each trade moves at most MAX_TRADE_BPS of NAV out of idle balance, and after it NAV (idle +
///        adapters' reported value) must be >= (1 - MAX_TRADE_LOSS_BPS) × NAV before, else the whole tx
///        reverts. A malicious or reckless manager can therefore lose at most MAX_TRADE_LOSS_BPS per trade
///        through bad trading, and a single trade cannot be a one-shot drain.
///    G3  The allowance given to an adapter is exactly `amountIn`, set before and zeroed after the call.
///    G4  The manager is paid ONLY through the performance fee, as newly minted shares (no asset leaves the
///        vault). Those shares redeem like any investor's — that is earned fee, not a privileged exit.
///    G5  Anyone unable to redeem for lack of idle liquidity can force an unwind (`forceUnwind`).
///   NOT guaranteed (honest residual risk): the manager can still trade badly; the allowlisted adapters
///   are trusted code (a malicious adapter or one with a manipulable `valueOf` defeats G2); mark-to-model
///   valuation of illiquid positions can be gamed up to the haircut the adapter applies. "Mathematically
///   locked out" therefore means: no code path for the manager to move funds out — NOT that trading risk
///   or adapter risk is zero.
///
/// PERFORMANCE FEE — strict per-share HIGH-WATER MARK
///   price  = NAV × 1e18 / shares                      (gross of the not-yet-taken fee)
///   hwm    = highest net price at which a fee was last crystallised (initially 1e18)
///   if price <= hwm            : fee = 0                (no new profit ⇒ no fee, losses must be recovered)
///   else                       : fee = FEE_BPS × (price − hwm) × shares / 1e18        [in base asset]
///   The fee is paid by minting f = fee × S / (NAV − fee) shares to the manager, which leaves every other
///   holder's value reduced by exactly `fee`. New hwm = NAV / (S + f) (the investors' net price).
///   Using a PER-SHARE mark means deposits/withdrawals can neither create nor hide profit.
///   Crystallisation runs automatically before every deposit, redemption and trade-free `settleFees()`
///   (permissionless), so no investor ever enters or leaves at a price that ignores an accrued fee.
///   LIMIT: fee timing is not epoch-locked; NAV manipulation right before crystallisation is bounded only
///   by the adapters' conservative valuation. An oracle-TWAP + epoch lock is the Phase-5 hardening.
///
/// NOT AUDITED. Base asset: plain 18-decimal ERC-20. Shares are non-transferable (ZK-wrapping later).
contract StrategyVault is ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    // ------------------------------------------------------------------ constants
    uint256 internal constant BPS = 10_000;
    uint256 internal constant WAD = 1e18;
    uint256 public constant MINIMUM_SHARES = 1_000; // locked at first deposit (inflation-attack guard)
    uint256 public constant MAX_ADAPTERS = 8;
    uint256 public immutable PERFORMANCE_FEE_BPS; // e.g. 2000 = 20 %, capped at 30 %
    uint256 public immutable MAX_TRADE_BPS; // max share of NAV committed by one trade
    uint256 public immutable MAX_TRADE_LOSS_BPS; // max NAV drop tolerated per trade (slippage/valuation)

    // ------------------------------------------------------------------ identity (immutable)
    IERC20Min public immutable asset;
    address public immutable manager;
    bytes32 public immutable managerDid; // hash/identifier of the manager's DID document
    IStrategyAdapter[] private _adapters;
    mapping(address => bool) public isAdapter;

    // ------------------------------------------------------------------ state
    uint256 public totalShares;
    mapping(address => uint256) public sharesOf;
    uint256 public highWaterMark = WAD; // per-share, 1e18

    // ------------------------------------------------------------------ events / errors
    event Deposit(address indexed investor, address indexed to, uint256 assets, uint256 shares);
    event Redeem(address indexed investor, address indexed to, uint256 shares, uint256 assets);
    event TradeExecuted(address indexed adapter, uint256 amountIn, uint256 navBefore, uint256 navAfter);
    event FeeCrystallised(uint256 feeAssets, uint256 feeShares, uint256 newHighWaterMark, uint256 grossPrice);
    event ForcedUnwind(address indexed caller, address indexed adapter);

    error NotManager();
    error AmountZero();
    error InvalidRecipient();
    error InvalidConfig();
    error UnknownAdapter();
    error TradeTooLarge();
    error LossLimitBreached(uint256 navBefore, uint256 navAfter);
    error InsufficientShares();
    error InsufficientIdleLiquidity();
    error LiquidityNotNeeded();
    error FeeOnTransferUnsupported();

    constructor(
        IERC20Min asset_,
        address manager_,
        bytes32 managerDid_,
        IStrategyAdapter[] memory adapters_,
        uint256 performanceFeeBps_,
        uint256 maxTradeBps_,
        uint256 maxTradeLossBps_
    ) {
        if (
            address(asset_) == address(0) || manager_ == address(0) || adapters_.length == 0
                || adapters_.length > MAX_ADAPTERS || performanceFeeBps_ > 3_000 || maxTradeBps_ == 0
                || maxTradeBps_ > BPS || maxTradeLossBps_ > 1_000 || asset_.decimals() != 18
        ) revert InvalidConfig();
        asset = asset_;
        manager = manager_;
        managerDid = managerDid_;
        PERFORMANCE_FEE_BPS = performanceFeeBps_;
        MAX_TRADE_BPS = maxTradeBps_;
        MAX_TRADE_LOSS_BPS = maxTradeLossBps_;
        for (uint256 i; i < adapters_.length; ++i) {
            address a = address(adapters_[i]);
            if (a == address(0) || isAdapter[a]) revert InvalidConfig();
            isAdapter[a] = true;
            _adapters.push(adapters_[i]);
        }
    }

    // ================================================================== NAV
    /// @notice Idle balance + adapters' reported positions, in base asset.
    function totalAssets() public view returns (uint256 nav) {
        nav = asset.balanceOf(address(this));
        uint256 n = _adapters.length;
        for (uint256 i; i < n; ++i) {
            nav += _adapters[i].valueOf(address(this));
        }
    }

    function adapters() external view returns (IStrategyAdapter[] memory) {
        return _adapters;
    }

    /// @notice Gross per-share price (1e18) before any pending fee. 1e18 when empty.
    function sharePrice() public view returns (uint256) {
        return totalShares == 0 ? WAD : totalAssets() * WAD / totalShares;
    }

    /// @notice Fee that WOULD be crystallised now, in base asset. 0 whenever price <= high-water mark.
    function pendingFee() public view returns (uint256 fee) {
        uint256 s = totalShares;
        if (s == 0) return 0;
        uint256 price = totalAssets() * WAD / s;
        if (price <= highWaterMark) return 0;
        fee = (price - highWaterMark) * s / WAD * PERFORMANCE_FEE_BPS / BPS;
    }

    // ================================================================== investors
    function deposit(uint256 assets, address to) external nonReentrant returns (uint256 shares) {
        if (assets == 0) revert AmountZero();
        if (to == address(0)) revert InvalidRecipient();
        _crystallise(); // entrant buys at a price that already deducts the accrued fee

        uint256 s = totalShares;
        if (s == 0) {
            if (assets <= MINIMUM_SHARES) revert AmountZero();
            shares = assets - MINIMUM_SHARES;
            totalShares = MINIMUM_SHARES; // permanently locked, owned by nobody
            highWaterMark = WAD;
        } else {
            shares = assets * s / totalAssets();
        }
        if (shares == 0) revert AmountZero();

        totalShares += shares;
        sharesOf[to] += shares;

        uint256 before = asset.balanceOf(address(this));
        asset.safeTransferFrom(msg.sender, address(this), assets);
        if (asset.balanceOf(address(this)) - before != assets) revert FeeOnTransferUnsupported();
        emit Deposit(msg.sender, to, assets, shares);
    }

    /// @notice Burn shares for base asset out of IDLE balance. Reverts if not enough is idle — then use
    ///         `forceUnwind`. Pays at the post-fee price.
    function redeem(uint256 shares, address to) external nonReentrant returns (uint256 assets) {
        if (shares == 0) revert AmountZero();
        if (to == address(0)) revert InvalidRecipient();
        if (sharesOf[msg.sender] < shares) revert InsufficientShares();
        _crystallise();

        assets = shares * totalAssets() / totalShares;
        if (assets > asset.balanceOf(address(this))) revert InsufficientIdleLiquidity();

        sharesOf[msg.sender] -= shares;
        totalShares -= shares;
        asset.safeTransfer(to, assets);
        emit Redeem(msg.sender, to, shares, assets);
    }

    /// @notice Any shareholder who currently CANNOT redeem `shares` for lack of idle cash can force one
    ///         adapter to liquidate everything to base asset. Anti-lock-up guarantee (G5).
    function forceUnwind(IStrategyAdapter adapter, uint256 shares) external nonReentrant {
        if (!isAdapter[address(adapter)]) revert UnknownAdapter();
        if (sharesOf[msg.sender] < shares || shares == 0) revert InsufficientShares();
        uint256 owed = shares * totalAssets() / totalShares;
        if (owed <= asset.balanceOf(address(this))) revert LiquidityNotNeeded();
        adapter.unwind(address(this));
        emit ForcedUnwind(msg.sender, address(adapter));
    }

    // ================================================================== the manager's ONLY power
    /// @notice Rebalance through an allowlisted adapter. The ONLY state-changing function the manager can
    ///         call. It cannot name a recipient, a token or a target.
    function executeTrade(IStrategyAdapter adapter, uint256 amountIn, bytes calldata data) external nonReentrant {
        if (msg.sender != manager) revert NotManager();
        if (!isAdapter[address(adapter)]) revert UnknownAdapter();

        uint256 navBefore = totalAssets();
        if (amountIn > navBefore * MAX_TRADE_BPS / BPS || amountIn > asset.balanceOf(address(this))) {
            revert TradeTooLarge();
        }

        // allowance exists only for this call (G3)
        asset.safeApprove(address(adapter), amountIn);
        adapter.execute(address(this), amountIn, data);
        asset.safeApprove(address(adapter), 0);

        uint256 navAfter = totalAssets();
        if (navAfter * BPS < navBefore * (BPS - MAX_TRADE_LOSS_BPS)) revert LossLimitBreached(navBefore, navAfter);
        emit TradeExecuted(address(adapter), amountIn, navBefore, navAfter);
    }

    // ================================================================== performance fee
    /// @notice Permissionless settlement tick: crystallise the fee if (and only if) the per-share price is
    ///         above the high-water mark. Anyone (keeper / Automator) may call it on a cycle.
    function settleFees() external nonReentrant {
        _crystallise();
    }

    function _crystallise() private {
        uint256 s = totalShares;
        if (s == 0) return;
        uint256 nav = totalAssets();
        uint256 price = nav * WAD / s;
        if (price <= highWaterMark) return; // no new profit ⇒ fee == 0

        uint256 fee = (price - highWaterMark) * s / WAD * PERFORMANCE_FEE_BPS / BPS;
        if (fee == 0 || fee >= nav) return;
        uint256 feeShares = fee * s / (nav - fee);

        totalShares = s + feeShares;
        sharesOf[manager] += feeShares;
        highWaterMark = nav * WAD / (s + feeShares); // investors' net price becomes the new mark
        emit FeeCrystallised(fee, feeShares, highWaterMark, price);
    }
}
