// Minimal ABI encoding for the ONE call the bot makes: SearcherExecutor.rebalance(uint256 ×5).
// Amounts are 18-decimal fixed point held in unsigned __int128 (max ≈ 3.4e20 whole tokens — far above any supply).
#pragma once
#include <cmath>
#include <cstdint>
#include <stdexcept>
#include <string>

namespace sb {

__extension__ typedef unsigned __int128 U128;  // GCC/Clang extension, silences -Wpedantic
enum class Round { Down, Up };

inline U128 toWei(double tokens, Round r) {
  if (!(tokens >= 0) || tokens > 3.0e20) throw std::out_of_range("amount out of range");
  const long double x = static_cast<long double>(tokens) * 1e18L;
  return static_cast<U128>(r == Round::Down ? std::floor(x) : std::ceil(x));
}

inline double fromWei(U128 w) { return static_cast<double>(static_cast<long double>(w) / 1e18L); }

inline std::string word(U128 v) {  // 32-byte big-endian word, lowercase hex, no prefix
  static const char* H = "0123456789abcdef";
  std::string s(64, '0');
  for (int i = 63; i >= 0 && v != 0; --i) { s[static_cast<std::size_t>(i)] = H[static_cast<unsigned>(v & 0xF)]; v >>= 4; }
  return s;
}

/// selector 0xc0661737 = keccak256("rebalance(uint256,uint256,uint256,uint256,uint256)")[0:4]
/// (taken from the solc methodIdentifiers of contracts/src/periphery/SearcherExecutor.sol; checked in CI)
inline std::string encodeRebalance(U128 amount0Out, U128 amount1Out, U128 pay0, U128 pay1, U128 treasuryPay) {
  return "0xc0661737" + word(amount0Out) + word(amount1Out) + word(pay0) + word(pay1) + word(treasuryPay);
}

}  // namespace sb
