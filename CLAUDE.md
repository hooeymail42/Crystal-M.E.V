# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build                    # Dev build
cargo build --release          # Release build
cargo test                     # Run all tests
cargo test --lib               # Library tests only
cargo test <test_name>         # Single test
cargo run                      # Run the bot
```

Rust edition 2021, Solana SDK 1.16.25, requires Rust 1.93.0+.

## Architecture

This is a **Solana MEV arbitrage bot** that finds and executes cross-DEX arbitrage opportunities using flash loans (Kamino Finance) and atomic multi-leg transactions.

### Core Flow

`main.rs` initializes the RPC client, loads wallet config, starts pool subscriptions, runs slippage prediction, then enters the opportunity detection and execution loop.

### Module Layout

- **`src/chain/`** — Blockchain interaction layer
  - `pools.rs` — Central `Pool` enum (9 variants) and `PoolData` trait that all pool types implement. `MintPoolData` aggregates pools per token mint.
  - `opportunity_detector.rs` — Finds 2-DEX and multi-hop arbitrage opportunities
  - `trading_graph.rs` — Graph-based cycle detection for arbitrage paths
  - `pool_subscription.rs` — Real-time WebSocket pool state monitoring
  - `wallet_integration.rs` — Wallet loading (`WalletConfig::from_env()`), health checks, `TransactionExecutor`
  - `volume_weighted_slippage.rs` — ML-based slippage prediction
  - `trade_logger.rs` — CSV trade logging
  - `transaction.rs`, `refresh.rs`, `token_fetch.rs`, `token_price.rs` — Transaction building and pool data refresh (stubs)

- **`src/dex/`** — DEX-specific pool parsing and swap logic
  - `raydium/` — V4 AMM (`amm_info.rs`), CP-AMM (`cp_amm_info.rs`), CLMM (`clmm_info.rs`)
  - `pump/` — Pump.fun AMM
  - `meteora/` — DLMM (`dlmm_info.rs`), Dynamic AMM V2 (`dammv2_info.rs`)
  - `whirlpool/` — Orca Whirlpool concentrated liquidity
  - `solfi/` — SolFi protocol
  - `vertigo/` — Vertigo AMM

  Each DEX submodule has `constants.rs` (program IDs) and info modules implementing pool deserialization and quote logic.

- **`src/config.rs`** — Configuration loading (currently minimal)

### Adding a New DEX

1. Create `src/dex/<name>/` with `mod.rs`, `constants.rs`, and info module
2. Add variant to `Pool` enum in `chain/pools.rs`
3. Implement `PoolData` trait for the new pool type
4. Add pool list to `MintPoolData`
5. Register in `src/dex/mod.rs`

## Configuration

The bot reads from environment variables (`.env` file) and optionally `config.toml`:

- `RPC_URL` — Solana RPC endpoint
- `WALLET_PRIVATE_KEY` or `SOLANA_KEYPAIR` / `SOLANA_KEYPAIR_BASE58` — Wallet key
- `MINT_N` + `MINT_N_<DEX>_POOL_LIST` — Token mints and their pool addresses per DEX
- `SPAM_ENABLED`, `SPAM_SENDING_RPC_URLS` — Multi-RPC transaction submission
- `FLASHLOAN_ENABLED` — Kamino flash loan toggle
- `BOT_COMPUTE_UNIT_LIMIT` — Compute budget

Wallet defaults to **demo mode** (no real execution). Call `wallet.enable_real_execution()` to go live.

## On-Chain Program

The bot calls an on-chain executor program: `MEViEnscUm6tsQRoGd9h6nLQaQspKj7DB2M5FwM3Xvz`. Transactions use versioned transactions with Address Lookup Tables (ALTs) for efficient account packing.

## Serialization

Pool account data uses **Borsh** deserialization (`borsh = "0.9.3"`). Ensure struct field order matches on-chain layout exactly.
