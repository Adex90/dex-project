// Avellaneda–Stoikov (2008) inventory control, plus the on-line estimators it needs.
#pragma once
#include <algorithm>
#include <cmath>

namespace sb {

/// Model:  dS = σ dW,  order arrival intensity λ(δ) = A·e^(−κδ),  CARA utility with risk aversion γ.
///   reservation price   r(s,q,t) = s − q·γ·σ²·(T−t)
///   optimal total spread  δ_a+δ_b = γ·σ²·(T−t) + (2/γ)·ln(1 + γ/κ)
/// q is the signed inventory (token0 held minus target), so a LONG inventory pulls r below s: the bot
/// becomes keener to SELL and reluctant to BUY, which is exactly the skew we feed into the arbitrage planner.
struct ASParams {
  double gamma = 0.1;     // risk aversion (per unit of quote currency); higher ⇒ stronger inventory skew
  double horizonSec = 60; // (T−t): rolling horizon, re-armed every tick (stationary approximation)
  double minKappa = 1e-6;
};

struct Quotes {
  double reservation;  // r
  double halfSpread;   // δ/2
  double bid() const { return reservation - halfSpread; }
  double ask() const { return reservation + halfSpread; }
};

inline Quotes quotes(const ASParams& p, double s, double q, double sigma2PerSec, double kappa) {
  const double k = std::max(kappa, p.minKappa);
  const double tau = p.horizonSec;
  const double r = s - q * p.gamma * sigma2PerSec * tau;
  const double spread = p.gamma * sigma2PerSec * tau + (2.0 / p.gamma) * std::log1p(p.gamma / k);
  return {r, 0.5 * spread};
}

/// EWMA estimator of price variance PER SECOND from irregularly spaced observations.
class VarianceEstimator {
 public:
  explicit VarianceEstimator(double halfLifeSec) : lambdaPerSec_(std::log(2.0) / halfLifeSec) {}
  void update(double tSec, double price) {
    if (have_ && tSec > t_) {
      const double dt = tSec - t_;
      const double dp = price - p_;
      const double w = 1.0 - std::exp(-lambdaPerSec_ * dt);
      var_ = (1.0 - w) * var_ + w * (dp * dp / dt);
    }
    t_ = tSec; p_ = price; have_ = true;
  }
  double varPerSec() const { return var_; }

 private:
  double lambdaPerSec_, var_ = 0, t_ = 0, p_ = 0;
  bool have_ = false;
};

/// κ from observed execution depths. If arrivals are Poisson with λ(δ)=A·e^(−κδ), the distance δ between
/// mid and the price at which market orders execute is Exponential(κ), so the MLE is κ̂ = 1 / mean(δ).
/// Feed it |trade price − mid| for each public trade; EWMA keeps it adaptive.
class KappaEstimator {
 public:
  explicit KappaEstimator(double alpha = 0.02, double init = 1.0) : alpha_(alpha), meanDepth_(1.0 / init) {}
  void onTrade(double depthFromMid) { meanDepth_ = (1.0 - alpha_) * meanDepth_ + alpha_ * std::max(depthFromMid, 1e-12); }
  double kappa() const { return 1.0 / meanDepth_; }

 private:
  double alpha_, meanDepth_;
};

}  // namespace sb
