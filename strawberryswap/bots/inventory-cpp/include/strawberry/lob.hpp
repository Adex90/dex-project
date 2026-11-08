// Limit-order-book state and short-horizon signal extraction (CEX / off-chain reference venue).
#pragma once
#include <algorithm>
#include <array>
#include <bit>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <optional>

namespace sb {

enum class Side { Bid, Ask };

/// Price-level book over INTEGER ticks in a fixed window: two contiguous quantity ladders plus 64-bit occupancy
/// bitmaps, so the best level after a deletion and the next n levels for imbalance() are found with count-zero
/// instructions instead of a red-black-tree walk. No allocation after construction; ~3.6x faster than the previous
/// std::map<double, double> book on a realistic L2 stream (bench/bench.cpp) and immune to the double-key split
/// (2000.1 vs 2000.0999999) that a floating-point key allows.
///
/// The window spans W ticks (at 0.01 that is ±$82 around the touch). When the touch moves out of it the window is
/// re-centred around the new level; a level beyond the window on the FAR side of a side that still has its touch inside
/// the window is ignored (it is worse than that touch by more than W/2 ticks). A jump larger than W/2 should come with
/// a snapshot resync (`clear()`), as venue feeds do.
/// Single-threaded by design: one feed thread owns it and publishes immutable snapshots to the strategy thread.
class OrderBook {
 public:
  static constexpr int W = 1 << 14;

  explicit OrderBook(double tickSize = 0.01) : tick_(tickSize) {}

  /// Hot path: venues publish prices that are exact multiples of the tick; parse them to ticks once at the edge.
  void set(Side s, std::int64_t tick, double qty) {
    if (!init_) {
      base_ = tick - W / 2;
      init_ = true;
    }
    std::int64_t i = tick - base_;
    if (i < 0 || i >= W) {
      if (qty <= 0.0) return;  // deleting a level we never held
      const bool deep = (s == Side::Bid && i < 0 && bb_ >= 0) || (s == Side::Ask && i >= W && ba_ < W);
      if (deep) return;  // worse than this side's touch by more than the window: irrelevant to top-of-book signals
      recentre(tick);    // the touch moved out of the window: the market moved
      i = tick - base_;
    }
    write(s, static_cast<int>(i), qty);
  }
  /// Convenience for feeds that publish decimals (and for tests).
  void setPrice(Side s, double price, double qty) { set(s, toTick(price), qty); }
  std::int64_t toTick(double price) const { return std::llround(price / tick_); }

  /// Snapshot resync: O(levels present), not O(W).
  void clear() {
    clearSide(bidQ_, bidOcc_);
    clearSide(askQ_, askOcc_);
    bb_ = -1;
    ba_ = W;
    init_ = false;
  }

  bool valid() const { return bb_ >= 0 && ba_ < W && bb_ < ba_; }
  double bestBid() const { return price(bb_); }
  double bestAsk() const { return price(ba_); }
  double bidQty() const { return bidQ_[static_cast<std::size_t>(bb_)]; }
  double askQty() const { return askQ_[static_cast<std::size_t>(ba_)]; }
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
    std::size_t k = 0;
    for (int i = bb_; i >= 0 && k < n; i = prevSet(bidOcc_, i), ++k) b += bidQ_[static_cast<std::size_t>(i)];
    k = 0;
    for (int i = ba_; i < W && k < n; i = nextSet(askOcc_, i), ++k) a += askQ_[static_cast<std::size_t>(i)];
    return (b + a) > 0 ? (b - a) / (b + a) : 0.0;
  }

 private:
  using Ladder = std::array<double, W>;
  using Bits = std::array<std::uint64_t, W / 64>;

  double price(int i) const { return static_cast<double>(base_ + i) * tick_; }

  void write(Side s, int i, double qty) {
    const bool bid = s == Side::Bid;
    Ladder& q = bid ? bidQ_ : askQ_;
    Bits& occ = bid ? bidOcc_ : askOcc_;
    const auto w = static_cast<std::size_t>(i >> 6);
    const std::uint64_t bit = std::uint64_t{1} << (i & 63);
    if (qty > 0.0) {
      q[static_cast<std::size_t>(i)] = qty;
      occ[w] |= bit;
      if (bid && i > bb_) bb_ = i;
      if (!bid && i < ba_) ba_ = i;
    } else {
      q[static_cast<std::size_t>(i)] = 0.0;
      occ[w] &= ~bit;
      if (bid && i == bb_) bb_ = prevSet(occ, i);
      if (!bid && i == ba_) ba_ = nextSet(occ, i);
    }
  }

  /// Move the window so `tick` sits in its middle, keeping every level that is still inside it. In place (no 256 KB
  /// stack copies); O(W), and only ever needed when the touch jumps by more than W/2 ticks.
  void recentre(std::int64_t tick) {
    const std::int64_t nb = tick - W / 2, shift = nb - base_;  // old index i becomes i - shift
    shiftLadder(bidQ_, shift);
    shiftLadder(askQ_, shift);
    base_ = nb;
    rebuild(bidQ_, bidOcc_);
    rebuild(askQ_, askOcc_);
    bb_ = prevSet(bidOcc_, W);
    ba_ = nextSet(askOcc_, -1);
  }

  static void shiftLadder(Ladder& q, std::int64_t shift) {
    if (shift >= W || shift <= -W) {
      q.fill(0.0);
      return;
    }
    const auto k = static_cast<std::size_t>(shift < 0 ? -shift : shift), n = static_cast<std::size_t>(W) - k;
    if (shift > 0) {
      std::memmove(q.data(), q.data() + k, n * sizeof(double));
      std::fill(q.begin() + static_cast<std::ptrdiff_t>(n), q.end(), 0.0);
    } else if (shift < 0) {
      std::memmove(q.data() + k, q.data(), n * sizeof(double));
      std::fill(q.begin(), q.begin() + static_cast<std::ptrdiff_t>(k), 0.0);
    }
  }

  static void rebuild(const Ladder& q, Bits& occ) {
    occ.fill(0);
    for (std::size_t i = 0; i < q.size(); ++i)
      if (q[i] > 0.0) occ[i >> 6] |= std::uint64_t{1} << (i & 63);
  }

  static void clearSide(Ladder& q, Bits& occ) {
    for (std::size_t w = 0; w < occ.size(); ++w) {
      for (std::uint64_t m = occ[w]; m != 0; m &= m - 1) q[(w << 6) + static_cast<std::size_t>(std::countr_zero(m))] = 0.0;
      occ[w] = 0;
    }
  }

  static int prevSet(const Bits& o, int i) {  // highest occupied index < i, or -1
    if (i <= 0) return -1;
    int w = (i - 1) >> 6;
    std::uint64_t m = o[static_cast<std::size_t>(w)] & (~std::uint64_t{0} >> (63 - ((i - 1) & 63)));
    while (m == 0) {
      if (--w < 0) return -1;
      m = o[static_cast<std::size_t>(w)];
    }
    return (w << 6) + 63 - std::countl_zero(m);
  }

  static int nextSet(const Bits& o, int i) {  // lowest occupied index > i, or W
    if (i + 1 >= W) return W;
    int w = (i + 1) >> 6;
    std::uint64_t m = o[static_cast<std::size_t>(w)] & (~std::uint64_t{0} << ((i + 1) & 63));
    while (m == 0) {
      if (++w >= W / 64) return W;
      m = o[static_cast<std::size_t>(w)];
    }
    return (w << 6) + std::countr_zero(m);
  }

  double tick_;
  std::int64_t base_ = 0;
  bool init_ = false;
  int bb_ = -1, ba_ = W;
  Ladder bidQ_{}, askQ_{};
  Bits bidOcc_{}, askOcc_{};
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
