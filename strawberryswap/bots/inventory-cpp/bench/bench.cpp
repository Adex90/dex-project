// Hot-path benchmarks: the shipped implementations (include/strawberry) against the ones they replaced in Phase 8
// (kept verbatim below in namespace `legacy`). Not part of the default build:
//   cmake -S . -B build -DSTRAWBERRY_BENCH=ON && cmake --build build --target bench && ./build/bench
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <functional>
#include <limits>
#include <map>
#include <memory>
#include <random>
#include <string>
#include <vector>
#include "strawberry/abi.hpp"
#include "strawberry/avellaneda_stoikov.hpp"
#include "strawberry/curve.hpp"
#include "strawberry/lob.hpp"

using namespace sb;
using Clock = std::chrono::steady_clock;
template <class T> inline void keep(const T& v) { asm volatile("" : : "g"(&v) : "memory"); }

namespace legacy {

/// The previous book: std::map keyed by double price.
class MapBook {
 public:
  void set(Side s, double price, double qty) {
    auto upd = [&](auto& m) {
      if (qty <= 0.0) m.erase(price); else m[price] = qty;
    };
    s == Side::Bid ? upd(bids_) : upd(asks_);
  }
  bool valid() const { return !bids_.empty() && !asks_.empty() && bids_.begin()->first < asks_.begin()->first; }
  double micro() const {
    const double pb = bids_.begin()->first, pa = asks_.begin()->first, qb = bids_.begin()->second, qa = asks_.begin()->second;
    return (pb * qa + pa * qb) / (qa + qb);
  }
  double imbalance(std::size_t n) const {
    double b = 0, a = 0;
    std::size_t i = 0;
    for (auto it = bids_.begin(); it != bids_.end() && i < n; ++it, ++i) b += it->second;
    i = 0;
    for (auto it = asks_.begin(); it != asks_.end() && i < n; ++it, ++i) a += it->second;
    return (b + a) > 0 ? (b - a) / (b + a) : 0.0;
  }

 private:
  std::map<double, double, std::greater<>> bids_;
  std::map<double, double> asks_;
};

/// The previous planner: 80-step one-sided halving to a feasible end, then a 200-step ternary search.
inline Plan plan(const PoolState& s, double oracleP, double bid, double ask, const PlannerParams& pp) {
  Plan out;
  const double spot = s.spot();
  if (!(oracleP > 0) || !(spot > 0) || std::fabs(spot - oracleP) / oracleP < 1e-9) return out;
  const bool sell = spot > oracleP;
  double lo = sell ? oracleP : spot, hi = sell ? spot : oracleP;
  const double eps = 1e-9 * oracleP;
  lo += eps;
  hi -= eps;
  if (!(lo < hi)) return out;
  for (int i = 0; i < 80; ++i) {
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
  out.ok = true; out.sellsToken0 = sell; out.targetPrice = pt; out.net = e.net;
  return out;
}

/// The previous ABI encoder: five std::string words concatenated.
inline std::string word(U128 v) {
  static const char* H = "0123456789abcdef";
  std::string s(64, '0');
  for (int i = 63; i >= 0 && v != 0; --i) { s[static_cast<std::size_t>(i)] = H[static_cast<unsigned>(v & 0xF)]; v >>= 4; }
  return s;
}
inline std::string encodeRebalance(U128 a, U128 b, U128 c, U128 d, U128 e) {
  return "0xc0661737" + word(a) + word(b) + word(c) + word(d) + word(e);
}

}  // namespace legacy

struct Msg { Side s; std::int64_t tick; double qty; };

/// An uncrossed L2 stream: levels near a random-walking mid (geometric depth), 30 % deletions, crossed levels removed.
static std::vector<Msg> feed(std::size_t n, std::uint64_t seed) {
  std::mt19937_64 g(seed);
  std::geometric_distribution<int> depth(0.25);
  std::uniform_real_distribution<double> U(0, 1);
  std::map<std::int64_t, double> bids, asks;
  std::int64_t mid = 200000;  // 2000.00 at tick 0.01
  std::vector<Msg> out;
  out.reserve(n);
  while (out.size() < n) {
    if (U(g) < 0.05) {
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
    const auto t0 = Clock::now();
    f();
    const auto t1 = Clock::now();
    r.push_back(std::chrono::duration<double, std::nano>(t1 - t0).count() / static_cast<double>(ops));
  }
  std::sort(r.begin(), r.end());
  return r[r.size() / 2];
}

int main() {
  // ---------------------------------------------------------------- 1. book update + the strategy's per-update reads
  const auto msgs = feed(2'000'000, 7);
  double sinkA = 0, sinkB = 0;
  const double tMap = nsPer(msgs.size(), 7, [&] {
    legacy::MapBook b;
    for (const auto& m : msgs) {
      b.set(m.s, static_cast<double>(m.tick) * 0.01, m.qty);
      if (b.valid()) sinkA += b.micro() + b.imbalance(5);
    }
    keep(sinkA);
  });
  const double tFlat = nsPer(msgs.size(), 7, [&] {
    auto b = std::make_unique<OrderBook>(0.01);
    for (const auto& m : msgs) {
      b->set(m.s, m.tick, m.qty);
      if (b->valid()) sinkB += b->micro() + b->imbalance(5);
    }
    keep(sinkB);
  });
  std::printf("book  update+micro+imbalance(5): std::map %.1f ns/msg   flat ladder %.1f ns/msg   (x%.1f)   checksum %s\n", tMap, tFlat,
              tMap / tFlat, std::fabs(sinkA - sinkB) <= 1e-6 * std::fabs(sinkA) ? "equal" : "DIFFERENT");

  // ---------------------------------------------------------------- 2. planner
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
  PlannerParams pp;
  pp.gasCostQuote = 0.4;
  pp.minNet = 0;
  pp.maxInv0 = 50;
  pp.maxInv1 = 100'000;
  int agree = 0, better = 0;
  double gain = 0, worse = 0;
  for (const auto& c : cases) {
    const Plan a = legacy::plan(c.s, c.P, c.bid, c.ask, pp), b = plan(c.s, c.P, c.bid, c.ask, pp);
    agree += a.ok == b.ok;
    if (a.ok && b.ok) {
      gain = std::max(gain, b.net - a.net);
      worse = std::max(worse, a.net - b.net);
      better += b.net - a.net > 1e-6;
    }
  }
  double s1 = 0, s2 = 0;
  const double tTern = nsPer(cases.size(), 5, [&] { for (const auto& c : cases) s1 += legacy::plan(c.s, c.P, c.bid, c.ask, pp).net; keep(s1); });
  const double tClosed = nsPer(cases.size(), 5, [&] { for (const auto& c : cases) s2 += plan(c.s, c.P, c.bid, c.ask, pp).net; keep(s2); });
  std::printf("plan  bisect+ternary %.0f ns   closed form %.0f ns   (x%.0f)\n", tTern, tClosed, tTern / tClosed);
  std::printf("      go/no-go agreement %d/%zu; closed form better in %d cases (by up to %.4f token1), never worse than %.1e\n", agree,
              cases.size(), better, gain, worse);

  // ---------------------------------------------------------------- 3. calldata encoding
  const U128 v[5] = {toWei(1.5, Round::Down), toWei(2999.25, Round::Down), toWei(0.75, Round::Up), 0, toWei(12.5, Round::Up)};
  std::size_t len = 0;
  const double tStr = nsPer(1'000'000, 5, [&] {
    for (int i = 0; i < 1'000'000; ++i) {
      const auto s = legacy::encodeRebalance(v[0], v[1], v[2], v[3], v[4] + static_cast<U128>(i & 1));
      len += s.size();
      keep(s);
    }
  });
  const double tBuf = nsPer(1'000'000, 5, [&] {
    for (int i = 0; i < 1'000'000; ++i) {
      const Calldata c = encodeRebalance(v[0], v[1], v[2], v[3], v[4] + static_cast<U128>(i & 1));
      keep(c);
    }
  });
  const bool same = encodeRebalance(v[0], v[1], v[2], v[3], v[4]).view() == legacy::encodeRebalance(v[0], v[1], v[2], v[3], v[4]);
  std::printf("abi   std::string concat %.1f ns   fixed buffer %.1f ns   (x%.1f)   identical output: %s\n", tStr, tBuf, tStr / tBuf, same ? "yes" : "NO");
  keep(len);

  // ---------------------------------------------------------------- 4. variance estimator (open item Q-6, not fixed yet)
  // EWMA(dp²/dt) per tick measures σ² + 2η²/dt: microstructure noise inflates it with the feed rate.
  for (double hz : {1.0, 10.0, 100.0}) {
    std::mt19937_64 r(3);
    std::normal_distribution<double> dW(0, 1), eta(0, 0.05);
    VarianceEstimator ve(30);
    double t = 0, px = 2000;
    const double dt = 1.0 / hz;
    for (int i = 0; i < static_cast<int>(3600 * hz); ++i) {
      t += dt;
      px += std::sqrt(0.16 * dt) * dW(r);
      ve.update(t, px + eta(r));
    }
    std::printf("var   feed %5.0f Hz: true σ²=0.160/s  tick-EWMA=%.3f (%+.0f%%)\n", hz, ve.varPerSec(), 100 * (ve.varPerSec() / 0.16 - 1));
  }
  return 0;
}
