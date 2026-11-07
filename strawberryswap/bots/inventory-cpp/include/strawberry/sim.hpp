// Deterministic in-memory chain used by `searcher_dryrun` and the self-tests. It re-implements the
// DarkPool acceptance rules, so a plan that passes `preflight` here would also pass them on-chain
// (up to integer rounding, which the engine's rounding directions already cover).
#pragma once
#include <string>
#include "strawberry/chain.hpp"
#include "strawberry/abi.hpp"

namespace sb {

class SimChain final : public IChain {
 public:
  PoolState pool{1000, 2'000'000, 1000, 2'000'000};  // spot 2000
  OracleQuote oracle{2000, 0};
  Inventory inv{50, 100'000};
  int sent = 0;

  PoolState readPool() override { return pool; }
  OracleQuote readOracle() override { return oracle; }
  Inventory readInventory() override { return inv; }

  bool preflight(const std::string& cd) override {
    U128 a[5];
    if (!decode(cd, a)) return false;
    const double o0 = fromWei(a[0]), o1 = fromWei(a[1]), p0 = fromWei(a[2]), p1 = fromWei(a[3]), tr = fromWei(a[4]);
    if (o0 >= pool.r0 || o1 >= pool.r1 || p0 > inv.token0 + o0 || p1 + tr > inv.token1 + o1) return false;
    return chainRules(pool, oracle.price, o0, o1, p0, p1, tr).all();
  }

  std::optional<std::string> send(const std::string& cd) override {
    U128 a[5];
    if (!decode(cd, a)) return std::nullopt;
    const double o0 = fromWei(a[0]), o1 = fromWei(a[1]), p0 = fromWei(a[2]), p1 = fromWei(a[3]), tr = fromWei(a[4]);
    pool.r0 += p0 - o0; pool.r1 += p1 - o1;
    inv.token0 += o0 - p0; inv.token1 += o1 - p1 - tr;
    return "0xsim" + std::to_string(++sent);
  }

 private:
  static bool decode(const std::string& cd, U128 (&out)[5]) {
    if (cd.size() != 2 + 8 + 5 * 64 || cd.rfind("0xc0661737", 0) != 0) return false;
    for (int i = 0; i < 5; ++i) {
      const std::string w = cd.substr(10 + static_cast<std::size_t>(i) * 64 + 32, 32);  // low 128 bits
      U128 v = 0;
      for (char c : w) v = (v << 4) | static_cast<U128>(c <= '9' ? c - '0' : c - 'a' + 10);
      out[i] = v;
    }
    return true;
  }
};

}  // namespace sb
