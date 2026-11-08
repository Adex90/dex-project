// DarkPool curve maths and the flashRebalance PLANNER.
// Everything here is an off-chain ESTIMATE in token units (double). The chain re-checks every rule exactly;
// the planner therefore leans conservative (round repayments up, payouts down, add a Treasury margin) and the
// engine always runs an eth_call preflight before sending.
#pragma once
#include <algorithm>
#include <cmath>

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
  double extractionBps = 8000;      // DarkPool.EXTRACTION_BPS: the pool PULLS exactly this share of the harvest
  double treasuryMarginBps = 5;     // headroom on the allowance (the cap) in case the oracle ticks before inclusion
  double gasCostQuote = 0;          // expected gas in token1 units
  double minNet = 0;                // minimum expected profit worth the risk
  double maxInv0 = 0, maxInv1 = 0;  // executor inventory available to deliver
};

/// A concrete rebalance: what the pool sends out, what the executor must return, and the Treasury allowance.
struct Plan {
  bool ok = false;
  bool sellsToken0 = false;  // true: pool price > oracle, we SELL token0 into the pool
  double amount0Out = 0, amount1Out = 0;
  double pay0 = 0, pay1 = 0;
  double treasuryPay = 0;    // allowance for the pool's pull (a cap); the pool takes exactly harvest · extraction
  double harvest = 0;        // value the pool loses at oracle price = our gross gain (token1)
  double edge = 0;           // gain measured against the AS bid/ask
  double net = 0;            // edge − Treasury cut − gas
  double targetPrice = 0;
};

namespace detail {

struct Eval { double in0, out0, in1, out1, harvest, cut, cap, edge, net; bool feasible; };

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
  e.cut = std::max(0.0, e.harvest) * (pp.extractionBps / 1e4);
  e.cap = e.cut * (1.0 + pp.treasuryMarginBps / 1e4);
  if (!sell) e.feasible = e.feasible && (e.in1 + e.cap <= pp.maxInv1);
  e.net = e.edge - e.cut - pp.gasCostQuote;
  return e;
}

}  // namespace detail

/// Chooses the target price pt (how far to push the pool) that maximises net profit — IN CLOSED FORM.
///   sell (spot > P):  net(x_t) = (1−φ)(y − K/x_t) − (x_t − x)(ask − φP) − gas   ⇒   p* = (ask − φP)/(1 − φ)
///   buy  (spot < P):  net(y_t) = (x − K/y_t)(bid − φP) − (y_t − y)(1 − φ) − gas  ⇒   p* = (bid − φP)/(1 − φ)
/// with φ = extraction share. Both objectives are concave in x_t (resp. y_t), so the optimum over the feasible interval
/// is p* clamped to it, and that interval is closed-form too:
///   sell: pt ≥ K/(x + inv0)² (inventory), pt > (y − r1)²/K (real reserve), pt ∈ (P, spot)
///   buy : pt < K/(x − r0)² (real reserve), √pt ≤ u⁺ where u⁺ is the positive root of
///         (1−φc)√K·u² − ((1−φc)y − φc·P·x + inv1)·u − φc·P·√K = 0   (repayment + capped cut ≤ inventory), pt ∈ (spot, P)
/// Always TOWARD the oracle, never past it (the contract requires |spot'−P| < |spot−P|). ~25 ns, exact: the previous
/// 80-step "bisection" + 200-step ternary search took ~5–10 µs and could stop up to half an interval short of the
/// feasibility boundary, leaving up to 23 % of the profit (Phase-8 audit Q-3).
inline Plan plan(const PoolState& s, double oracleP, double bid, double ask, const PlannerParams& pp) {
  Plan out;
  const double spot = s.spot();
  if (!(oracleP > 0) || !(spot > 0) || std::fabs(spot - oracleP) / oracleP < 1e-9) return out;
  const bool sell = spot > oracleP;
  const double phi = pp.extractionBps / 1e4, phiCap = phi * (1.0 + pp.treasuryMarginBps / 1e4);
  const double K = s.k(), eps = 1e-9 * oracleP;
  double lo = sell ? oracleP + eps : spot + eps, hi = sell ? spot - eps : oracleP - eps;
  if (sell) {
    const double xmax = s.x() + pp.maxInv0;
    lo = std::max(lo, K / (xmax * xmax));
    const double ymin = s.y() - s.r1;
    if (ymin > 0) lo = std::max(lo, (ymin * ymin / K) * (1 + 1e-12));
  } else {
    const double xmin = s.x() - s.r0;
    if (xmin > 0) hi = std::min(hi, (K / (xmin * xmin)) * (1 - 1e-12));
    const double sK = std::sqrt(K), a = (1 - phiCap) * sK, b = -((1 - phiCap) * s.y() - phiCap * oracleP * s.x() + pp.maxInv1),
                 c = -phiCap * oracleP * sK;
    const double u = (-b + std::sqrt(b * b - 4 * a * c)) / (2 * a);
    hi = std::min(hi, u * u * (1 - 1e-12));
  }
  if (!(lo < hi)) return out;
  const double pt = std::clamp(((sell ? ask : bid) - phi * oracleP) / (1 - phi), lo, hi);
  const auto e = detail::evalAt(s, pt, sell, oracleP, bid, ask, pp);
  if (!e.feasible || e.net <= pp.minNet) return out;

  out.ok = true; out.sellsToken0 = sell; out.targetPrice = pt;
  out.amount0Out = e.out0; out.amount1Out = e.out1;
  out.pay0 = e.in0; out.pay1 = e.in1; out.treasuryPay = e.cap;
  out.harvest = e.harvest; out.edge = e.edge; out.net = e.net;
  return out;
}

/// Independent re-implementation of the three DarkPool.flashRebalance acceptance rules (I1, I3a, I3b),
/// used by tests and by the dry-run mock chain. Inputs are the amounts that will actually be sent.
/// `cut` is what DarkPool pulls (80 % of the harvest at P); treasuryOk = the allowance (`treasuryPaid`) covers it.
struct RuleCheck { bool kOk, deviationOk, treasuryOk; double cut; bool all() const { return kOk && deviationOk && treasuryOk; } };

inline RuleCheck chainRules(const PoolState& s, double oracleP, double out0, double out1, double in0, double in1, double treasuryPaid) {
  const double n0 = s.r0 - out0 + in0, n1 = s.r1 - out1 + in1;
  const double x = n0 + s.v0, y = n1 + s.v1;
  RuleCheck r{};
  r.kOk = x * y >= s.k() * (1.0 - 1e-12);  // tolerance for double rounding; chain is exact
  const double devBefore = std::fabs(s.spot() - oracleP) / oracleP, devAfter = std::fabs(y / x - oracleP) / oracleP;
  r.deviationOk = devAfter < devBefore;
  const double valueBefore = s.x() * oracleP + s.y(), valueAfter = x * oracleP + y;
  r.cut = std::max(0.0, valueBefore - valueAfter) * 0.8;
  r.treasuryOk = treasuryPaid >= r.cut * (1.0 - 1e-12);
  return r;
}

}  // namespace sb
