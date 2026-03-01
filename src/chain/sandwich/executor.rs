//! Sandwich executor — builds and submits Jito bundles for sandwich opportunities.
//!
//! Currently a structured stub that logs planned actions. Full execution requires
//! building swap instructions for the specific DEX (using the existing TransactionBuilder)
//! and submitting via the Jito block engine.
//!
//! The bundle structure is: [frontrun_tx, backrun_tx]
//! (True 3-TX sandwiches that include the victim TX require the victim's signed bytes,
//! which are not available in a post-execution detection model.)

use crate::chain::sandwich::monitor::SandwichOpportunity;
use crate::chain::sandwich::calculator::SandwichPlan;
use tracing::info;

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Result of a sandwich execution attempt.
#[derive(Debug, Clone)]
pub struct SandwichExecutionResult {
    pub submitted: bool,
    pub bundle_id: Option<String>,
    pub error: Option<String>,
}

impl SandwichExecutionResult {
    fn skipped(reason: &str) -> Self {
        Self {
            submitted: false,
            bundle_id: None,
            error: Some(reason.to_string()),
        }
    }

    fn stub_submitted() -> Self {
        Self {
            submitted: true,
            bundle_id: Some("stub-bundle-id".to_string()),
            error: None,
        }
    }
}

/// Builds and (when live) submits 2-TX Jito bundles for sandwich attacks.
pub struct SandwichExecutor {
    pub enabled: bool,
    pub jito_tip_lamports: u64,
}

impl SandwichExecutor {
    pub fn new(enabled: bool, jito_tip_lamports: u64) -> Self {
        Self { enabled, jito_tip_lamports }
    }

    /// Execute a sandwich given an opportunity and a computed plan.
    ///
    /// In demo mode (enabled=false) this only logs. In live mode it is a stub
    /// that logs the bundle it *would* send — full Jito SDK integration is pending.
    pub fn execute(
        &self,
        opp: &SandwichOpportunity,
        plan: &SandwichPlan,
    ) -> SandwichExecutionResult {
        if !self.enabled {
            info!(
                "[SandwichExec] (disabled) Would sandwich: {} | frontrun={:.4} SOL profit={:.4} SOL conf={}",
                opp.description,
                plan.frontrun_lamports as f64 / LAMPORTS_PER_SOL,
                plan.expected_profit_lamports as f64 / LAMPORTS_PER_SOL,
                plan.confidence,
            );
            return SandwichExecutionResult::skipped("sandwich disabled");
        }

        // ── Live stub ───────────────────────────────────────────────────────
        // TODO: replace with real bundle construction + Jito submission.
        //
        // Steps:
        //   1. Build frontrun swap instruction via TransactionBuilder:
        //      - action = "buy", amount_in = plan.frontrun_lamports
        //      - pool = opp.pool_address, dex = opp.dex_name
        //   2. Build backrun swap instruction (sell tokens received from frontrun):
        //      - action = "sell", amount_in = estimated tokens received
        //   3. Add SetComputeUnitPrice + Jito tip transfer instructions
        //   4. Create two VersionedTransactions (frontrun_tx, backrun_tx)
        //   5. jito_client.send_bundle([frontrun_tx, backrun_tx]).await
        //   6. Track result and update AI trade memory
        info!(
            "[SandwichExec] LIVE (stub) bundle: pool={} dex={} frontrun={:.4} SOL est_profit={:.4} SOL tip={} lamps",
            opp.pool_address,
            opp.dex_name,
            plan.frontrun_lamports as f64 / LAMPORTS_PER_SOL,
            plan.expected_profit_lamports as f64 / LAMPORTS_PER_SOL,
            self.jito_tip_lamports,
        );

        SandwichExecutionResult::stub_submitted()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::sandwich::monitor::SandwichOpportunity;
    use crate::chain::sandwich::calculator::SandwichPlan;
    use solana_sdk::pubkey::Pubkey;

    fn make_opp() -> SandwichOpportunity {
        SandwichOpportunity {
            pool_address: Pubkey::new_unique(),
            dex_name: "Raydium".to_string(),
            victim_swap_sol: 10.0,
            victim_token_amount: 1_000_000,
            victim_is_buy: true,
            mint: Pubkey::new_unique(),
            slot: 100,
            description: "test".to_string(),
        }
    }

    fn make_plan() -> SandwichPlan {
        SandwichPlan {
            frontrun_lamports: 2_000_000_000,
            expected_profit_lamports: 5_000_000,
            confidence: 70,
            description: "test plan".to_string(),
        }
    }

    #[test]
    fn test_disabled_executor_does_not_submit() {
        let exec = SandwichExecutor::new(false, 10_000);
        let result = exec.execute(&make_opp(), &make_plan());
        assert!(!result.submitted);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_enabled_executor_returns_stub_result() {
        let exec = SandwichExecutor::new(true, 10_000);
        let result = exec.execute(&make_opp(), &make_plan());
        assert!(result.submitted);
        assert!(result.bundle_id.is_some());
        assert!(result.error.is_none());
    }
}
