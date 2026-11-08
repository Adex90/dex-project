// The Searcher: LOB signals → Avellaneda–Stoikov quotes → curve planner → preflight → send.
#pragma once
#include <cmath>
#include <optional>
#include <string>
#include "strawberry/abi.hpp"
#include "strawberry/avellaneda_stoikov.hpp"
#include "strawberry/chain.hpp"
#include "strawberry/curve.hpp"
#include "strawberry/lob.hpp"

namespace sb {

struct SearcherConfig {
  double tickSize = 0.01;             // reference venue's price tick (the order book is keyed by integer ticks)
  ASParams as;
  AlphaParams alpha;
  PlannerParams planner;
  double targetInventory0 = 0;       // desired token0 holding (AS "q = 0" point)
  double maxOracleDivergenceBps = 30; // zk oracle vs our LOB fair value; beyond this we assume one is wrong
  std::uint64_t maxOracleAgeSec = 45; // keep inside DarkPool.MAX_ORACLE_AGE (60 s) with latency headroom
  double varHalfLifeSec = 30, ofiHalfLifeSec = 5;
};

enum class Skip { None, NoBook, StaleOracle, OracleDivergence, NoEdge, PreflightFailed, SendFailed };

struct Decision {
  Skip skip = Skip::None;
  Plan plan;
  Calldata calldata;
  std::string txHash;
  Quotes quotes{0, 0};
  double fair = 0, q = 0;
};

class Searcher {
 public:
  explicit Searcher(SearcherConfig c) : cfg_(c), book_(c.tickSize), var_(c.varHalfLifeSec), ofi_(c.ofiHalfLifeSec) {}

  // ---- feed side (single thread) -------------------------------------------------------------------
  OrderBook& book() { return book_; }
  void onBookUpdate(double tSec) {
    if (!book_.valid()) return;
    ofi_.onTopOfBook(tSec, book_.bestBid(), book_.bidQty(), book_.bestAsk(), book_.askQty());
    var_.update(tSec, book_.mid());
  }
  void onPublicTrade(double price) {
    if (book_.valid()) kappa_.onTrade(std::fabs(price - book_.mid()));
  }

  // ---- decision side -------------------------------------------------------------------------------
  Decision tick(double nowSec, IChain& chain) {
    Decision d;
    if (!book_.valid()) { d.skip = Skip::NoBook; return d; }

    const OracleQuote oq = chain.readOracle();
    if (nowSec - static_cast<double>(oq.updatedAtSec) > static_cast<double>(cfg_.maxOracleAgeSec)) { d.skip = Skip::StaleOracle; return d; }

    d.fair = fairValue(book_, ofi_, cfg_.alpha);
    // Never act on a disagreement between two price sources: either the oracle or our feed is wrong.
    if (std::fabs(oq.price - d.fair) / d.fair * 1e4 > cfg_.maxOracleDivergenceBps) { d.skip = Skip::OracleDivergence; return d; }

    const Inventory inv = chain.readInventory();
    d.q = inv.token0 - cfg_.targetInventory0;
    d.quotes = quotes(cfg_.as, d.fair, d.q, var_.varPerSec(), kappa_.kappa());

    PlannerParams pp = cfg_.planner;
    pp.maxInv0 = inv.token0; pp.maxInv1 = inv.token1;
    d.plan = plan(chain.readPool(), oq.price, d.quotes.bid(), d.quotes.ask(), pp);
    if (!d.plan.ok) { d.skip = Skip::NoEdge; return d; }

    // Round in the pool's favour: take slightly less, return slightly more; the Treasury allowance is a cap (rounded up).
    const U128 o0 = toWei(d.plan.amount0Out, Round::Down), o1 = toWei(d.plan.amount1Out, Round::Down);
    const U128 p0 = toWei(d.plan.pay0 * (1 + 1e-9), Round::Up), p1 = toWei(d.plan.pay1 * (1 + 1e-9), Round::Up);
    const U128 tr = toWei(d.plan.treasuryPay, Round::Up);
    d.calldata = encodeRebalance(o0, o1, p0, p1, tr);

    if (!chain.preflight(d.calldata.view())) { d.skip = Skip::PreflightFailed; return d; }
    if (auto h = chain.send(d.calldata.view())) d.txHash = *h; else d.skip = Skip::SendFailed;
    return d;
  }

 private:
  SearcherConfig cfg_;
  OrderBook book_;
  VarianceEstimator var_;
  OfiTracker ofi_;
  KappaEstimator kappa_;
};

inline const char* toString(Skip s) {
  switch (s) {
    case Skip::None: return "sent";
    case Skip::NoBook: return "no-book";
    case Skip::StaleOracle: return "stale-oracle";
    case Skip::OracleDivergence: return "oracle-divergence";
    case Skip::NoEdge: return "no-edge";
    case Skip::PreflightFailed: return "preflight-failed";
    case Skip::SendFailed: return "send-failed";
  }
  return "?";
}

}  // namespace sb
