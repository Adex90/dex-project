// Limit-order-book state and short-horizon signal extraction (CEX / off-chain reference venue).
#pragma once
#include <algorithm>
#include <cmath>
#include <functional>
#include <map>
#include <optional>

namespace sb {

enum class Side { Bid, Ask };

/// Price-level book. qty == 0 deletes the level. Single-threaded by design: one feed thread owns it and
/// publishes immutable snapshots (Signal) to the strategy thread.
class OrderBook {
 public:
  void set(Side s, double price, double qty) {
    auto upd = [&](auto& m) {
      if (qty <= 0.0) m.erase(price); else m[price] = qty;
    };
    s == Side::Bid ? upd(bids_) : upd(asks_);
  }
  void clear() { bids_.clear(); asks_.clear(); }  // snapshot resync
  bool valid() const { return !bids_.empty() && !asks_.empty() && bestBid() < bestAsk(); }
  double bestBid() const { return bids_.begin()->first; }
  double bestAsk() const { return asks_.begin()->first; }
  double bidQty() const { return bids_.begin()->second; }
  double askQty() const { return asks_.begin()->second; }
  double mid() const { return 0.5 * (bestBid() + bestAsk()); }

  /// Size-weighted microprice (Stoikov 2018): leans toward the side with LESS resting size.
  ///   micro = (Pb·Qa + Pa·Qb) / (Qa + Qb)
  double micro() const {
    const double qb = bidQty(), qa = askQty();
    return (bestBid() * qa + bestAsk() * qb) / (qa + qb);
  }

  /// Depth imbalance over the top `n` levels, in [-1, 1]; +1 = all bid depth (buy pressure).
  double imbalance(std::size_t n) const {
    double b = 0, a = 0;
    std::size_t i = 0;
    for (auto it = bids_.begin(); it != bids_.end() && i < n; ++it, ++i) b += it->second;
    i = 0;
    for (auto it = asks_.begin(); it != asks_.end() && i < n; ++it, ++i) a += it->second;
    return (b + a) > 0 ? (b - a) / (b + a) : 0.0;
  }

 private:
  std::map<double, double, std::greater<>> bids_;  // best (highest) first
  std::map<double, double> asks_;                  // best (lowest) first
};

/// Order-flow imbalance, Cont–Kukanov–Stoikov (2014). Each best-quote change contributes
///   e = 1[Pb' >= Pb]·Qb' − 1[Pb' <= Pb]·Qb − 1[Pa' <= Pa]·Qa' + 1[Pa' >= Pa]·Qa
/// Positive e = net buying pressure. Contributions decay exponentially so the signal is short-horizon.
class OfiTracker {
 public:
  explicit OfiTracker(double halfLifeSec) : decayPerSec_(std::log(2.0) / halfLifeSec) {}

  void onTopOfBook(double tSec, double pb, double qb, double pa, double qa) {
    decay(tSec);
    if (prev_) {
      const auto& p = *prev_;
      const double eb = (pb >= p.pb ? qb : 0.0) - (pb <= p.pb ? p.qb : 0.0);
      const double ea = -(pa <= p.pa ? qa : 0.0) + (pa >= p.pa ? p.qa : 0.0);
      ofi_ += eb + ea;
    }
    prev_ = Top{pb, qb, pa, qa};
    // running scale of displayed size, used to normalise OFI into a dimensionless signal
    const double depth = 0.5 * (qb + qa);
    scale_ = scale_ == 0.0 ? depth : 0.99 * scale_ + 0.01 * depth;
  }

  /// Squashed to (-1, 1). Normalised by typical top-of-book size so it is comparable across regimes.
  double signal() const { return scale_ > 0 ? std::tanh(ofi_ / (3.0 * scale_)) : 0.0; }

 private:
  struct Top { double pb, qb, pa, qa; };
  void decay(double t) {
    if (last_ >= 0) ofi_ *= std::exp(-decayPerSec_ * std::max(0.0, t - last_));
    last_ = t;
  }
  double decayPerSec_;
  double ofi_ = 0, scale_ = 0, last_ = -1;
  std::optional<Top> prev_;
};

struct AlphaParams {
  std::size_t depthLevels = 5;
  double wImbalance = 0.4;   // weight of depth imbalance
  double wOfi = 0.6;         // weight of order-flow imbalance
  double maxTiltBps = 2.0;   // cap on how far alpha may move fair value, in bps of price
};

/// Fair-value estimate = microprice tilted by a bounded alpha. The tilt is deliberately small: this bot
/// is an arbitrageur, not a directional trader; alpha only breaks ties and avoids trading INTO flow.
inline double fairValue(const OrderBook& b, const OfiTracker& ofi, const AlphaParams& p) {
  const double alpha = std::clamp(p.wImbalance * b.imbalance(p.depthLevels) + p.wOfi * ofi.signal(), -1.0, 1.0);
  return b.micro() * (1.0 + alpha * p.maxTiltBps * 1e-4);
}

}  // namespace sb
