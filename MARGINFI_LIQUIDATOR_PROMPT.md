# Build Prompt: Flash-Loan MarginFi Liquidator Bot

Paste the block below to an agent working in this repository. It is written to
build on the existing Kamino flash-loan and Jito infrastructure already present
in `src/chain/transaction.rs` and `src/jito/`, and to follow the module and
config conventions documented in `CLAUDE.md`.

> ⚠️ This is a liquidation bot that executes real on-chain transactions and moves
> real funds. Keep `ENABLE_REAL_EXECUTION` off until every step below is verified
> against mainnet account data in simulation. Liquidation is permissionless and
> legal on Solana, but a wrong account layout or a mispriced collateral swap loses
> the borrow-plus-fee on every attempt.

---

## PROMPT

You are extending an existing Solana MEV bot (Rust, Solana SDK 1.18, borsh 0.10)
to add a **flash-loan-funded MarginFi liquidator**. The bot already has a working
Kamino KLend flash-loan wrapper, a multi-DEX swap layer, Jito bundle submission,
a circuit breaker, and demo-mode-by-default execution. Reuse all of it. Do not
rebuild what exists.

### Goal

Continuously scan MarginFi v2 lending accounts, find accounts whose health has
dropped below the liquidation threshold, and atomically:

1. Flash-borrow the liability asset (or SOL, then swap) from Kamino KLend.
2. Call MarginFi `lending_account_liquidate` to seize discounted collateral by
   repaying part of the unhealthy account's debt.
3. Swap the seized collateral back to the borrowed asset on the best DEX.
4. Repay the flash loan + fee.
5. Keep the liquidation bonus as profit.

The entire sequence must be one atomic transaction. If any leg fails, the flash
loan repay fails and the whole transaction reverts — you risk only fees.

### Reuse these existing components (read them first)

- `src/chain/transaction.rs`
  - `build_flash_borrow_ix`, `build_flash_repay_ix`, `build_flashloan_transaction`
    — the KLend flash-loan wrapper. Study the exact account order and the rule that
    **repay `liquidityAmount` equals the borrow amount, not borrow+fee** (Kamino
    charges the ~0.09% fee internally).
  - `calculate_optimal_flashloan` — sizing helper; adapt its fee-aware math for
    liquidation economics.
  - The DEX swap-instruction builders (`build_raydium_swap_from_step`,
    `build_damm_v2_swap_from_step`, etc.) for the collateral→liability swap leg.
- `src/config.rs` — `BotConfig::from_env()`. Add new env vars here following the
  existing `flashloan_*` pattern (lines ~20-23 and ~114-121).
- `src/jito/` — bundle submission. Liquidations are competitive; submit via Jito
  with a tip and, optionally, spam multiple RPCs (`SPAM_ENABLED`).
- Circuit breaker / drawdown protection in `main.rs` — wire the liquidator into
  the same safety gates.

### New module: `src/liquidator/`

Create a self-contained module mirroring the `src/dex/<name>/` convention:

```
src/liquidator/
  mod.rs              # LiquidatorEngine: scan → evaluate → build → submit
  marginfi/
    constants.rs      # program IDs, group pubkey, discriminators
    account.rs        # Borsh layouts: MarginfiAccount, Bank, balances
    health.rs         # asset/liability weighting, health factor, max repay
    liquidate_ix.rs   # build lending_account_liquidate instruction
  scanner.rs          # getProgramAccounts + WebSocket refresh of margin accounts
  oracle.rs           # price feeds for collateral/liability valuation
```

Register it in `src/lib.rs` / `src/main.rs`.

### MarginFi specifics you must get exactly right

1. **Program & group.** MarginFi v2 program:
   `MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA`. Load the lending group and its
   `Bank` accounts. Each `Bank` holds the mint, vaults, oracle key, and the asset
   & liability weights used for health.

2. **Account layout.** `MarginfiAccount` stores a fixed array of `Balance` slots
   (each references a `Bank` and holds `asset_shares` / `liability_shares` as
   `WrappedI80F48` fixed-point). Deserialize with Borsh; field order must match the
   on-chain IDL exactly. **Verify every offset against a live mainnet account** —
   the same discipline `CLAUDE.md` documents for the DAMM/Raydium layouts. Do not
   trust a guessed layout; dump a real account with `getAccountInfo` and diff.

3. **Health computation (`health.rs`).**
   - Convert each balance's shares to native amounts using the bank's
     `asset_share_value` / `liability_share_value`.
   - Value each in USD via the bank oracle price.
   - Weighted asset value = Σ(asset_usd × bank.asset_weight_maint).
   - Weighted liability value = Σ(liab_usd × bank.liability_weight_maint).
   - Account is liquidatable when `weighted_assets < weighted_liabilities`
     (maintenance health < 0).
   - Compute **max liquidatable amount**: MarginFi caps how much you can seize per
     call (liquidator + insurance fee, collateral bonus). Respect the protocol's
     liquidation limit so the instruction does not fail.

4. **`lending_account_liquidate` instruction (`liquidate_ix.rs`).** Build with the
   correct discriminator and account list: signer/liquidator marginfi account,
   liquidatee marginfi account, asset bank + liability bank (with their vaults and
   oracles), plus the two oracle accounts passed as `remaining_accounts`. The
   liquidator must itself hold a MarginFi account with a deposit in the asset bank
   to receive seized collateral — decide whether to open one at startup or use an
   existing one, and document it.

5. **Oracles (`oracle.rs`).** MarginFi banks reference Pyth or SwitchboardV2
   feeds. Parse the current price + confidence from the feed account. Reject
   opportunities when confidence interval is too wide or the price is stale — a bad
   price is how liquidators lose money.

### Economic gating (do not skip)

Before building any transaction, require:

```
seized_collateral_usd * (1 - swap_slippage) * (1 - swap_fees)
  > repaid_liability_usd
  + flashloan_fee (0.09% of borrow)
  + jito_tip
  + priority_fee
  + gas
```

Reuse `gas_fee.rs` thresholds and `volume_weighted_slippage.rs` for the collateral
swap-out estimate. If the collateral is illiquid (thin pools), the swap-out leg
can erase the bonus — size the liquidation to what the DEX can absorb, exactly as
the CLMM sizing gotcha in `CLAUDE.md` warns.

### Transaction assembly

Order the atomic transaction as:

```
[compute_limit, compute_price,
 flash_borrow(liability_or_SOL),
 (optional swap SOL→liability),
 marginfi_liquidate,
 swap(seized_collateral → borrowed_asset),
 flash_repay(borrow_amount)]
```

Build it through the existing `build_flashloan_transaction` path where possible; if
the extra legs push you past the pattern, add a `build_liquidation_transaction`
alongside it that follows the same borrow-index / repay-amount rules. **Mind the
1232-byte v0 limit** — use the Address Lookup Table (`CLAUDE.md` documents the
active ALT) and extend it with the MarginFi group, bank, vault, and oracle
accounts.

### Config (add to `.env` and `config.rs`)

```
MARGINFI_LIQUIDATOR_ENABLED=true
MARGINFI_GROUP=<group pubkey>
MARGINFI_LIQUIDATOR_ACCOUNT=<your marginfi account pubkey>
MARGINFI_MIN_PROFIT_USD=5
MARGINFI_MAX_POSITION_SOL=50
MARGINFI_SCAN_INTERVAL_MS=400
MARGINFI_HEALTH_BUFFER=0.0        # only act below this maintenance health
```

Reuse existing `FLASHLOAN_*`, `JITO_*`, `SPAM_*`, and `ENABLE_REAL_EXECUTION`.

### Scanning strategy (`scanner.rs`)

- Bootstrap with `getProgramAccounts` on the MarginFi program filtered to the
  group, with a `dataSize` filter for `MarginfiAccount`.
- Keep accounts warm via WebSocket (`pool_subscription.rs` pattern) and re-check
  health on every oracle price tick, not on a fixed timer alone — liquidations
  trigger on price moves.
- Maintain a priority queue by "closeness to liquidation × size" so the hottest
  accounts are re-evaluated first.

### Safety & correctness requirements

- **Demo mode default.** Log the full intended transaction and simulated profit
  without sending until `ENABLE_REAL_EXECUTION=true`.
- **Simulate every transaction** (`simulateTransaction`) before sending; drop on
  any error.
- Wire into the existing circuit breaker (pause after N consecutive failures) and
  drawdown protection.
- Never log or print private keys. Follow `wallet_integration.rs` for key loading.
- Add unit tests: health-factor math against known account snapshots, max-repay
  cap, and the economic gate. Add a layout test that deserializes a captured
  mainnet `MarginfiAccount` and `Bank` byte blob and asserts known field values.

### Build & verify

```bash
cargo build --release
cargo test --lib
RUST_LOG=solana_mev_bot=debug cargo run   # demo mode; confirm scan + simulate logs
```

Deliver: the new module, config wiring, tests passing, `cargo build --release`
clean, and a short note in `CLAUDE.md` under "Known Gotchas" recording the verified
MarginFi account/bank offsets and the liquidation account list (so the next change
does not re-derive them). Do not enable real execution in code or committed config.

### Definition of done

- `cargo build --release` and `cargo test --lib` pass.
- In demo mode the bot logs at least one correctly-evaluated liquidation candidate
  from live mainnet data with a positive simulated net profit after all fees.
- The atomic transaction simulates successfully against mainnet
  (`simulateTransaction` returns no error) with the flash-loan repay satisfied.

## END PROMPT
