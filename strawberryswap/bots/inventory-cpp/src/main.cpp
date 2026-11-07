// searcher_dryrun — runs the full decision pipeline against a synthetic order book and an in-memory chain.
// It performs NO network I/O and signs nothing. Real deployments replace SimChain with a JSON-RPC adapter
// (reads + eth_call) and a remote-signer adapter (MPC/TSS) behind the same IChain port.
#include <cstdio>
#include <random>
#include "strawberry/searcher.hpp"
#include "strawberry/sim.hpp"

int main() {
  using namespace sb;
  SearcherConfig cfg;
  cfg.targetInventory0 = 50;
  cfg.planner.gasCostQuote = 0.4;
  cfg.planner.minNet = 0.5;
  Searcher bot(cfg);
  SimChain chain;

  std::mt19937 rng(7);
  std::normal_distribution<double> step(0, 0.4), noise(0, 0.05);
  double px = 2000.0;
  for (int sec = 1; sec <= 300; ++sec) {
    px += step(rng);  // the off-chain market moves; the pool only moves when somebody trades it
    const double half = 0.15 + std::fabs(noise(rng));
    bot.book().clear();  // synthetic feed publishes full snapshots; a real feed applies deltas
    bot.book().set(Side::Bid, px - half, 30 + 20 * std::fabs(noise(rng)) * 10);
    bot.book().set(Side::Ask, px + half, 30 + 20 * std::fabs(noise(rng)) * 10);
    bot.onBookUpdate(sec);
    bot.onPublicTrade(px + noise(rng));
    chain.oracle = {px + noise(rng) * 0.2, static_cast<std::uint64_t>(sec)};  // zk-DECO price ≈ market

    const Decision d = bot.tick(sec, chain);
    if (d.skip == Skip::None) {
      std::printf("t=%3ds SENT %s  %s token0  harvest=%.4f  treasury=%.4f  net=%.4f  r=%.3f  q=%+.2f\n", sec, d.txHash.c_str(),
                  d.plan.sellsToken0 ? "sell" : "buy ", d.plan.harvest, d.plan.treasuryPay, d.plan.net, d.quotes.reservation, d.q);
    }
  }
  std::printf("done: %d rebalances, inventory token0=%.3f token1=%.1f, pool spot=%.3f\n", chain.sent, chain.inv.token0, chain.inv.token1, chain.pool.spot());
}
