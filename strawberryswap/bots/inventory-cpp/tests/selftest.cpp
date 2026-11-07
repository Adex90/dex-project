#include <cassert>
#include <cstdio>
#include <random>
#include "strawberry/abi.hpp"
#include "strawberry/avellaneda_stoikov.hpp"
#include "strawberry/curve.hpp"
#include "strawberry/lob.hpp"
#include "strawberry/searcher.hpp"
#include "strawberry/sim.hpp"

using namespace sb;
static int checks = 0;
#define CHECK(c) do { ++checks; if (!(c)) { std::fprintf(stderr, "FAIL %s:%d  %s\n", __FILE__, __LINE__, #c); std::abort(); } } while (0)

static void testAvellanedaStoikov() {
  ASParams p; p.gamma = 0.1; p.horizonSec = 60;
  const double s = 2000, sig2 = 0.05, kappa = 1.5;
  const auto flat = quotes(p, s, 0, sig2, kappa), longQ = quotes(p, s, +5, sig2, kappa), shortQ = quotes(p, s, -5, sig2, kappa);
  CHECK(flat.reservation == s);                    // no inventory ⇒ no skew
  CHECK(longQ.reservation < s && shortQ.reservation > s);  // long ⇒ want to sell ⇒ lower r
  CHECK(flat.halfSpread > 0 && flat.halfSpread == longQ.halfSpread);  // spread independent of q
  CHECK(quotes(p, s, 0, 0.5, kappa).halfSpread > flat.halfSpread);    // more vol ⇒ wider
  CHECK(quotes(p, s, 0, sig2, 10).halfSpread < flat.halfSpread);      // deeper book (bigger κ) ⇒ tighter
  // closed form check of spread
  const double want = 0.5 * (0.1 * sig2 * 60 + (2 / 0.1) * std::log(1 + 0.1 / kappa));
  CHECK(std::fabs(flat.halfSpread - want) < 1e-12);

  KappaEstimator k(0.5, 1.0);
  for (int i = 0; i < 50; ++i) k.onTrade(0.25);
  CHECK(std::fabs(k.kappa() - 4.0) < 1e-3);  // mean depth 0.25 ⇒ κ = 4

  VarianceEstimator v(10);
  double t = 0, px = 100; std::mt19937 g(1); std::normal_distribution<double> n(0, 0.3);
  for (int i = 0; i < 4000; ++i) { t += 1; px += n(g); v.update(t, px); }
  CHECK(v.varPerSec() > 0.05 && v.varPerSec() < 0.15);  // true 0.09 per second
}

static void testLob() {
  OrderBook b;
  b.set(Side::Bid, 99, 10); b.set(Side::Bid, 98, 5); b.set(Side::Ask, 101, 10); b.set(Side::Ask, 102, 5);
  CHECK(b.valid() && b.mid() == 100 && b.micro() == 100 && b.imbalance(2) == 0);
  b.set(Side::Bid, 99, 30);
  CHECK(b.micro() > 100 && b.imbalance(2) > 0);  // heavy bid ⇒ microprice leans up
  b.set(Side::Bid, 99, 0);
  CHECK(b.bestBid() == 98);                       // level deletion

  OfiTracker o(5);
  o.onTopOfBook(0, 99, 10, 101, 10);
  o.onTopOfBook(1, 99, 25, 101, 10);               // bid size grows ⇒ buy pressure
  const double up = o.signal();
  CHECK(up > 0);
  o.onTopOfBook(2, 99, 25, 101, 40);               // ask size grows ⇒ sell pressure
  CHECK(o.signal() < up);
  OfiTracker d(1); d.onTopOfBook(0, 99, 10, 101, 10); d.onTopOfBook(0.1, 99, 30, 101, 10);
  const double s0 = d.signal(); d.onTopOfBook(20, 99, 30, 101, 10);
  CHECK(d.signal() < s0 * 0.01);                   // decays away
}

static void testAbi() {
  const std::string cd = encodeRebalance(toWei(1.0, Round::Down), 0, 0, 0, toWei(2.0, Round::Down));
  CHECK(cd.size() == 2 + 8 + 5 * 64);
  CHECK(cd.substr(0, 10) == "0xc0661737");
  CHECK(cd.substr(10, 64) == "0000000000000000000000000000000000000000000000000de0b6b3a7640000");
  CHECK(cd.substr(10 + 4 * 64, 64) == "0000000000000000000000000000000000000000000000001bc16d674ec80000");
  CHECK(toWei(0.1, Round::Up) >= toWei(0.1, Round::Down));
}

// Property test: every plan the planner emits must satisfy the contract's three acceptance rules.
static void testPlannerAgainstContractRules() {
  std::mt19937_64 g(42);
  std::uniform_real_distribution<double> u(0, 1);
  int plans = 0;
  for (int i = 0; i < 40000; ++i) {
    PoolState s{50 + u(g) * 5000, 1e5 + u(g) * 1e7, 10 + u(g) * 1000, 1e4 + u(g) * 1e6};
    const double P = s.spot() * (1 + (u(g) - 0.5) * 0.04);  // oracle within ±2 % of spot
    const double half = u(g) * 0.002 * P, skew = (u(g) - 0.5) * 0.002 * P;
    PlannerParams pp; pp.gasCostQuote = u(g) * 2; pp.maxInv0 = u(g) * 200; pp.maxInv1 = u(g) * 4e5; pp.minNet = 0;
    const Plan p = plan(s, P, P + skew - half, P + skew + half, pp);
    if (!p.ok) continue;
    ++plans;
    CHECK(p.net > 0);
    CHECK(p.pay0 <= pp.maxInv0 * (1 + 1e-12) && p.pay1 + p.treasuryPay <= pp.maxInv1 * (1 + 1e-12) + p.amount1Out);
    const auto r = chainRules(s, P, p.amount0Out, p.amount1Out, p.pay0, p.pay1, p.treasuryPay);
    CHECK(r.kOk); CHECK(r.deviationOk); CHECK(r.treasuryOk);
    CHECK(p.sellsToken0 == (s.spot() > P));
    // never push past the oracle
    CHECK(p.sellsToken0 ? p.targetPrice >= P * (1 - 1e-9) : p.targetPrice <= P * (1 + 1e-9));
  }
  CHECK(plans > 1000);  // the generator must actually exercise the planner

  // optimality: ternary-search result matches a brute-force grid
  PoolState s{1000, 2e6, 1000, 2e6};  // spot 2000
  const double P = 1980; PlannerParams pp; pp.maxInv0 = 1e9; pp.maxInv1 = 1e12; pp.gasCostQuote = 1;
  const double bid = P - 0.5, ask = P + 0.5;
  const Plan p = plan(s, P, bid, ask, pp);
  CHECK(p.ok && p.sellsToken0);
  double best = -1e300;
  for (int i = 1; i < 20000; ++i) {
    const double pt = P + (s.spot() - P) * i / 20000.0;
    const double xt = std::sqrt(s.k() / pt), yt = std::sqrt(s.k() * pt);
    const double in0 = xt - s.x(), out1 = s.y() - yt, h = out1 - in0 * P;
    best = std::max(best, out1 - in0 * ask - h * 0.8 * (1 + 5e-4) - 1);
  }
  CHECK(p.net >= best - 1e-6 * std::fabs(best) - 1e-6);

  // inventory skew: a long book is happier to sell into the pool than a short one
  PoolState s2{1000, 2e6, 1000, 2e6};
  ASParams as; const auto qLong = quotes(as, 1985, +20, 0.05, 1.5), qShort = quotes(as, 1985, -20, 0.05, 1.5);
  PlannerParams pq; pq.maxInv0 = 1e9; pq.maxInv1 = 1e12;
  const Plan pl = plan(s2, 1985, qLong.bid(), qLong.ask(), pq), ps = plan(s2, 1985, qShort.bid(), qShort.ask(), pq);
  CHECK(pl.ok && pl.sellsToken0);
  CHECK(!ps.ok || pl.amount1Out >= ps.amount1Out);
}

static void testEndToEnd() {
  SearcherConfig c; c.planner.gasCostQuote = 0.5; c.targetInventory0 = 50; c.as.gamma = 0.1;
  Searcher bot(c);
  SimChain chain;
  chain.oracle = {1985, 100};                    // pool 2000 vs zk-oracle 1985: pool too expensive
  bot.book().set(Side::Bid, 1984.9, 40); bot.book().set(Side::Ask, 1985.1, 40);
  double t = 0;
  for (int i = 0; i < 20; ++i) { t += 1; bot.onBookUpdate(t); bot.onPublicTrade(1985.0 + 0.2 * (i % 3)); }
  chain.oracle.updatedAtSec = 105;
  Decision d = bot.tick(110, chain);
  CHECK(d.skip == Skip::None && !d.txHash.empty() && d.plan.sellsToken0);
  CHECK(chain.pool.spot() < 2000 && chain.pool.spot() >= 1985 * (1 - 1e-6));   // moved TOWARD oracle, not past
  CHECK(chain.inv.token1 > 100'000 - 1);                                       // inventory moved as planned

  // safety gates
  SimChain stale; stale.oracle = {1985, 0};
  CHECK(bot.tick(110, stale).skip == Skip::StaleOracle);
  SimChain bad; bad.oracle = {1900, 105};         // oracle 4 % away from the LOB ⇒ refuse to trade
  CHECK(bot.tick(110, bad).skip == Skip::OracleDivergence);
  Searcher empty(c); SimChain any;
  CHECK(empty.tick(1, any).skip == Skip::NoBook);
  SimChain flat; flat.pool = {1000, 1985e3, 1000, 1985e3}; flat.oracle = {1985, 105};
  CHECK(bot.tick(110, flat).skip == Skip::NoEdge);                              // nothing to do ⇒ do nothing
}

int main() {
  testAvellanedaStoikov(); testLob(); testAbi(); testPlannerAgainstContractRules(); testEndToEnd();
  std::printf("selftest OK (%d checks)\n", checks);
}
