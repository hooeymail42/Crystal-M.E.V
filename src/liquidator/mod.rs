//! Flash-loan-funded MarginFi v2 liquidator.
//!
//! Pipeline: [`scanner`] fetches MarginFi accounts + banks + oracle prices →
//! [`marginfi::health`] scores them → the economic gate here rejects
//! unprofitable candidates → [`marginfi::liquidate_ix`] + the existing Kamino
//! flash-loan wrapper in `chain::transaction` assemble one atomic transaction.
//!
//! # Safety posture
//!
//! - Demo-mode by default: nothing is sent unless `ENABLE_REAL_EXECUTION=true`
//!   **and** the collateral-swap routing is wired (see
//!   [`LiquidatorEngine::assemble_transaction`]).
//! - Every candidate passes the economic gate ([`estimate_net_profit_usd`]) and
//!   an oracle-usability check before a transaction is ever built.
//! - The MarginFi account/bank byte layouts and the liquidate account list are
//!   marked `VERIFY` in their modules and must be confirmed against mainnet
//!   before real funds are used.

pub mod marginfi;
pub mod oracle;
pub mod scanner;

use std::sync::Arc;

use anyhow::{bail, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::instruction::Instruction;
use solana_sdk::transaction::VersionedTransaction;
use tracing::{debug, info, warn};

use crate::chain::transaction::TransactionBuilder;

use marginfi::account::{Bank, MarginfiAccount};
use marginfi::health::{self, WeightedPosition};
use scanner::MarginfiScanner;

/// Runtime configuration for the liquidator, populated from `BotConfig`.
#[derive(Debug, Clone)]
pub struct LiquidatorConfig {
    pub enabled: bool,
    pub program: Pubkey,
    pub group: Pubkey,
    /// The liquidator's own MarginFi account (receives seized collateral).
    /// `None` disables real transaction assembly (scan/evaluate still run).
    pub liquidator_marginfi_account: Option<Pubkey>,
    /// Minimum net profit, in USD, required to act on a candidate.
    pub min_profit_usd: f64,
    /// Maximum flash-loan position, expressed in SOL, per liquidation.
    pub max_position_sol: f64,
    /// Only act when maintenance health is below `-buffer * liabilities`.
    pub health_buffer: f64,
    /// MarginFi liquidation bonus (collateral discount), e.g. 0.05 = 5%.
    pub liquidation_bonus: f64,
    /// Max oracle confidence-to-price ratio (e.g. 0.02 = 2%).
    pub max_oracle_conf_pct: f64,
    /// Max oracle staleness in seconds.
    pub max_oracle_age_secs: i64,
    /// Kamino flash-loan fee fraction (e.g. 0.0009 = 0.09%).
    pub flashloan_fee_pct: f64,
    /// Expected collateral-swap slippage fraction (e.g. 0.005 = 0.5%).
    pub swap_slippage_pct: f64,
    /// Expected collateral-swap DEX fee fraction (e.g. 0.003 = 0.3%).
    pub swap_fee_pct: f64,
    /// Fixed per-attempt costs in USD (Jito tip + priority fee + gas).
    pub fixed_cost_usd: f64,
}

impl LiquidatorConfig {
    pub fn is_active(&self) -> bool {
        self.enabled
    }
}

/// A scored, economically-viable liquidation opportunity.
#[derive(Debug, Clone)]
pub struct LiquidationCandidate {
    pub liquidatee: Pubkey,
    pub health_factor: f64,
    /// Bank of the debt to repay.
    pub liab_bank: Pubkey,
    /// Bank of the collateral to seize.
    pub asset_bank: Pubkey,
    /// USD value of debt to repay this call.
    pub repay_usd: f64,
    /// Estimated net profit in USD after all costs.
    pub net_profit_usd: f64,
}

/// Build a [`LiquidatorConfig`] from the global `BotConfig`. Returns `Ok(None)`
/// when the liquidator is disabled. An empty `MARGINFI_GROUP` falls back to the
/// mainnet main group; an empty liquidator account leaves real assembly
/// disabled (scan/evaluate still run).
pub fn config_from_bot(cfg: &crate::config::BotConfig) -> Result<Option<LiquidatorConfig>> {
    use std::str::FromStr;
    if !cfg.marginfi_liquidator_enabled {
        return Ok(None);
    }
    let group = if cfg.marginfi_group.trim().is_empty() {
        marginfi::constants::marginfi_main_group()
    } else {
        Pubkey::from_str(cfg.marginfi_group.trim())?
    };
    let liquidator_marginfi_account = if cfg.marginfi_liquidator_account.trim().is_empty() {
        None
    } else {
        Some(Pubkey::from_str(cfg.marginfi_liquidator_account.trim())?)
    };
    Ok(Some(LiquidatorConfig {
        enabled: true,
        program: marginfi::constants::marginfi_program_id(),
        group,
        liquidator_marginfi_account,
        min_profit_usd: cfg.marginfi_min_profit_usd,
        max_position_sol: cfg.marginfi_max_position_sol,
        health_buffer: cfg.marginfi_health_buffer,
        liquidation_bonus: cfg.marginfi_liquidation_bonus,
        max_oracle_conf_pct: cfg.marginfi_max_oracle_conf_pct,
        max_oracle_age_secs: cfg.marginfi_max_oracle_age_secs,
        flashloan_fee_pct: 0.0009,
        swap_slippage_pct: cfg.marginfi_swap_slippage_pct,
        swap_fee_pct: cfg.marginfi_swap_fee_pct,
        fixed_cost_usd: cfg.marginfi_fixed_cost_usd,
    }))
}

/// Spawn a demo-safe background scan loop for the liquidator. It scans and logs
/// profitable candidates on `scan_interval_ms`; it never sends a transaction
/// (the collateral-swap routing and layout verification are the remaining gates
/// before real execution). Returns immediately with the spawned task handle.
pub fn spawn_scan_loop(
    mut engine: LiquidatorEngine,
    scan_interval_ms: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        info!(
            "MarginFi liquidator scan loop started (interval {}ms, DEMO — no sends)",
            scan_interval_ms
        );
        loop {
            let now = chrono::Utc::now().timestamp();
            match engine.run_once(now) {
                Ok(_c) => {}
                Err(e) => warn!("liquidator scan error: {}", e),
            }
            std::thread::sleep(std::time::Duration::from_millis(scan_interval_ms));
        }
    })
}

/// The economic gate. Returns estimated **net** profit in USD for repaying
/// `repay_usd` of debt and seizing collateral at `liquidation_bonus` discount,
/// after flash-loan fee, collateral-swap slippage + fees, and fixed costs.
///
/// Model: seizing collateral worth `repay_usd * (1 + bonus)`, swapping it back
/// to the borrowed asset loses `slippage + fee`, then we repay the borrow plus
/// the flash-loan fee, and pay fixed costs once.
pub fn estimate_net_profit_usd(
    repay_usd: f64,
    liquidation_bonus: f64,
    flashloan_fee_pct: f64,
    swap_slippage_pct: f64,
    swap_fee_pct: f64,
    fixed_cost_usd: f64,
) -> f64 {
    let seized_usd = repay_usd * (1.0 + liquidation_bonus);
    let swap_out_usd = seized_usd * (1.0 - swap_slippage_pct) * (1.0 - swap_fee_pct);
    let flashloan_fee_usd = repay_usd * flashloan_fee_pct;
    swap_out_usd - repay_usd - flashloan_fee_usd - fixed_cost_usd
}

/// Orchestrates scanning, scoring, and (when wired) execution.
pub struct LiquidatorEngine {
    cfg: LiquidatorConfig,
    scanner: MarginfiScanner,
    tx_builder: Arc<TransactionBuilder>,
    /// SOL price in USD, used to convert the SOL-denominated position cap and to
    /// value SOL-denominated flash borrows. Refreshed by the caller.
    sol_price_usd: f64,
}

impl LiquidatorEngine {
    pub fn new(
        cfg: LiquidatorConfig,
        rpc: Arc<RpcClient>,
        tx_builder: Arc<TransactionBuilder>,
    ) -> Self {
        let scanner = MarginfiScanner::new(rpc, cfg.program, cfg.group);
        Self {
            cfg,
            scanner,
            tx_builder,
            sol_price_usd: 0.0,
        }
    }

    pub fn set_sol_price_usd(&mut self, price: f64) {
        self.sol_price_usd = price;
    }

    /// Build the weighted positions for one account, loading each referenced
    /// bank and its oracle price. Positions whose oracle is unusable are
    /// dropped and the account is skipped (returns `None`).
    fn positions_for(
        &mut self,
        account: &MarginfiAccount,
        now_unix: i64,
    ) -> Option<(Vec<WeightedPosition>, Vec<(Pubkey, Bank)>)> {
        let mut positions = Vec::new();
        let mut banks = Vec::new();
        for bal in account.active_balances() {
            let bank = match self.scanner.load_bank(&bal.bank_pk) {
                Ok(b) => b,
                Err(e) => {
                    debug!("skip balance: bank {} load failed: {}", bal.bank_pk, e);
                    return None;
                }
            };
            let price = match self.scanner.load_price(&bank.oracle_key) {
                Ok(p) => p,
                Err(e) => {
                    debug!("skip account: oracle {} failed: {}", bank.oracle_key, e);
                    return None;
                }
            };
            if !price.is_usable(self.cfg.max_oracle_conf_pct, self.cfg.max_oracle_age_secs, now_unix)
            {
                debug!("skip account: unusable price for bank {}", bal.bank_pk);
                return None;
            }

            let asset_amt = bank.asset_amount(&bal.asset_shares);
            if asset_amt > 0.0 {
                positions.push(WeightedPosition {
                    amount: asset_amt,
                    price_usd: price.price_usd,
                    decimals: bank.mint_decimals,
                    weight_maint: bank.asset_weight_maint.to_f64(),
                    is_liability: false,
                });
            }
            let liab_amt = bank.liability_amount(&bal.liability_shares);
            if liab_amt > 0.0 {
                positions.push(WeightedPosition {
                    amount: liab_amt,
                    price_usd: price.price_usd,
                    decimals: bank.mint_decimals,
                    weight_maint: bank.liability_weight_maint.to_f64(),
                    is_liability: true,
                });
            }
            banks.push((bal.bank_pk, bank));
        }
        Some((positions, banks))
    }

    /// Evaluate a single account into a candidate, applying the health trigger
    /// and the economic gate. Returns `None` when not liquidatable or not
    /// profitable.
    pub fn evaluate_account(
        &mut self,
        pk: &Pubkey,
        account: &MarginfiAccount,
        now_unix: i64,
    ) -> Option<LiquidationCandidate> {
        let (positions, banks) = self.positions_for(account, now_unix)?;
        let report = health::evaluate(&positions);
        if !report.is_liquidatable(self.cfg.health_buffer) {
            return None;
        }

        // Pick the largest liability as the debt to repay and the largest asset
        // as the collateral to seize.
        let liab = banks
            .iter()
            .filter_map(|(bpk, b)| {
                let owed = account
                    .active_balances()
                    .find(|x| &x.bank_pk == bpk)
                    .map(|x| b.liability_amount(&x.liability_shares))
                    .unwrap_or(0.0);
                if owed > 0.0 {
                    Some((*bpk, b.clone(), owed))
                } else {
                    None
                }
            })
            .max_by(|a, c| a.2.partial_cmp(&c.2).unwrap())?;
        let asset = banks
            .iter()
            .filter_map(|(bpk, b)| {
                let held = account
                    .active_balances()
                    .find(|x| &x.bank_pk == bpk)
                    .map(|x| b.asset_amount(&x.asset_shares))
                    .unwrap_or(0.0);
                if held > 0.0 {
                    Some((*bpk, b.clone(), held))
                } else {
                    None
                }
            })
            .max_by(|a, c| a.2.partial_cmp(&c.2).unwrap())?;

        let repay_usd = health::max_repay_usd(
            &report,
            liab.1.liability_weight_maint.to_f64(),
            asset.1.asset_weight_maint.to_f64(),
            self.cfg.liquidation_bonus,
        );
        if repay_usd <= 0.0 {
            return None;
        }

        // Clamp to the flash-loan position cap (expressed in SOL → USD).
        let repay_usd = if self.sol_price_usd > 0.0 {
            repay_usd.min(self.cfg.max_position_sol * self.sol_price_usd)
        } else {
            repay_usd
        };

        let net = estimate_net_profit_usd(
            repay_usd,
            self.cfg.liquidation_bonus,
            self.cfg.flashloan_fee_pct,
            self.cfg.swap_slippage_pct,
            self.cfg.swap_fee_pct,
            self.cfg.fixed_cost_usd,
        );
        if net < self.cfg.min_profit_usd {
            debug!(
                "candidate {} below min profit: net=${:.2} < ${:.2}",
                pk, net, self.cfg.min_profit_usd
            );
            return None;
        }

        Some(LiquidationCandidate {
            liquidatee: *pk,
            health_factor: report.health_factor(),
            liab_bank: liab.0,
            asset_bank: asset.0,
            repay_usd,
            net_profit_usd: net,
        })
    }

    /// One scan → evaluate cycle. Returns the profitable candidates found,
    /// sorted by net profit (descending). Does NOT send anything.
    pub fn run_once(&mut self, now_unix: i64) -> Result<Vec<LiquidationCandidate>> {
        if !self.cfg.is_active() {
            return Ok(vec![]);
        }
        let accounts = self.scanner.scan_accounts()?;
        let mut candidates = Vec::new();
        for (pk, acct) in &accounts {
            if let Some(c) = self.evaluate_account(pk, acct, now_unix) {
                candidates.push(c);
            }
        }
        candidates.sort_by(|a, b| b.net_profit_usd.partial_cmp(&a.net_profit_usd).unwrap());
        info!(
            "liquidator scan: {} accounts, {} profitable candidates",
            accounts.len(),
            candidates.len()
        );
        for c in &candidates {
            info!(
                "  candidate {} hf={:.3} repay=${:.2} net=${:.2}",
                c.liquidatee, c.health_factor, c.repay_usd, c.net_profit_usd
            );
        }
        Ok(candidates)
    }

    /// Wrap the inner liquidation instructions in the Kamino flash-loan borrow/
    /// repay via the existing `TransactionBuilder`. `inner` must contain the
    /// MarginFi liquidate instruction and the collateral→liability swap (and any
    /// MarginFi deposit/withdraw legs) in execution order.
    ///
    /// `borrow_amount` is in the flash-loan reserve's native units; `reserve`,
    /// `reserve_vault`, and `fee_receiver` come from the flash-loan config.
    ///
    /// Returns an error if `inner` is empty — an empty inner set cannot repay
    /// the flash loan, so it is never safe to submit.
    pub fn assemble_transaction(
        &self,
        inner: Vec<Instruction>,
        borrow_amount: u64,
        reserve: &Pubkey,
        reserve_vault: &Pubkey,
        fee_receiver: &Pubkey,
        recent_blockhash: solana_sdk::hash::Hash,
    ) -> Result<VersionedTransaction> {
        if inner.is_empty() {
            bail!("refusing to assemble a flash-loan liquidation with no inner instructions (would not repay)");
        }
        self.tx_builder.build_flashloan_transaction(
            inner,
            borrow_amount,
            reserve,
            reserve_vault,
            fee_receiver,
            recent_blockhash,
        )
    }

    /// Simulate an assembled transaction. Thin pass-through to the builder so the
    /// engine always simulates before any real send.
    pub fn simulate(&self, tx: &VersionedTransaction) -> Result<bool> {
        self.tx_builder.simulate(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profit_gate_positive_when_bonus_covers_costs() {
        // Repay $1000 at 5% bonus; 0.09% flash fee; 0.5% slip; 0.3% swap fee;
        // $0.50 fixed. Bonus ($50) should clear ~ $8 flash+swap+fixed.
        let net = estimate_net_profit_usd(1000.0, 0.05, 0.0009, 0.005, 0.003, 0.5);
        assert!(net > 0.0, "net was {}", net);
    }

    #[test]
    fn profit_gate_negative_when_costs_exceed_bonus() {
        // Tiny bonus, heavy slippage on illiquid collateral wipes it out.
        let net = estimate_net_profit_usd(1000.0, 0.02, 0.0009, 0.05, 0.003, 5.0);
        assert!(net < 0.0, "net was {}", net);
    }

    #[test]
    fn profit_scales_with_size() {
        let small = estimate_net_profit_usd(100.0, 0.05, 0.0009, 0.005, 0.003, 0.5);
        let big = estimate_net_profit_usd(10_000.0, 0.05, 0.0009, 0.005, 0.003, 0.5);
        assert!(big > small);
    }

    #[test]
    fn assemble_rejects_empty_inner() {
        // Build a minimal engine with a demo tx builder to exercise the guard.
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(solana_sdk::signature::Keypair::new());
        let tb = Arc::new(TransactionBuilder::new(
            rpc.clone(),
            payer,
            400_000,
            10_000,
            vec![],
            false,
        ));
        let cfg = LiquidatorConfig {
            enabled: true,
            program: marginfi::constants::marginfi_program_id(),
            group: marginfi::constants::marginfi_main_group(),
            liquidator_marginfi_account: None,
            min_profit_usd: 5.0,
            max_position_sol: 50.0,
            health_buffer: 0.0,
            liquidation_bonus: 0.05,
            max_oracle_conf_pct: 0.02,
            max_oracle_age_secs: 60,
            flashloan_fee_pct: 0.0009,
            swap_slippage_pct: 0.005,
            swap_fee_pct: 0.003,
            fixed_cost_usd: 0.5,
        };
        let engine = LiquidatorEngine::new(cfg, rpc, tb);
        let res = engine.assemble_transaction(
            vec![],
            1_000_000,
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            solana_sdk::hash::Hash::default(),
        );
        assert!(res.is_err());
    }
}
