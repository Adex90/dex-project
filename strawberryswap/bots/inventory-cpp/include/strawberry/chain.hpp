// Ports (interfaces) between the strategy and the outside world. The strategy core never does I/O, so it
// can be unit-tested and replayed deterministically. Real adapters (JSON-RPC, remote signer) live elsewhere.
#pragma once
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include "strawberry/curve.hpp"

namespace sb {

struct OracleQuote { double price; std::uint64_t updatedAtSec; };  // zk-DECO-verified price, token1 per token0
struct Inventory { double token0, token1; };                        // held by SearcherExecutor

class IChain {
 public:
  virtual ~IChain() = default;
  virtual PoolState readPool() = 0;
  virtual OracleQuote readOracle() = 0;
  virtual Inventory readInventory() = 0;
  /// eth_call of the exact calldata against the pending block. MUST be true before any send.
  virtual bool preflight(std::string_view calldataHex) = 0;
  /// Hands calldata to the remote signer / relay (private mempool). Returns tx hash. The bot NEVER holds a key.
  virtual std::optional<std::string> send(std::string_view calldataHex) = 0;
};

}  // namespace sb
