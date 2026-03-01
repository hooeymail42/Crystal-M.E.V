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

## Known Gotchas & Bug History

### MeteoraDAmmV2 Vault Accounts (FIXED 2026-03-01)
`a_vault`/`b_vault` are Meteora Vault Program (`24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi`)
accounts shared across multiple DAMM pools. Pool reserves are computed via LP-token ratio:
  `pool_reserve = vault_totalAmount * pool_lp_balance / lp_mint_supply`
Implemented in `refresh.rs` `refresh_vault_balances` MeteoraDAmmV2 block (3-step RPC fetch):
  1. Vault accounts → `totalAmount` at [11..19] + `tokenVault` at [19..51] + `lpMint` at [115..147]
  2. LP mint accounts → `supply` at [36..44] (SPL Mint layout)
  3. `a_vault_lp`/`b_vault_lp` SPL accounts → pool LP balance at [64..72]
`DeserializedPoolState::MeteoraDAmmV2` carries `a_vault_lp`, `b_vault_lp`, `admin_token_a_fee`, `admin_token_b_fee`.
`PoolRefreshManager.damm_v2_swap_accounts` caches `DammV2SwapAccounts` per pool (token vaults + LP mints).
Swap instruction (`transaction.rs:build_damm_v2_swap_from_step`) has correct 15-account list.
Pool layout: d[234..266]=admin_token_a_fee (Pubkey), d[266..298]=admin_token_b_fee (Pubkey).
Admin fee direction: swapForY(A→B) → adminTokenBFee; swapForY=false(B→A) → adminTokenAFee.
8 DAMM V2 pools active: USDC, USDT, JUP, BONK, WIF, POPCAT, MEW, GRASS (RAY removed — $3.5k TVL).

### Kamino Flash Loan `Custom(3007)` = BorrowingDisabled
Wrong reserve address causes this. Current SOL reserve: `d4A2prbA2whesmvHaL88BH6Ewn5N4bTSU2Ze8P6Bc4Q`.
Parse reserve bytes to verify: `[128..160]` = mint, `[160..192]` = supply_vault (FLASHLOAN_RESERVE_VAULT),
`[192..224]` = fee_vault (FLASHLOAN_FEE_RECEIVER). These two must be different addresses.

### CLMM Input Sizing (UNFIXED)
CLMM pools (RaydiumClmm, Whirlpool) produce absurd `in=879 SOL` input amounts. The optimal
input binary search does not account for concentrated liquidity tick range boundaries. These
pools are scanned but their opportunities should be treated with extra skepticism until fixed.

### Raydium AMM V4 Coin/PC Vault Ordering + User ATA Direction (FIXED 2026-03-01)
`pool_coin_token_account` is NOT always the token vault — the coin can be SOL or the token
depending on pool creation order. Must check `coin_mint_address` from deserialized pool state:
if `coin_mint == SOL_MINT`, then `(token_vault=pc_vault, sol_vault=coin_vault)`, otherwise
`(token_vault=coin_vault, sol_vault=pc_vault)`. Fixed in `refresh.rs`
`DeserializedPoolState::RaydiumAmm` match arm.

User ATA direction (`build_raydium_swap_from_step`): Raydium V4 infers swap direction from
`userSourceToken.mint` vs `coin_vault.mint`. Old code used `get_user_ata(&coin_mint)` for
buy destination — wrong if `coin_mint == SOL_MINT` (destination = WSOL = same as source).
Fix: `token_ata = if coin_is_sol { get_user_ata(&pc_mint) } else { get_user_ata(&coin_mint) }`.

### Raydium V4 Size Guard (FIXED 2026-03-01)
Raydium V4 AMM accounts are **exactly 1664 bytes**. The old minimum was 752 (wrong). Pools
from other programs (e.g. `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`) can be 752 bytes,
pass the old check, and get deserialized as garbage RaydiumAmm state — producing phantom arbs
and `InvalidSplTokenProgram` (Custom(38)) during simulation. Fix: raise minimum to 1664 in
`dex/raydium/amm_info.rs:try_deserialize`. Also remove any .env pool addresses not owned by
the real Raydium V4 program (`675kPX9MHTjS2zt1qfr1NYHuzeLXFQM5p84CmjZrtsm`).

### DLMM Quote Model (FIXED 2026-02-28)
`opportunity_detector.rs` `calculate_buy_quote`/`calculate_sell_quote` for `MeteoraDlmm` now
use the **direct active-bin price formula**: `price = (1 + bin_step/10000)^active_id`.
Guards: (1) reserve-ratio sanity check — reject if price deviates >10× from `token_reserve/sol_reserve`
ratio (catches v2-format pools with garbage `active_id` reads); (2) depth cap —
`output.min(reserve / 200)` prevents over-claiming active bin liquidity.
The old `build_dlmm_calc` / uniform-20-bin model (caused simulation failures due to overstated
per-bin liquidity) and `Bin`/`DlmmSwapCalculator` imports have been removed.

### DLMM Program Versions
Standard DLMM: `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` — mints at d[80]/d[112],
vaults at d[144]/d[176] after 8-byte discriminator. v2 program:
`Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB` — different layout. Only standard program
pools work with current deserialization. v2-pool phantom arbs are caught by the >10× deviation
check (their garbage active_id produces price incompatible with vault balances).

### Address Lookup Table (ALT)
The active ALT is `3Xj2vwD535dWUFUQCzWup3SuNhCyCYsbSiXhmpLUbSGw` (44 addresses, created
2026-02-19). Without a valid ALT, v0 transactions with flashloan+2 swaps exceed the 1232-byte
raw limit. Add new pool/vault accounts with:
`solana address-lookup-table extend <ALT_ADDR> --addresses "addr1,addr2,..."`
Keypair at `/tmp/bot-keypair.json` (derived from `SOLANA_KEYPAIR_BASE58` in .env).
