// PHASE 8 AUDIT — micro-benchmarks of the searcher's hot path: CURRENT code vs the patches proposed in
// docs/AUDIT_PHASE8.md §3. Not part of the build. From bots/inventory-cpp/:
//   g++ -std=c++20 -O3 -march=native -DNDEBUG -Iinclude audit/bench.cpp -o /tmp/bench && /tmp/bench
#include <algorithm>
#include <array>
#include <bit>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <random>
#include <vector>
#include "strawberry/abi.hpp"
#include "strawberry/avellaneda_stoikov.hpp"
#include "strawberry/curve.hpp"
#include "strawberry/lob.hpp"

using namespace sb;
using Clock = std::chrono::steady_clock;
template <class T> inline void keep(const T& v) { asm volatile("" : : "g"(&v) : "memory"); }

// =============================================================================================== PATCH 1: flat book
/// Integer-tick ladder over a fixed window with occupancy bitmaps: O(1) best-price maintenance via ctz/clz,
/// contiguous memory (no node allocation, no pointer chasing), deterministic latency. Prices arrive as ticks.
class FlatBook {
 public:
  static constexpr int W = 1 << 14;  // 16 384 ticks; re-centre (memmove + rebase) if the market leaves the window
  explicit FlatBook(std::int64_t centreTick, double tickSize) : base_(centreTick - W / 2), tick_(tickSize) {}
  void set(Side s, std::int64_t t, double qty) {
    const int i = static_cast<int>(t - base_);
    if (static_cast<unsigned>(i) >= static_cast<unsigned>(W)) return;
    auto& q = s == Side::Bid ? bid_ : ask_;
    auto& occ = s == Side::Bid ? ob_ : oa_;
    q[static_cast<std::size_t>(i)] = qty > 0 ? qty : 0;
    const std::uint64_t bit = 1ull << (i & 63);
    if (qty > 0) {
      occ[static_cast<std::size_t>(i >> 6)] |= bit;
      if (s == Side::Bid) { if (i > bb_) bb_ = i; } else { if (i < ba_) ba_ = i; }
    } else {
      occ[static_cast<std::size_t>(i >> 6)] &= ~bit;
      if (s == Side::Bid && i == bb_) bb_ = prevSet(ob_, i);
      if (s == Side::Ask && i == ba_) ba_ = nextSet(oa_, i);
    }
  }
  bool valid() const { return bb_ >= 0 && ba_ < W && bb_ < ba_; }
  double bestBid() const { return static_cast<double>(base_ + bb_) * tick_; }
  double bestAsk() const { return static_cast<double>(base_ + ba_) * tick_; }
  double bidQty() const { return bid_[static_cast<std::size_t>(bb_)]; }
  double askQty() const { return ask_[static_cast<std::size_t>(ba_)]; }
  double micro() const { const double qb = bidQty(), qa = askQty(); return (bestBid() * qa + bestAsk() * qb) / (qa + qb); }
  double imbalance(std::size_t n) const {
    double b = 0, a = 0;
    std::size_t k = 0;
    for (int i = bb_; i >= 0 && k < n; i = prevSet(ob_, i)) { b += bid_[static_cast<std::size_t>(i)]; ++k; }
    k = 0;
    for (int i = ba_; i < W && k < n; i = nextSet(oa_, i)) { a += ask_[static_cast<std::size_t>(i)]; ++k; }
    return (b + a) > 0 ? (b - a) / (b + a) : 0.0;
  }

 private:
  using Bits = std::array<std::uint64_t, W / 64>;
  static int prevSet(const Bits& o, int i) {  // highest set index < i, or -1
    int w = (i - 1) >> 6; if (i <= 0) return -1;
    std::uint64_t m = o[static_cast<std::size_t>(w)] & (~0ull >> (63 - ((i - 1) & 63)));
    while (!m) { if (--w < 0) return -1; m = o[static_cast<std::size_t>(w)]; }
    return (w << 6) + 63 - std::countl_zero(m);
  }
  static int nextSet(const Bits& o, int i) {  // lowest set index > i, or W
    int w = (i + 1) >> 6; if (i + 1 >= W) return W;
    std::uint64_t m = o[static_cast<std::size_t>(w)] & (~0ull << ((i + 1) & 63));
    while (!m) { if (++w >= W / 64) return W; m = o[static_cast<std::size_t>(w)]; }
    return (w << 6) + std::countr_zero(m);
  }
  std::int64_t base_;
  double tick_;
  int bb_ = -1, ba_ = W;
  std::array<double, W> bid_{}, ask_{};
  Bits ob_{}, oa_{};
};

// =============================================================================================== PATCH 2: closed-form planner
/// Exact maximiser of the SAME objective `plan()` searches numerically. With φ = extraction·(1+margin):
///   sell (spot > P): net(x_t) = (1−φ)(y − K/x_t) − (x_t − x)(ask − φP) − gas   ⇒  p* = (ask − φP)/(1 − φ)
///   buy  (spot < P): net(y_t) = (x − K/y_t)(bid − φP) − (y_t − y)(1 − φ) − gas  ⇒  p* = (bid − φP)/(1 − φ)
/// Both are concave in x_t (resp. y_t), so the optimum on the feasible interval is p* clamped to it. The feasible
/// interval is closed-form too: inventory (sell: x_t ≤ x + inv0; buy: a quadratic in √p_t) and real reserves.
inline Plan planClosed(const PoolState& s, double P, double bid, double ask, const PlannerParams& pp) {
  Plan out;
  const double spot = s.spot();
  if (!(P > 0) || !(spot > 0) || std::fabs(spot - P) / P < 1e-9) return out;
  const bool sell = spot > P;
  const double phi = pp.extractionBps / 1e4 * (1.0 + pp.treasuryMarginBps / 1e4), K = s.k(), eps = 1e-9 * P;
  double lo = sell ? P + eps : spot + eps, hi = sell ? spot - eps : P - eps;
  if (sell) {
    const double xmax = s.x() + pp.maxInv0;           // in0 <= inventory
    lo = std::max(lo, K / (xmax * xmax));
    const double ymin = s.y() - s.r1;                 // out1 < r1
    if (ymin > 0) lo = std::max(lo, (ymin * ymin / K) * (1 + 1e-12));
  } else {
    const double xmin = s.x() - s.r0;                 // out0 < r0
    if (xmin > 0) hi = std::min(hi, (K / (xmin * xmin)) * (1 - 1e-12));
    const double sK = std::sqrt(K), a = (1 - phi) * sK, b = -((1 - phi) * s.y() - phi * P * s.x() + pp.maxInv1), c = -phi * P * sK;
    const double u = (-b + std::sqrt(b * b - 4 * a * c)) / (2 * a);  // in1 + pay <= inventory  ⇔  √p_t <= u
    hi = std::min(hi, u * u * (1 - 1e-12));
  }
  if (!(lo < hi)) return out;
  const double pt = std::clamp(((sell ? ask : bid) - phi * P) / (1 - phi), lo, hi);
  const auto e = detail::evalAt(s, pt, sell, P, bid, ask, pp);
  if (!e.feasible || e.net <= pp.minNet) return out;
  out.ok = true; out.sellsToken0 = sell; out.targetPrice = pt;
  out.amount0Out = e.out0; out.amount1Out = e.out1; out.pay0 = e.in0; out.pay1 = e.in1; out.treasuryPay = e.pay;
  out.harvest = e.harvest; out.edge = e.edge; out.net = e.net;
  return out;
}

// =============================================================================================== PATCH 3: allocation-free ABI
struct HexLut { char t[512]; constexpr HexLut() : t() { const char* H = "0123456789abcdef"; for (int i = 0; i < 256; ++i) { t[2 * i] = H[i >> 4]; t[2 * i + 1] = H[i & 15]; } } };
static constexpr HexLut kHex{};
inline void hex64(char* p, std::uint64_t x) {        // 16 chars, big-endian, one table lookup per byte
  for (int i = 7; i >= 0; --i) { std::memcpy(p + 2 * i, kHex.t + 2 * (x & 0xFF), 2); x >>= 8; }
}
inline void encodeRebalanceInto(char* out /* 330 bytes */, const U128 (&v)[5]) {
  std::memcpy(out, "0xc0661737", 10);
  for (int w = 0; w < 5; ++w) {
    char* p = out + 10 + 64 * w;
    std::memset(p, '0', 32);                          // high 128 bits of every word are zero (amounts < 2^128)
    hex64(p + 32, static_cast<std::uint64_t>(v[w] >> 64));
    hex64(p + 48, static_cast<std::uint64_t>(v[w]));
  }
}

// =============================================================================================== harness
struct Msg { Side s; std::int64_t tick; double qty; };

static std::vector<Msg> feed(std::size_t n, std::uint32_t seed) {
  std::mt19937_64 g(seed);
  std::geometric_distribution<int> depth(0.25);
  std::uniform_real_distribution<double> U(0, 1);
  std::map<std::int64_t, double> bids, asks;          // reference state to keep the generated book uncrossed
  std::int64_t mid = 200000;                          // 2000.00 at tick 0.01
  std::vector<Msg> out; out.reserve(n);
  while (out.size() < n) {
    if (U(g) < 0.05) {                                // the market moves one tick: venues delete crossed levels
      mid += U(g) < 0.5 ? -1 : 1;
      while (!bids.empty() && bids.rbegin()->first >= mid) { out.push_back({Side::Bid, bids.rbegin()->first, 0}); bids.erase(std::prev(bids.end())); }
      while (!asks.empty() && asks.begin()->first <= mid) { out.push_back({Side::Ask, asks.begin()->first, 0}); asks.erase(asks.begin()); }
    }
    const bool bid = U(g) < 0.5;
    const std::int64_t t = bid ? mid - 1 - depth(g) : mid + 1 + depth(g);
    const double q = U(g) < 0.3 ? 0.0 : 1 + 100 * U(g);
    auto& m = bid ? bids : asks;
    if (q == 0) m.erase(t); else m[t] = q;
    out.push_back({bid ? Side::Bid : Side::Ask, t, q});
  }
  return out;
}

template <class F> static double nsPer(std::size_t ops, int reps, F&& f) {
  std::vector<double> r;
  for (int k = 0; k < reps; ++k) {
    const auto t0 = Clock::now(); f(); const auto t1 = Clock::now();
    r.push_back(std::chrono::duration<double, std::nano>(t1 - t0).count() / static_cast<double>(ops));
  }
  std::sort(r.begin(), r.end());
  return r[r.size() / 2];
}

int main() {
  // ---------------------------------------------------------------------------- 1. book update + strategy reads
  const auto msgs = feed(2'000'000, 7);
  double sinkA = 0, sinkB = 0;
  const double tMap = nsPer(msgs.size(), 7, [&] {
    OrderBook b;
    for (const auto& m : msgs) {
      b.set(m.s, static_cast<double>(m.tick) * 0.01, m.qty);
      if (b.valid()) sinkA += b.micro() + b.imbalance(5);
    }
    keep(sinkA);
  });
  const double tFlat = nsPer(msgs.size(), 7, [&] {
    FlatBook b(200000, 0.01);
    for (const auto& m : msgs) {
      b.set(m.s, m.tick, m.qty);
      if (b.valid()) sinkB += b.micro() + b.imbalance(5);
    }
    keep(sinkB);
  });
  std::printf("book  update+micro+imbalance(5): std::map %.1f ns/msg   flat ladder %.1f ns/msg   (x%.1f)   checksum %s\n",
              tMap, tFlat, tMap / tFlat, std::fabs(sinkA - sinkB) <= 1e-6 * std::fabs(sinkA) ? "equal" : "DIFFERENT");

  // ---------------------------------------------------------------------------- 2. planner
  std::mt19937_64 g(11);
  std::uniform_real_distribution<double> U(0, 1);
  struct Case { PoolState s; double P, bid, ask; };
  std::vector<Case> cases;
  for (int i = 0; i < 20000; ++i) {
    PoolState s{10 + 990 * U(g), 0, 1000, 2'000'000};
    s.r1 = s.r0 * 2000 * (0.5 + U(g));
    const double P = s.spot() * (1 + (U(g) - 0.5) * 0.04), half = P * 5e-4 * U(g);
    cases.push_back({s, P, P - half, P + half});
  }
  PlannerParams pp; pp.gasCostQuote = 0.4; pp.minNet = 0; pp.maxInv0 = 50; pp.maxInv1 = 100'000;
  int agree = 0, both = 0, okT = 0, okC = 0, closedBetter = 0; double worst = 0, worstNet = 0, gain = 0;
  for (const auto& c : cases) {
    const Plan a = plan(c.s, c.P, c.bid, c.ask, pp), b = planClosed(c.s, c.P, c.bid, c.ask, pp);
    okT += a.ok; okC += b.ok;
    if (a.ok && b.ok) { ++both; worst = std::max(worst, std::fabs(a.targetPrice - b.targetPrice) / a.targetPrice); worstNet = std::max(worstNet, a.net - b.net); gain = std::max(gain, b.net - a.net); closedBetter += b.net - a.net > 1e-6; }
    agree += a.ok == b.ok;
  }
  double s1 = 0, s2 = 0;
  const double tTern = nsPer(cases.size(), 5, [&] { for (const auto& c : cases) s1 += plan(c.s, c.P, c.bid, c.ask, pp).net; keep(s1); });
  const double tClosed = nsPer(cases.size(), 5, [&] { for (const auto& c : cases) s2 += planClosed(c.s, c.P, c.bid, c.ask, pp).net; keep(s2); });
  std::printf("plan  ternary(80 bisect + 200x2 evalAt) %.0f ns   closed form %.0f ns   (x%.0f)\n", tTern, tClosed, tTern / tClosed);
  std::printf("      agreement on go/no-go %d/%zu, both-ok %d, max |Δp*|/p* = %.2e, max (net_ternary - net_closed) = %.2e token1\n",
              agree, cases.size(), both, worst, worstNet);
  std::printf("      closed form strictly better (>1e-6) in %d cases, by up to %.4f token1\n", closedBetter, gain);

  // ---------------------------------------------------------------------------- 3. ABI encoding
  const U128 v[5] = {toWei(1.5, Round::Down), toWei(2999.25, Round::Down), toWei(0.75, Round::Up), 0, toWei(12.5, Round::Up)};
  std::size_t len = 0; char buf[330];
  const double tStr = nsPer(1'000'000, 5, [&] { for (int i = 0; i < 1'000'000; ++i) { auto s = encodeRebalance(v[0], v[1], v[2], v[3], v[4] + static_cast<U128>(i & 1)); len += s.size(); keep(s); } });
  const double tBuf = nsPer(1'000'000, 5, [&] { for (int i = 0; i < 1'000'000; ++i) { U128 w[5] = {v[0], v[1], v[2], v[3], v[4] + static_cast<U128>(i & 1)}; encodeRebalanceInto(buf, w); keep(buf); } });
  U128 w[5] = {v[0], v[1], v[2], v[3], v[4]}; encodeRebalanceInto(buf, w);
  std::printf("abi   std::string concat %.1f ns   fixed buffer %.1f ns   (x%.1f)   identical output: %s\n", tStr, tBuf, tStr / tBuf,
              std::string(buf, 330) == encodeRebalance(v[0], v[1], v[2], v[3], v[4]) ? "yes" : "NO");
  keep(len);

  // ---------------------------------------------------------------------------- 4. variance estimator bias
  // True efficient price: Brownian, σ² = 0.16 /s (main.cpp's synthetic market). Observed mid = efficient + bounce
  // noise η ~ N(0, 0.05²). VarianceEstimator feeds EWMA(dp²/dt) per tick: E = σ² + 2η²/dt  →  biased by feed rate.
  for (double hz : {1.0, 10.0, 100.0}) {
    std::mt19937_64 r(3);
    std::normal_distribution<double> dW(0, 1), eta(0, 0.05);
    VarianceEstimator ve(30);
    double t = 0, px = 2000;
    const double dt = 1.0 / hz;
    for (int i = 0; i < static_cast<int>(3600 * hz); ++i) { t += dt; px += std::sqrt(0.16 * dt) * dW(r); ve.update(t, px + eta(r)); }
    // PATCH: sample on a fixed 1 s grid (previous-tick interpolation) — the noise term becomes 2η²/1 s.
    std::mt19937_64 r2(3);
    VarianceEstimator vg(30);
    double t2 = 0, px2 = 2000, nextGrid = 1, last = 2000;
    for (int i = 0; i < static_cast<int>(3600 * hz); ++i) {
      t2 += dt; px2 += std::sqrt(0.16 * dt) * dW(r2); last = px2 + eta(r2);
      if (t2 >= nextGrid) { vg.update(nextGrid, last); nextGrid += 1; }
    }
    std::printf("var   feed %5.0f Hz: true σ²=0.160/s  tick-EWMA=%.3f (%+.0f%%)   1 s grid=%.3f (%+.0f%%)\n", hz, ve.varPerSec(),
                100 * (ve.varPerSec() / 0.16 - 1), vg.varPerSec(), 100 * (vg.varPerSec() / 0.16 - 1));
  }
  return 0;
}
