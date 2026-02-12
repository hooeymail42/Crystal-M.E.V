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
RUST_LOG=solana_mev_bot=debug cargo run  # With debug logging
```

Rust edition 2021, Solana SDK 1.18, borsh 0.10.

## Architecture

A **Solana MEV arbitrage bot** that finds and executes cross-DEX arbitrage opportunities using flash loans (Kamino Finance), Jito bundles, and atomic multi-leg transactions.

### Core Flow

`main.rs`: Load config → Init RPC + wallet → Load pool data per mint → Start WebSocket subscriptions → Enter main loop: refresh pools → detect opportunities → size positions → build/simulate/execute transactions.

Key safety features: circuit breaker (pauses after 10 consecutive failures, exits after 50), drawdown protection (pauses at 20% drawdown), demo mode by default.

### Module Layout

**`src/chain/`** — Blockchain interaction layer:
- `pools.rs` — Central `Pool` enum and `PoolData` trait. `MintPoolData` aggregates pools per token mint.
- `opportunity_detector.rs` — Finds 2-DEX and multi-hop arbitrage using `OpportunityConfig` thresholds
- `trading_graph.rs` — Graph-based cycle detection for arbitrage paths
- `transaction.rs` — `TransactionBuilder`: builds swap instructions, flash loan wrapping, Jito bundles, parallel submission
- `refresh.rs` — `PoolRefreshManager`: fetches and deserializes pool account data
- `pool_subscription.rs` — `WebSocketPoolSubscriber`: real-time pool state via WebSocket
- `capital_manager.rs` — `CapitalManager`: dynamic position sizing, risk limits, auto-compounding
- `gas_fee.rs` — `GasFeeConfig`: gas cost estimation, profitability thresholds
- `volume_weighted_slippage.rs` — ML-based slippage prediction from volume history
- `wallet_integration.rs` — `WalletConfig`: wallet loading, health checks
- `trade_logger.rs` — CSV trade logging (`ExecutedTrade`)
- `constants.rs` — `SOL_MINT` and other constants

**`src/dex/`** — DEX-specific pool parsing and swap logic:
- `raydium/` — V4 AMM, CP-AMM, CLMM (concentrated liquidity)
- `pump/` — Pump.fun AMM
- `meteora/` — DLMM, Dynamic AMM V2
- `whirlpool/` — Orca Whirlpool (concentrated liquidity)
- `phoenix/` — Phoenix order book
- `lifinity/` — Lifinity AMM
- `heaven/` — Heaven protocol
- `solfi/` — SolFi protocol
- `vertigo/` — Vertigo AMM

Each DEX submodule has `constants.rs` (program IDs) and info modules implementing pool deserialization and `get_quote()`.

**`src/config.rs`** — `BotConfig::from_env()` loads all settings.

### Adding a New DEX

1. Create `src/dex/<name>/` with `mod.rs`, `constants.rs`, and `<name>_info.rs`
2. Add variant to `Pool` enum in `chain/pools.rs`
3. Implement `PoolData` trait (especially `get_quote()` and `build_swap_instruction()`)
4. Add `add_<name>_pool()` method to `MintPoolData`
5. Add variant to `Dex` enum in `src/dex/mod.rs`
6. Handle in `TransactionBuilder::build_instructions_from_opportunity()`

### Key Traits

- `PoolData`: `pool_address()`, `get_quote(amount_in, a_to_b) -> (amount_out, price_impact)`, `build_swap_instruction()`, `get_dex_name()`

## Configuration

Environment variables (`.env` file):

**Required:**
- `RPC_URL` — Solana RPC endpoint
- `WALLET_PRIVATE_KEY` / `SOLANA_KEYPAIR` / `SOLANA_KEYPAIR_BASE58` — Wallet key

**Pools (per mint):**
- `MINT_N` + `MINT_N_<DEX>_POOL_LIST` — Token mints and pool addresses

**Optional features:**
- `WS_URL` — WebSocket URL for real-time pool updates
- `ENABLE_REAL_EXECUTION=true` — Enable live trading (default: demo mode)
- `JITO_ENABLED`, `JITO_TIP_LAMPORTS`, `JITO_BLOCK_ENGINE_URL` — Jito bundle submission
- `FLASHLOAN_ENABLED`, `FLASHLOAN_RESERVE`, `FLASHLOAN_RESERVE_VAULT`, `FLASHLOAN_FEE_RECEIVER` — Kamino flash loans
- `SPAM_ENABLED`, `SPAM_RPC_URLS` — Parallel multi-RPC submission

## On-Chain Program

Executor program: `MEViEnscUm6tsQRoGd9h6nLQaQspKj7DB2M5FwM3Xvz`. Uses versioned transactions with Address Lookup Tables (ALTs).

## Serialization

Pool account data uses **Borsh** deserialization. Struct field order must match on-chain layout exactly.
