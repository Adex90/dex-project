// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {NoteTree} from "../privacy/NoteTree.sol";
import {ReentrancyGuard} from "../utils/ReentrancyGuard.sol";
import {SafeTransfer} from "../utils/SafeTransfer.sol";
import {IERC20Min} from "../interfaces/IERC20Min.sol";
import {IZKVerifier} from "../interfaces/IZKVerifier.sol";
import {IPoseidonHasher} from "../interfaces/IPoseidonHasher.sol";

/// @title OptionsMarket — Module 3: fully-collateralised European options (ZK-ERC1155)
///
/// PRODUCT
///   One *series* = (underlying, quote, underlyingPerContract U, quotePerContract Q, expiry, call|put).
///   The strike is implied: Q / U. The long side is a fungible ERC-1155-style token whose id is the series
///   id. All sizes are INTEGER numbers of contracts, so every amount below is an exact product
///   (qty × U or qty × Q): there is no rounding anywhere, hence no dust insolvency.
///
///     CALL  collateral = U underlying per contract. Exercise: holder pays Q quote, receives U underlying.
///     PUT   collateral = Q quote      per contract. Exercise: holder pays U underlying, receives Q quote.
///
/// AXIOM — TOTAL COVERAGE (naked options are impossible by construction)
///   * The ONLY way long tokens come into existence is `mintOptionToken`, which pulls 100 % of the
///     collateral from the writer in the same call and reverts unless the contract actually received the
///     exact amount (fee-on-transfer tokens are rejected). There is no other mint path, no admin, no
///     credit, no margin, no re-hypothecation: collateral is never lent or swapped.
///   * Solvency invariant, enforced after every entrypoint that moves tokens (I-SOLV):
///         token.balanceOf(this) >= liabilities[token]
///     where `liabilities` = Σ collateralHeld + Σ proceedsHeld of every series using that token.
///   * Supply invariant (until the post-expiry writer-claim phase begins) (I-SUPPLY):
///         totalSupply[id] + exercised[id] == written[id]
///     and  totalSupply[id] == Σ public balances + shieldedSupply[id].
///
/// EUROPEAN EXERCISE
///   Exercise is possible only inside [expiry, expiry + EXERCISE_WINDOW). Never before expiry.
///   Settlement is PHYSICAL (delivery against payment), so no price oracle is needed and none can be
///   manipulated. Out-of-the-money options are simply not exercised and expire worthless; there is no
///   auto-exercise (holders must act inside the window).
///   After the window, writers call `claimWriter`: each receives the pro-rata share (by contracts
///   written) of the series' remaining collateral AND of the exercise proceeds. Exercises are therefore
///   assigned pro-rata to all writers of the series, deterministically and without randomness.
///
/// ZK-ERC1155 (privacy)
///   Long tokens exist in two domains: public (`balanceOf`) and shielded notes (NoteTree join-split with
///   `assetId = series id`). `transferZK` moves option tokens without revealing sender, receiver or size.
///   `shield` / `unshield` are the only bridges and reveal their amount. Exercise and closing operate on
///   public balances, so a shielded holder must `unshield` (to a stealth address) before exercising.
///   WRITERS ARE PUBLIC: their collateral deposit is visible by necessity; use a stealth/fresh address.
///   The public subset implements the ERC-1155 views/transfers but intentionally NO receiver hooks
///   (`onERC1155Received`): hooks are a classic reentrancy vector and this contract is pull-only.
///
/// NOT AUDITED. Tokens must be plain ERC-20 (no fee-on-transfer, no rebasing, no hooks).
contract OptionsMarket is NoteTree, ReentrancyGuard {
    using SafeTransfer for IERC20Min;

    // ------------------------------------------------------------------ parameters
    uint64 public constant EXERCISE_WINDOW = 1 days;

    // ------------------------------------------------------------------ types
    struct SeriesParams {
        IERC20Min underlying;
        IERC20Min quote;
        uint128 underlyingPerContract; // U, in underlying base units
        uint128 quotePerContract; // Q, in quote base units
        uint64 expiry; // unix seconds
        bool isCall;
    }

    struct Series {
        IERC20Min underlying;
        IERC20Min quote;
        uint128 underlyingPerContract;
        uint128 quotePerContract;
        uint64 expiry;
        bool isCall;
        bool exists;
    }

    // ------------------------------------------------------------------ storage
    mapping(uint256 => Series) public series;
    mapping(uint256 => uint256) public written; // contracts written and not yet claimed
    mapping(uint256 => uint256) public exercised; // contracts exercised
    mapping(uint256 => uint256) public totalSupply; // long tokens outstanding (public + shielded)
    mapping(uint256 => uint256) public shieldedSupply;
    mapping(uint256 => uint256) public collateralHeld; // in the series' collateral token
    mapping(uint256 => uint256) public proceedsHeld; // in the series' proceeds token
    mapping(uint256 => mapping(address => uint256)) public writtenBy;
    mapping(uint256 => mapping(address => uint256)) public balanceOf; // id => holder => amount
    mapping(address => mapping(address => bool)) public isApprovedForAll;
    mapping(address => uint256) public liabilities; // token => Σ owed to writers / exercisers

    // ------------------------------------------------------------------ events / errors
    event SeriesCreated(uint256 indexed id, address underlying, address quote, uint256 u, uint256 q, uint64 expiry, bool isCall);
    event Written(uint256 indexed id, address indexed writer, address indexed to, uint256 qty, uint256 collateral);
    event Closed(uint256 indexed id, address indexed writer, uint256 qty, uint256 collateralReturned);
    event Exercised(uint256 indexed id, address indexed holder, address indexed to, uint256 qty, uint256 paid, uint256 received);
    event WriterClaimed(uint256 indexed id, address indexed writer, uint256 collateralOut, uint256 proceedsOut);
    event TransferSingle(address indexed operator, address indexed from, address indexed to, uint256 id, uint256 value);
    event ApprovalForAll(address indexed account, address indexed operator, bool approved);
    event Shielded(uint256 indexed id, address indexed from, uint256 amount);
    event Unshielded(uint256 indexed id, address indexed to, uint256 amount);

    error BadSeries();
    error UnknownSeries();
    error Expired();
    error NotExpired();
    error OutsideExerciseWindow();
    error ExerciseWindowOpen();
    error AmountZero();
    error InsufficientBalance();
    error NotAuthorized();
    error InvalidRecipient();
    error CollateralShortfall(); // received != required: the 100 % coverage gate
    error Insolvent();
    error NothingToClaim();
    error ShieldedSupplyExceeded();

    constructor(IZKVerifier verifier_, IPoseidonHasher hasher_) NoteTree(verifier_, hasher_) {}

    // ================================================================== series id
    /// @notice Deterministic id, reduced into the BN254 field so it can be a proof `assetId`.
    function seriesId(SeriesParams memory p) public pure returns (uint256) {
        return uint256(keccak256(abi.encode(p))) % FIELD;
    }

    // ================================================================== writing (100 % collateral)
    /// @notice Writes `qty` contracts and mints `qty` long tokens to `to`. The caller (the writer) MUST
    ///         deposit 100 % of the collateral in this call. Creates the series on first use.
    /// @dev    CEI: all accounting is committed BEFORE the token pull; the pull is then verified by
    ///         balance delta, and any shortfall reverts the whole transaction (accounting included).
    function mintOptionToken(SeriesParams calldata p, uint256 qty, address to)
        external
        nonReentrant
        returns (uint256 id)
    {
        if (qty == 0) revert AmountZero();
        if (to == address(0)) revert InvalidRecipient();
        id = _ensureSeries(p);
        Series storage s = series[id];
        if (block.timestamp >= s.expiry) revert Expired();

        IERC20Min cToken = _collateralToken(s);
        uint256 required = qty * _collateralPer(s); // exact: integer contracts

        // ---- effects
        written[id] += qty;
        writtenBy[id][msg.sender] += qty;
        totalSupply[id] += qty;
        balanceOf[id][to] += qty;
        collateralHeld[id] += required;
        liabilities[address(cToken)] += required;

        // ---- interaction + the coverage gate
        uint256 before = cToken.balanceOf(address(this));
        cToken.safeTransferFrom(msg.sender, address(this), required);
        if (cToken.balanceOf(address(this)) - before != required) revert CollateralShortfall();
        _requireSolvent(cToken);

        emit TransferSingle(msg.sender, address(0), to, id, qty);
        emit Written(id, msg.sender, to, qty, required);
    }

    /// @notice Writer burns `qty` long tokens from their own public balance before expiry and recovers the
    ///         matching collateral (closing a position). Cannot be used after expiry.
    function closePosition(uint256 id, uint256 qty) external nonReentrant {
        Series storage s = _series(id);
        if (qty == 0) revert AmountZero();
        if (block.timestamp >= s.expiry) revert Expired();
        if (writtenBy[id][msg.sender] < qty || balanceOf[id][msg.sender] < qty) revert InsufficientBalance();

        IERC20Min cToken = _collateralToken(s);
        uint256 refund = qty * _collateralPer(s);

        written[id] -= qty;
        writtenBy[id][msg.sender] -= qty;
        totalSupply[id] -= qty;
        balanceOf[id][msg.sender] -= qty;
        collateralHeld[id] -= refund;
        liabilities[address(cToken)] -= refund;

        cToken.safeTransfer(msg.sender, refund);
        _requireSolvent(cToken);
        emit TransferSingle(msg.sender, msg.sender, address(0), id, qty);
        emit Closed(id, msg.sender, qty, refund);
    }

    // ================================================================== European exercise
    /// @notice Exercise `qty` long tokens held PUBLICLY by the caller. Only inside the exercise window.
    ///         Delivery-versus-payment: the holder pays the "proceeds" leg and receives the collateral leg.
    function exercise(uint256 id, uint256 qty, address to) external nonReentrant {
        Series storage s = _series(id);
        if (qty == 0) revert AmountZero();
        if (to == address(0)) revert InvalidRecipient();
        if (block.timestamp < s.expiry || block.timestamp >= s.expiry + EXERCISE_WINDOW) {
            revert OutsideExerciseWindow();
        }
        if (balanceOf[id][msg.sender] < qty) revert InsufficientBalance();

        IERC20Min cToken = _collateralToken(s);
        IERC20Min pToken = _proceedsToken(s);
        uint256 pay = qty * _proceedsPer(s); // holder -> contract (strike leg or underlying leg)
        uint256 receive_ = qty * _collateralPer(s); // contract -> holder

        // ---- effects
        balanceOf[id][msg.sender] -= qty;
        totalSupply[id] -= qty;
        exercised[id] += qty;
        collateralHeld[id] -= receive_;
        proceedsHeld[id] += pay;
        liabilities[address(cToken)] -= receive_;
        liabilities[address(pToken)] += pay;

        // ---- interactions: payment first, then delivery
        uint256 before = pToken.balanceOf(address(this));
        pToken.safeTransferFrom(msg.sender, address(this), pay);
        if (pToken.balanceOf(address(this)) - before != pay) revert CollateralShortfall();
        cToken.safeTransfer(to, receive_);
        _requireSolvent(cToken);
        _requireSolvent(pToken);

        emit TransferSingle(msg.sender, msg.sender, address(0), id, qty);
        emit Exercised(id, msg.sender, to, qty, pay, receive_);
    }

    /// @notice After the window, a writer withdraws their pro-rata share of what is left of the series:
    ///         remaining collateral (unexercised contracts) + exercise proceeds.
    /// @dev    Share = held × writtenBy / written, with `written` decremented per claim, so the last
    ///         claimant receives the exact remainder (no dust left behind, no over-payment possible).
    function claimWriter(uint256 id, address to) external nonReentrant {
        Series storage s = _series(id);
        if (to == address(0)) revert InvalidRecipient();
        if (block.timestamp < s.expiry + EXERCISE_WINDOW) revert ExerciseWindowOpen();

        uint256 w = writtenBy[id][msg.sender];
        if (w == 0) revert NothingToClaim();
        uint256 total = written[id];
        IERC20Min cToken = _collateralToken(s);
        IERC20Min pToken = _proceedsToken(s);
        uint256 cOut = collateralHeld[id] * w / total;
        uint256 pOut = proceedsHeld[id] * w / total;

        writtenBy[id][msg.sender] = 0;
        written[id] = total - w;
        collateralHeld[id] -= cOut;
        proceedsHeld[id] -= pOut;
        liabilities[address(cToken)] -= cOut;
        liabilities[address(pToken)] -= pOut;

        if (cOut > 0) cToken.safeTransfer(to, cOut);
        if (pOut > 0) pToken.safeTransfer(to, pOut);
        _requireSolvent(cToken);
        _requireSolvent(pToken);
        emit WriterClaimed(id, msg.sender, cOut, pOut);
    }

    // ================================================================== public ERC-1155 subset (no hooks)
    function setApprovalForAll(address operator, bool approved) external {
        isApprovedForAll[msg.sender][operator] = approved;
        emit ApprovalForAll(msg.sender, operator, approved);
    }

    function safeTransferFrom(address from, address to, uint256 id, uint256 amount, bytes calldata) external {
        if (from != msg.sender && !isApprovedForAll[from][msg.sender]) revert NotAuthorized();
        if (to == address(0) || to == address(this)) revert InvalidRecipient();
        uint256 bal = balanceOf[id][from];
        if (bal < amount) revert InsufficientBalance();
        unchecked {
            balanceOf[id][from] = bal - amount;
        }
        balanceOf[id][to] += amount;
        emit TransferSingle(msg.sender, from, to, id, amount);
    }

    // ================================================================== shielded domain (ZK-ERC1155)
    /// @notice public -> shielded notes of asset `id`. Reveals `amount`.
    function shield(uint256 id, uint256 amount, JoinSplit calldata js) external nonReentrant {
        _series(id);
        if (amount == 0) revert AmountZero();
        _applyJoinSplit(js, amount, 0, _toField(abi.encode(msg.sender, js.encryptedNotes[0], js.encryptedNotes[1])), id);
        uint256 bal = balanceOf[id][msg.sender];
        if (bal < amount) revert InsufficientBalance();
        unchecked {
            balanceOf[id][msg.sender] = bal - amount;
        }
        shieldedSupply[id] += amount;
        emit Shielded(id, msg.sender, amount);
    }

    /// @notice shielded -> shielded. Hides sender, receiver and size. Anyone may relay it.
    function transferZK(uint256 id, JoinSplit calldata js) external nonReentrant {
        _series(id);
        _applyJoinSplit(js, 0, 0, _toField(abi.encode(address(0), js.encryptedNotes[0], js.encryptedNotes[1])), id);
    }

    /// @notice shielded -> public balance of `recipient` (bound into the proof). Reveals `amount`.
    function unshield(uint256 id, uint256 amount, address recipient, JoinSplit calldata js) external nonReentrant {
        _series(id);
        if (amount == 0) revert AmountZero();
        if (recipient == address(0) || recipient == address(this)) revert InvalidRecipient();
        // Back-stop independent of circuit soundness: cannot release more than was ever shielded.
        if (amount > shieldedSupply[id]) revert ShieldedSupplyExceeded();
        _applyJoinSplit(js, 0, amount, _toField(abi.encode(recipient, js.encryptedNotes[0], js.encryptedNotes[1])), id);
        shieldedSupply[id] -= amount;
        balanceOf[id][recipient] += amount;
        emit Unshielded(id, recipient, amount);
    }

    // ================================================================== internals
    function _ensureSeries(SeriesParams calldata p) private returns (uint256 id) {
        id = seriesId(p);
        if (series[id].exists) return id;
        if (
            address(p.underlying) == address(0) || address(p.quote) == address(0)
                || address(p.underlying) == address(p.quote) || p.underlyingPerContract == 0
                || p.quotePerContract == 0 || p.expiry <= block.timestamp
        ) revert BadSeries();
        series[id] = Series({
            underlying: p.underlying,
            quote: p.quote,
            underlyingPerContract: p.underlyingPerContract,
            quotePerContract: p.quotePerContract,
            expiry: p.expiry,
            isCall: p.isCall,
            exists: true
        });
        emit SeriesCreated(id, address(p.underlying), address(p.quote), p.underlyingPerContract, p.quotePerContract, p.expiry, p.isCall);
    }

    function _series(uint256 id) private view returns (Series storage s) {
        s = series[id];
        if (!s.exists) revert UnknownSeries();
    }

    // Call: collateral = underlying, proceeds = quote.  Put: collateral = quote, proceeds = underlying.
    function _collateralToken(Series storage s) private view returns (IERC20Min) {
        return s.isCall ? s.underlying : s.quote;
    }

    function _proceedsToken(Series storage s) private view returns (IERC20Min) {
        return s.isCall ? s.quote : s.underlying;
    }

    function _collateralPer(Series storage s) private view returns (uint256) {
        return s.isCall ? s.underlyingPerContract : s.quotePerContract;
    }

    function _proceedsPer(Series storage s) private view returns (uint256) {
        return s.isCall ? s.quotePerContract : s.underlyingPerContract;
    }

    /// @dev I-SOLV
    function _requireSolvent(IERC20Min t) private view {
        if (t.balanceOf(address(this)) < liabilities[address(t)]) revert Insolvent();
    }
}
