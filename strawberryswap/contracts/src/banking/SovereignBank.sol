// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {SafeTransfer} from "../utils/SafeTransfer.sol";
import {IERC20Min} from "../interfaces/IERC20Min.sol";

// =====================================================================================================
//   SOVEREIGN BANK — Module 5.  Two completely separate contracts; the facade only deploys them.
//
//   Separation is STRUCTURAL, not a policy flag: money in `DemandDeposits` lives at a different address
//   from money in `TimeDeposits`, and the first contract's bytecode contains no instruction path that
//   lends, stakes, swaps, approves or calls anything except the ERC-20 being deposited/withdrawn.
//   (Rothbardian 100 % reserve: demand liabilities are backed by idle, un-lent money-warehouse balances;
//    credit is funded only by time deposits whose maturity is at least the loan's.)
// =====================================================================================================

/// @title IYieldAdapter
/// @notice A venue where TIME-deposit capital may be put to work (P2P credit market, DarkPool LP, …).
/// @dev    Chosen per deposit by the depositor, so a bad adapter can only hurt the depositor who opted in.
///         Adapter MUST ensure every loan/position funded by `deploy` is repayable/unwindable by
///         `maturity` (maturity matching), and `recall` MUST send the tokens back to `msg.sender`.
interface IYieldAdapter {
    /// @dev Tokens were already transferred to the adapter before this call (push model: no allowance is
    ///      ever left open to an adapter). Return an opaque handle that identifies the position.
    function deploy(IERC20Min token, uint256 amount, uint64 maturity, bytes calldata data)
        external
        returns (bytes32 handle);

    /// @dev [audit H-6]: approve `amount` to msg.sender and return it; MUST revert unless msg.sender is the depositor
    ///      recorded at `deploy`, and MUST NOT push tokens. TimeDeposits pulls exactly `amount` itself.
    function recall(bytes32 handle) external returns (uint256 amount);
}

// -----------------------------------------------------------------------------------------------------
//  1. DEMAND DEPOSITS — pure custody. Zero yield. Zero systemic risk.
// -----------------------------------------------------------------------------------------------------

/// @title DemandDeposits
/// @notice Warehouse for money payable on demand.
///
/// WHAT THIS CONTRACT INTENTIONALLY LACKS (audit checklist — each item is verifiable by grep):
///   * no owner / admin / operator / pause / upgrade / `delegatecall` / `selfdestruct`
///   * no `approve`, no generic `call`, no adapter or strategy hook, no oracle, no interest accounting
///   * no function that sends funds to anyone but the depositor-chosen recipient of THEIR OWN balance
///   * no function that rescues "stray" tokens (donations are simply unclaimable — accepted trade-off)
///   The ONLY external calls are `transferFrom`/`transfer`/`balanceOf` on the token being custodied.
///
/// INTERPRETATION NOTE: "locked, 0 %" is implemented as: locked against ANY use by the protocol or a
///   third party; the depositor can always withdraw their own balance on demand (that is what makes it a
///   demand deposit). The depositor earns exactly 0.
///
/// INVARIANT (I-RES, 100 % reserve): totalDeposits[token] == Σ balanceOf[token][user], and nothing in this
///   contract can make token.balanceOf(this) fall below totalDeposits[token]. If the TOKEN itself shrinks the
///   balance (negative rebase, slashing, haircut), a withdrawal of `amount` pays amount · balance/totalDeposits:
///   every depositor bears the same fraction and nobody is frozen [audit M-1].
/// Per-token isolation: a hostile token can only corrupt accounting of ITSELF, never of another token.
contract DemandDeposits is ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    mapping(IERC20Min => mapping(address => uint256)) public balanceOf;
    mapping(IERC20Min => uint256) public totalDeposits;

    event Deposited(IERC20Min indexed token, address indexed account, uint256 amount);
    event Withdrawn(IERC20Min indexed token, address indexed account, address to, uint256 amount);

    error AmountZero();
    error InvalidRecipient();
    error InsufficientBalance();
    error FeeOnTransferUnsupported();

    function deposit(IERC20Min token, uint256 amount) external nonReentrant {
        if (amount == 0) revert AmountZero();
        // effects first (reverted wholesale if the pull below falls short)
        balanceOf[token][msg.sender] += amount;
        totalDeposits[token] += amount;

        uint256 before = token.balanceOf(address(this));
        token.safeTransferFrom(msg.sender, address(this), amount);
        if (token.balanceOf(address(this)) - before != amount) revert FeeOnTransferUnsupported();
        emit Deposited(token, msg.sender, amount);
    }

    /// @notice Withdraw from the caller's OWN balance. Nobody else can move it.
    function withdraw(IERC20Min token, uint256 amount, address to) external nonReentrant {
        if (amount == 0) revert AmountZero();
        if (to == address(0)) revert InvalidRecipient();
        uint256 bal = balanceOf[token][msg.sender];
        if (bal < amount) revert InsufficientBalance();
        unchecked {
            balanceOf[token][msg.sender] = bal - amount;
        }
        uint256 total = totalDeposits[token];
        totalDeposits[token] = total - amount;

        // [audit M-1]: a shortfall caused by the TOKEN (negative rebase, slashing) is shared pro rata instead of
        // freezing every account: pay amount·balance/total. balance/total is invariant under withdrawals, so the
        // order of exits cannot matter. Outflow is verified exactly (sender-fee tokens rejected).
        uint256 held = token.balanceOf(address(this));
        uint256 pay = held >= total ? amount : amount * held / total;
        token.safeTransfer(to, pay);
        if (held - token.balanceOf(address(this)) != pay) revert FeeOnTransferUnsupported();
        emit Withdrawn(token, msg.sender, to, pay);
    }
}

// -----------------------------------------------------------------------------------------------------
//  2. TIME DEPOSITS — fixed term. The ONLY capital that may be routed to credit / LP in exchange for yield.
// -----------------------------------------------------------------------------------------------------

/// @title TimeDeposits
/// @notice Fixed-term deposits. Time preference is respected by construction: there is NO early-withdrawal
///         function. Principal becomes claimable only at/after `maturity`.
///
/// MODES
///   * HOLD   (adapter == address(0)): funds sit idle until maturity; no external venue ever touches them.
///   * ROUTED (adapter != 0): the exact deposit is pushed to the chosen adapter, which lends it / provides
///     liquidity. At maturity the owner calls `settle`: the adapter approves and returns the amount due, and
///     this contract PULLS exactly that amount and pays it to the owner. Only that pull is measured, so money
///     arriving from anywhere else can never be credited to the wrong deposit [audit H-6].
///   HOLD exits are paid pro rata if the token itself shrank the held balance, exactly like DemandDeposits.
///
/// RISK (explicit, not hidden): a ROUTED time deposit is risk capital. A credit default or adapter fault
///   reduces what the OWNER OF THAT DEPOSIT receives; it can never touch another depositor's funds or any
///   demand deposit, because (a) deposits are routed one-by-one with exact amounts, (b) this contract holds
///   no allowance for adapters, (c) demand money lives in a different contract.
contract TimeDeposits is ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    uint64 public constant MIN_TERM = 1 days;
    uint64 public constant MAX_TERM = 5 * 365 days;
    /// @dev If an adapter's `recall` keeps reverting this long past maturity, the owner may write the
    ///      deposit off (receives whatever is in-contract = 0 for routed deposits) instead of waiting forever.
    uint64 public constant RECALL_GRACE = 30 days;

    struct Deposit {
        address owner;
        IERC20Min token;
        IYieldAdapter adapter; // address(0) => HOLD
        uint128 principal;
        uint64 maturity;
        bool closed;
        bytes32 handle; // adapter's position handle
    }

    Deposit[] public deposits;
    /// @notice Σ principal of HOLD deposits not yet closed (must stay fully backed by real balance).
    mapping(IERC20Min => uint256) public heldInContract;

    event Opened(uint256 indexed id, address indexed owner, address token, uint256 amount, uint64 maturity, address adapter, bytes32 handle);
    event Settled(uint256 indexed id, address indexed owner, address to, uint256 principal, uint256 paid);
    event WrittenOff(uint256 indexed id, address indexed owner, uint256 principal);

    error AmountZero();
    error BadTerm();
    error InvalidRecipient();
    error NotOwner();
    error AlreadyClosed();
    error NotMatured();
    error FeeOnTransferUnsupported();
    error ReserveBreached();
    error RecallFailed();

    function depositCount() external view returns (uint256) {
        return deposits.length;
    }

    function open(IERC20Min token, uint256 amount, uint64 term, IYieldAdapter adapter, bytes calldata adapterData)
        external
        nonReentrant
        returns (uint256 id)
    {
        if (amount == 0 || amount > type(uint128).max) revert AmountZero();
        if (term < MIN_TERM || term > MAX_TERM) revert BadTerm();
        uint64 maturity = uint64(block.timestamp) + term;

        // ---- pull funds (verified by delta)
        uint256 before = token.balanceOf(address(this));
        token.safeTransferFrom(msg.sender, address(this), amount);
        if (token.balanceOf(address(this)) - before != amount) revert FeeOnTransferUnsupported();

        // ---- effects
        id = deposits.length;
        deposits.push(Deposit(msg.sender, token, adapter, uint128(amount), maturity, false, bytes32(0)));

        bytes32 handle;
        if (address(adapter) == address(0)) {
            heldInContract[token] += amount;
        } else {
            // ---- interaction: push EXACT amount, then let the adapter book the position
            token.safeTransfer(address(adapter), amount);
            handle = adapter.deploy(token, amount, maturity, adapterData);
            deposits[id].handle = handle;
            // local check: exactly the routed amount left; nothing held for anyone else moved
            if (token.balanceOf(address(this)) < before) revert ReserveBreached();
        }
        emit Opened(id, msg.sender, address(token), amount, maturity, address(adapter), handle);
    }

    /// @notice At/after maturity, the owner collects principal (HOLD) or the adapter's return (ROUTED).
    function settle(uint256 id, address to) external nonReentrant {
        Deposit storage d = deposits[id];
        if (d.owner != msg.sender) revert NotOwner();
        if (d.closed) revert AlreadyClosed();
        if (block.timestamp < d.maturity) revert NotMatured();
        if (to == address(0)) revert InvalidRecipient();

        IERC20Min token = d.token;
        uint256 principal = d.principal;
        uint256 paid;

        // ---- effects BEFORE any external call
        d.closed = true;

        if (address(d.adapter) == address(0)) {
            uint256 held = heldInContract[token];
            heldInContract[token] = held - principal;
            uint256 bal = token.balanceOf(address(this));
            paid = bal >= held ? principal : principal * bal / held; // [audit M-1]: pro rata, never frozen
        } else {
            try d.adapter.recall(d.handle) returns (uint256 amount) {
                // [audit H-6]: credit ONLY what THIS deposit's adapter hands over, in a pull executed here. A balance
                // delta around an arbitrary external call also captures anyone else's money that arrives meanwhile.
                uint256 before = token.balanceOf(address(this));
                if (amount > 0) token.safeTransferFrom(address(d.adapter), address(this), amount);
                if (token.balanceOf(address(this)) - before != amount) revert FeeOnTransferUnsupported();
                paid = amount;
            } catch {
                if (block.timestamp < d.maturity + RECALL_GRACE) revert RecallFailed();
                emit WrittenOff(id, msg.sender, principal);
                return; // closed stays true: owner's claim is abandoned, nobody else's money is touched
            }
        }
        if (paid > 0) {
            uint256 pre = token.balanceOf(address(this));
            token.safeTransfer(to, paid);
            if (pre - token.balanceOf(address(this)) != paid) revert FeeOnTransferUnsupported(); // exact outflow
        }
        emit Settled(id, msg.sender, to, principal, paid);
    }
}

// -----------------------------------------------------------------------------------------------------
//  3. FACADE
// -----------------------------------------------------------------------------------------------------

/// @title SovereignBank
/// @notice Deploys the two infrastructures once and records their addresses. It holds no funds, has no
///         admin and no function that touches either vault: it is a registry, nothing more.
contract SovereignBank {
    DemandDeposits public immutable demand;
    TimeDeposits public immutable time;

    constructor() {
        demand = new DemandDeposits();
        time = new TimeDeposits();
    }
}
