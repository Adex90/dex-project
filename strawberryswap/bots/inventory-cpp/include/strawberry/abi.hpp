// Minimal ABI encoding for the ONE call the bot makes: SearcherExecutor.rebalance(uint256 ×5).
// Amounts are 18-decimal fixed point held in unsigned __int128 (max ≈ 3.4e20 whole tokens — far above any supply).
#pragma once
#include <cmath>
#include <cstdint>
#include <stdexcept>
#include <array>
#include <cstring>
#include <string_view>

namespace sb {

__extension__ typedef unsigned __int128 U128;  // GCC/Clang extension, silences -Wpedantic
enum class Round { Down, Up };

inline U128 toWei(double tokens, Round r) {
  if (!(tokens >= 0) || tokens > 3.0e20) throw std::out_of_range("amount out of range");
  const long double x = static_cast<long double>(tokens) * 1e18L;
  return static_cast<U128>(r == Round::Down ? std::floor(x) : std::ceil(x));
}

inline double fromWei(U128 w) { return static_cast<double>(static_cast<long double>(w) / 1e18L); }

/// Calldata for SearcherExecutor.rebalance: "0x" + selector + 5 words, as lowercase hex (330 chars). A fixed array:
/// building it allocates nothing (the previous std::string concatenation made five heap allocations per order).
struct Calldata {
  static constexpr std::size_t kLen = 2 + 8 + 5 * 64;
  std::array<char, kLen> bytes{};
  std::string_view view() const { return {bytes.data(), bytes.size()}; }
};

namespace detail {
struct HexLut {
  char t[512];
  constexpr HexLut() : t() {
    const char* h = "0123456789abcdef";
    for (int i = 0; i < 256; ++i) {
      t[2 * i] = h[i >> 4];
      t[2 * i + 1] = h[i & 15];
    }
  }
};
inline constexpr HexLut kHex{};
inline void hex64(char* p, std::uint64_t x) {  // 16 chars, big-endian, one table lookup per byte
  for (int i = 7; i >= 0; --i) {
    std::memcpy(p + 2 * i, kHex.t + 2 * (x & 0xFF), 2);
    x >>= 8;
  }
}
}  // namespace detail

/// selector 0xc0661737 = keccak256("rebalance(uint256,uint256,uint256,uint256,uint256)")[0:4]
/// (taken from the solc methodIdentifiers of contracts/src/periphery/SearcherExecutor.sol; checked in CI)
inline Calldata encodeRebalance(U128 amount0Out, U128 amount1Out, U128 pay0, U128 pay1, U128 treasuryPay) {
  Calldata c;
  char* o = c.bytes.data();
  std::memcpy(o, "0xc0661737", 10);
  const U128 v[5] = {amount0Out, amount1Out, pay0, pay1, treasuryPay};
  for (int w = 0; w < 5; ++w) {
    char* p = o + 10 + 64 * w;
    std::memset(p, '0', 32);  // amounts are < 2^128: the high half of every word is zero
    detail::hex64(p + 32, static_cast<std::uint64_t>(v[w] >> 64));
    detail::hex64(p + 48, static_cast<std::uint64_t>(v[w]));
  }
  return c;
}

}  // namespace sb
