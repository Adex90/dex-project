// DarkPool curve maths and the flashRebalance PLANNER.
// Everything here is an off-chain ESTIMATE in token units (double). The chain re-checks every rule exactly;
// the planner therefore leans conservative (round repayments up, payouts down, add a Treasury margin) and the
// engine always runs an eth_call preflight before sending.
#pragma once
#include <algorithm>
#include <cmath>
#include <limits>

namespace sb {

/// Mirrors DarkPool: (R0+v0)(R1+v1)=K.
struct PoolState {
  double r0, r1, v0, v1;
  double x() const { return r0 + v0; }
  double y() const { return r1 + v1; }
  double k() const { return x() * y(); }
  double spot() const { return y() / x(); }  // token1 per token0
};

struct PlannerParams {
  double extractionBps = 8000;      // DarkPool.EXTRACTION_BPS
  double treasuryMarginBps = 5;     // overpay a hair: the oracle price may tick before inclusion
  double gasCostQuote = 0;          // expected gas in token1 units
  double minNet = 0;                // minimum expected profit worth the risk
  double maxInv0 = 0, maxInv1 = 0;  // executor inventory available to deliver
};

/// A concrete rebalance: what the pool sends out, what the executor must return, what the Treasury gets.
struct Plan {
  bool ok = false;
  bool sellsToken0 = false;  // true: pool price > oracle, we SELL token0 into the pool
  double amount0Out = 0, amount1Out = 0;
  double pay0 = 0, pay1 = 0, treasuryPay = 0;
  double harvest = 0;        // value the pool loses at oracle price = our gross gain (token1)
  double edge = 0;           // gain measured against the AS bid/ask
  double net = 0;           // edge − treasury − gas
  double targetPrice = 0;
};

namespace detail {

struct Eval { double in0, out0, in1, out1, harvest, pay, edge, net; bool feasible; };

inline Eval evalAt(const PoolState& s, double pt, bool sell, double oracleP, double bid, double ask, const PlannerParams& pp) {
  const double k = s.k();
  const double xt = std::sqrt(k / pt), yt = std::sqrt(k * pt);
  Eval e{};
  if (sell) {  // pool price too high: we give token0, take token1
    e.in0 = xt - s.x();
    e.out1 = s.y() - yt;
    e.harvest = e.out1 - e.in0 * oracleP;
    e.edge = e.out1 - e.in0 * ask;
    e.feasible = e.in0 > 0 && e.out1 > 0 && e.out1 < s.r1 && e.in0 <= pp.maxInv0;
  } else {     // pool price too low: we give token1, take token0
    e.in1 = yt - s.y();
    e.out0 = s.x() - xt;
    e.harvest = e.out0 * oracleP - e.in1;
    e.edge = e.out0 * bid - e.in1;
    e.feasible = e.in1 > 0 && e.out0 > 0 && e.out0 < s.r0;
  }
  e.pay = std::max(0.0, e.harvest) * (pp.extractionBps / 1e4) * (1.0 + pp.treasuryMarginBps / 1e4);
  if (!sell) e.feasible = e.feasible && (e.in1 + e.pay <= pp.maxInv1);
  e.net = e.edge - e.pay - pp.gasCostQuote;
  return e;
}

}  // namespace detail

/// Chooses the target price pt (how far to push the pool) that maximises expected net profit.
///   sell case: pt ∈ [oracle, spot)   buy case: pt ∈ (spot, oracle]    — always TOWARD the oracle, never past
/// (the contract requires |spot'−P| < |spot−P|). Net profit is smooth and single-peaked in pt, so a ternary
/// search finds the optimum; with no Treasury cut and no costs the peak is exactly pt = our ask/bid.
inline Plan plan(const PoolState& s, double oracleP, double bid, double ask, const PlannerParams& pp) {
  Plan out;
  const double spot = s.spot();
  if (!(oracleP > 0) || !(spot > 0) || std::fabs(spot - oracleP) / oracleP < 1e-9) return out;
  const bool sell = spot > oracleP;

  double lo = sell ? oracleP : spot, hi = sell ? spot : oracleP;
  // shrink the interval so every probe is feasible (inventory / reserve limits are monotone in pt)
  const double eps = 1e-9 * oracleP;
  if (sell) { lo += eps; hi -= eps; } else { lo += eps; hi -= eps; }
  if (!(lo < hi)) return out;
  for (int i = 0; i < 80; ++i) {  // bisection for the extreme feasible end
    const double probe = sell ? lo : hi;
    if (detail::evalAt(s, probe, sell, oracleP, bid, ask, pp).feasible) break;
    if (sell) lo = lo + (hi - lo) * 0.5; else hi = hi - (hi - lo) * 0.5;
    if (hi - lo < eps) return out;
  }
  for (int i = 0; i < 200; ++i) {
    const double a = lo + (hi - lo) / 3.0, b = hi - (hi - lo) / 3.0;
    const auto ea = detail::evalAt(s, a, sell, oracleP, bid, ask, pp);
    const auto eb = detail::evalAt(s, b, sell, oracleP, bid, ask, pp);
    const double na = ea.feasible ? ea.net : -std::numeric_limits<double>::infinity();
    const double nb = eb.feasible ? eb.net : -std::numeric_limits<double>::infinity();
    if (na < nb) lo = a; else hi = b;
  }
  const double pt = 0.5 * (lo + hi);
  const auto e = detail::evalAt(s, pt, sell, oracleP, bid, ask, pp);
  if (!e.feasible || e.net <= pp.minNet) return out;

  out.ok = true; out.sellsToken0 = sell; out.targetPrice = pt;
  out.amount0Out = e.out0; out.amount1Out = e.out1;
  out.pay0 = e.in0; out.pay1 = e.in1; out.treasuryPay = e.pay;
  out.harvest = e.harvest; out.edge = e.edge; out.net = e.net;
  return out;
}

/// Independent re-implementation of the three DarkPool.flashRebalance acceptance rules (I1, I3a, I3b),
/// used by tests and by the dry-run mock chain. Inputs are the amounts that will actually be sent.
struct RuleCheck { bool kOk, deviationOk, treasuryOk; bool all() const { return kOk && deviationOk && treasuryOk; } };

inline RuleCheck chainRules(const PoolState& s, double oracleP, double out0, double out1, double in0, double in1, double treasuryPaid) {
  const double n0 = s.r0 - out0 + in0, n1 = s.r1 - out1 + in1;
  const double x = n0 + s.v0, y = n1 + s.v1;
  RuleCheck r{};
  r.kOk = x * y >= s.k() * (1.0 - 1e-12);  // tolerance for double rounding; chain is exact
  const double devBefore = std::fabs(s.spot() - oracleP) / oracleP, devAfter = std::fabs(y / x - oracleP) / oracleP;
  r.deviationOk = devAfter < devBefore;
  const double valueBefore = s.x() * oracleP + s.y(), valueAfter = x * oracleP + y;
  r.treasuryOk = treasuryPaid >= std::max(0.0, valueBefore - valueAfter) * 0.8 * (1.0 - 1e-12);
  return r;
}

}  // namespace sb
