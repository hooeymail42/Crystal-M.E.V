//! Sandwich calculator — computes optimal frontrun size and expected profit.
//!
//! Uses constant-product AMM math (x*y=k) to simulate the three-leg sandwich:
//!   1. Our frontrun:  SOL → Token  (we buy, price moves against victim)
//!   2. Victim swap:   SOL → Token  (they buy at worse price)
//!   3. Our backrun:   Token → SOL  (we sell into victim's elevated price)
//!
//! The optimal frontrun amount maximises our net profit = backrun_out - frontrun_in.
//! We binary-search over frontrun sizes and pick the best one.

use crate::chain::sandwich::monitor::SandwichOpportunity;
use tracing::debug;

// Lamports per SOL.
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// A computed sandwich plan — the output of the calculator.
#[derive(Debug, Clone)]
pub struct SandwichPlan {
    /// Optimal frontrun input in lamports.
    pub frontrun_lamports: u64,
    /// Expected profit after all fees and the Jito tip, in lamports.
    pub expected_profit_lamports: i64,
    /// Confidence score 0–100.
    pub confidence: u8,
    /// Human-readable summary.
    pub description: String,
}

/// Calculator configuration.
#[derive(Debug, Clone)]
pub struct SandwichCalculatorConfig {
    /// Fee fraction per swap leg (e.g. 0.0025 = 25 bps).
    pub fee_fraction: f64,
    /// Jito tip in lamports added to the cost side.
    pub jito_tip_lamports: u64,
    /// Minimum net profit required to emit a plan (lamports).
    pub min_profit_lamports: u64,
    /// Maximum frontrun size as a fraction of pool SOL reserve.
    pub max_frontrun_fraction: f64,
}

impl Default for SandwichCalculatorConfig {
    fn default() -> Self {
        Self {
            fee_fraction: 0.0025,          // 25 bps
            jito_tip_lamports: 10_000,
            min_profit_lamports: 2_000_000, // 0.002 SOL
            max_frontrun_fraction: 0.05,    // at most 5% of pool SOL reserve
        }
    }
}

/// Stateless calculator — call `compute()` for each opportunity.
pub struct SandwichCalculator {
    config: SandwichCalculatorConfig,
}

impl SandwichCalculator {
    pub fn new(config: SandwichCalculatorConfig) -> Self {
        Self { config }
    }

    /// Compute a sandwich plan for `opp` given current pool reserves.
    ///
    /// `sol_reserve` and `token_reserve` are the pool reserves **before** the victim swap.
    ///
    /// Returns `None` if no profitable plan exists.
    pub fn compute(
        &self,
        opp: &SandwichOpportunity,
        sol_reserve: u64,
        token_reserve: u64,
    ) -> Option<SandwichPlan> {
        if sol_reserve == 0 || token_reserve == 0 {
            return None;
        }

        // Only handle victim buys (SOL→Token) for now; that's the common case.
        // A victim buy increases the token price — we frontrun by also buying, then sell back.
        if !opp.victim_is_buy {
            return None;
        }

        let victim_sol = (opp.victim_swap_sol * LAMPORTS_PER_SOL) as u64;
        let max_frontrun = ((sol_reserve as f64) * self.config.max_frontrun_fraction) as u64;
        let max_frontrun = max_frontrun.min(victim_sol * 2); // cap at 2× victim size

        if max_frontrun == 0 {
            return None;
        }

        // Binary-search for the frontrun amount that maximises net profit.
        let mut best_profit: i64 = 0;
        let mut best_frontrun: u64 = 0;
        let steps = 20usize;

        for i in 1..=steps {
            let frontrun = max_frontrun * i as u64 / steps as u64;
            let profit = self.simulate_sandwich(sol_reserve, token_reserve, frontrun, victim_sol);
            if profit > best_profit {
                best_profit = profit;
                best_frontrun = frontrun;
            }
        }

        // Deduct Jito tip.
        let net_profit = best_profit - self.config.jito_tip_lamports as i64;

        if net_profit < self.config.min_profit_lamports as i64 {
            debug!(
                "[SandwichCalc] pool={} net_profit={:.4} SOL below threshold — skipping",
                opp.pool_address,
                net_profit as f64 / LAMPORTS_PER_SOL
            );
            return None;
        }

        let confidence = self.score_confidence(opp, sol_reserve, net_profit);

        Some(SandwichPlan {
            frontrun_lamports: best_frontrun,
            expected_profit_lamports: net_profit,
            confidence,
            description: format!(
                "{} pool {} — frontrun={:.4} SOL profit={:.4} SOL conf={}",
                opp.dex_name,
                opp.pool_address,
                best_frontrun as f64 / LAMPORTS_PER_SOL,
                net_profit as f64 / LAMPORTS_PER_SOL,
                confidence,
            ),
        })
    }

    /// Simulate the three-leg sandwich using constant-product (x*y=k) math.
    ///
    /// Returns net profit in lamports (can be negative).
    fn simulate_sandwich(
        &self,
        sol_reserve: u64,
        token_reserve: u64,
        frontrun_sol: u64,
        victim_sol: u64,
    ) -> i64 {
        let fee = self.config.fee_fraction;

        // ── Leg 1: our frontrun (SOL → Token) ───────────────────────────────
        let (tokens_out_1, sol1, tok1) =
            self.amm_swap_sol_to_token(sol_reserve, token_reserve, frontrun_sol, fee);

        // ── Leg 2: victim swap (SOL → Token) ────────────────────────────────
        let (_tokens_victim, sol2, tok2) =
            self.amm_swap_sol_to_token(sol1, tok1, victim_sol, fee);

        // ── Leg 3: our backrun (Token → SOL) ────────────────────────────────
        let (sol_out_3, _, _) =
            self.amm_swap_token_to_sol(sol2, tok2, tokens_out_1, fee);

        // Net: what we got back minus what we spent.
        sol_out_3 as i64 - frontrun_sol as i64
    }

    /// AMM: swap `sol_in` for tokens.  Returns (tokens_out, new_sol_reserve, new_token_reserve).
    fn amm_swap_sol_to_token(
        &self,
        sol_res: u64,
        tok_res: u64,
        sol_in: u64,
        fee: f64,
    ) -> (u64, u64, u64) {
        let sol_in_after_fee = (sol_in as f64 * (1.0 - fee)) as u64;
        let k = sol_res as u128 * tok_res as u128;
        let new_sol = sol_res + sol_in_after_fee;
        let new_tok = (k / new_sol as u128) as u64;
        let tokens_out = tok_res.saturating_sub(new_tok);
        (tokens_out, sol_res + sol_in, new_tok)
    }

    /// AMM: swap `tok_in` for SOL.  Returns (sol_out, new_sol_reserve, new_token_reserve).
    fn amm_swap_token_to_sol(
        &self,
        sol_res: u64,
        tok_res: u64,
        tok_in: u64,
        fee: f64,
    ) -> (u64, u64, u64) {
        let tok_in_after_fee = (tok_in as f64 * (1.0 - fee)) as u64;
        let k = sol_res as u128 * tok_res as u128;
        let new_tok = tok_res + tok_in_after_fee;
        let new_sol = (k / new_tok as u128) as u64;
        let sol_out = sol_res.saturating_sub(new_sol);
        (sol_out, new_sol, tok_res + tok_in)
    }

    /// Heuristic confidence score (0–100).
    fn score_confidence(&self, opp: &SandwichOpportunity, sol_reserve: u64, profit_lamports: i64) -> u8 {
        let mut score: f64 = 50.0;

        // Higher profit → more confident.
        let profit_sol = profit_lamports as f64 / LAMPORTS_PER_SOL;
        score += (profit_sol * 100.0).min(25.0);

        // Larger victim swap relative to pool → more price impact → better sandwich.
        let impact = opp.victim_swap_sol / (sol_reserve as f64 / LAMPORTS_PER_SOL);
        score += (impact * 200.0).min(20.0);

        // Penalise unknown DEXes.
        if opp.dex_name == "Unknown" {
            score -= 20.0;
        }

        score.clamp(0.0, 100.0) as u8
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::sandwich::monitor::{SandwichConfig, SandwichOpportunity};
    use solana_sdk::pubkey::Pubkey;

    fn make_opp(victim_sol: f64, is_buy: bool) -> SandwichOpportunity {
        SandwichOpportunity {
            pool_address: Pubkey::new_unique(),
            dex_name: "Raydium".to_string(),
            victim_swap_sol: victim_sol,
            victim_token_amount: (victim_sol * 1_000_000.0) as u64,
            victim_is_buy: is_buy,
            mint: Pubkey::new_unique(),
            slot: 100,
            description: format!("test victim {victim_sol} SOL"),
        }
    }

    #[test]
    fn test_no_plan_for_sell_victim() {
        let calc = SandwichCalculator::new(SandwichCalculatorConfig::default());
        let opp = make_opp(10.0, false); // victim is selling — we skip
        let result = calc.compute(&opp, 1_000_000_000_000, 5_000_000_000_000);
        assert!(result.is_none());
    }

    #[test]
    fn test_profitable_buy_sandwich() {
        let calc = SandwichCalculator::new(SandwichCalculatorConfig {
            min_profit_lamports: 1, // accept tiny profit for test
            ..SandwichCalculatorConfig::default()
        });
        let opp = make_opp(50.0, true); // large 50 SOL victim buy
        // Pool: 10k SOL × 5M tokens
        let sol_reserve = 10_000 * 1_000_000_000u64;
        let tok_reserve = 5_000_000 * 1_000_000u64;
        let plan = calc.compute(&opp, sol_reserve, tok_reserve);
        assert!(plan.is_some(), "should find a profitable sandwich");
        let plan = plan.unwrap();
        assert!(plan.frontrun_lamports > 0);
        assert!(plan.expected_profit_lamports > 0);
    }

    #[test]
    fn test_amm_swap_round_trip() {
        let calc = SandwichCalculator::new(SandwichCalculatorConfig::default());
        let sol_res = 1_000_000_000_000u64; // 1000 SOL
        let tok_res = 5_000_000_000_000u64; // 5000 tokens (scaled)
        let sol_in = 1_000_000_000u64;      // 1 SOL

        let (tokens_out, new_sol, new_tok) =
            calc.amm_swap_sol_to_token(sol_res, tok_res, sol_in, 0.0025);

        assert!(tokens_out > 0, "should receive tokens");
        assert_eq!(new_sol, sol_res + sol_in, "new sol reserve should increase");
        assert!(new_tok < tok_res, "token reserve should decrease");
    }
}
