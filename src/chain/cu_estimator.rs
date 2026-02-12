use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    message::{VersionedMessage, v0},
    signature::{Keypair, Signer},
    transaction::VersionedTransaction,
};
use std::sync::Arc;
use tracing::{info, warn};

/// Default CU limit used for simulation (max allowed on Solana)
const SIMULATION_CU_LIMIT: u32 = 1_400_000;

/// Buffer multiplier applied on top of actual CU consumed during simulation
const DEFAULT_CU_BUFFER_PCT: f64 = 0.15; // 15%

/// Minimum CU to request (avoid too-small values that could fail)
const MIN_CU_LIMIT: u32 = 50_000;

/// Maximum CU to request
const MAX_CU_LIMIT: u32 = 1_400_000;

/// Estimates compute units by simulating a transaction with max CU budget,
/// then reading the actual units consumed from the simulation result.
pub struct CuEstimator {
    rpc: Arc<RpcClient>,
    buffer_pct: f64,
}

impl CuEstimator {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            buffer_pct: DEFAULT_CU_BUFFER_PCT,
        }
    }

    pub fn with_buffer(mut self, buffer_pct: f64) -> Self {
        self.buffer_pct = buffer_pct;
        self
    }

    /// Estimate CU for a set of swap instructions by simulating with max budget.
    /// Returns the recommended CU limit (actual + buffer), or a fallback on failure.
    pub fn estimate_cu(
        &self,
        swap_instructions: &[Instruction],
        payer: &Keypair,
        fallback_cu: u32,
    ) -> u32 {
        match self.simulate_for_cu(swap_instructions, payer) {
            Ok(actual_cu) => {
                let buffered = (actual_cu as f64 * (1.0 + self.buffer_pct)) as u32;
                let clamped = buffered.clamp(MIN_CU_LIMIT, MAX_CU_LIMIT);
                info!(
                    "CU estimation: actual={}, buffered={}, clamped={}",
                    actual_cu, buffered, clamped
                );
                clamped
            }
            Err(e) => {
                warn!("CU estimation failed, using fallback {}: {}", fallback_cu, e);
                fallback_cu
            }
        }
    }

    /// Run simulation and extract units_consumed from the result.
    fn simulate_for_cu(
        &self,
        swap_instructions: &[Instruction],
        payer: &Keypair,
    ) -> Result<u64> {
        let mut instructions = Vec::with_capacity(swap_instructions.len() + 1);

        // Use max CU budget so simulation doesn't fail due to CU limits
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(
            SIMULATION_CU_LIMIT,
        ));

        instructions.extend_from_slice(swap_instructions);

        let blockhash = self.rpc.get_latest_blockhash()?;
        let message = v0::Message::try_compile(
            &payer.pubkey(),
            &instructions,
            &[],  // No ALTs needed for simulation
            blockhash,
        )?;
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &[payer],
        )?;

        let result = self.rpc.simulate_transaction(&tx)?;

        if let Some(err) = result.value.err {
            return Err(anyhow::anyhow!("Simulation error: {:?}", err));
        }

        result
            .value
            .units_consumed
            .ok_or_else(|| anyhow::anyhow!("Simulation did not return units_consumed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cu_clamping() {
        // Verify buffer and clamping logic
        let actual = 200_000u64;
        let buffer_pct = 0.15;
        let buffered = (actual as f64 * (1.0 + buffer_pct)) as u32;
        // Float rounding: 200000 * 1.15 can be 229999 or 230000
        assert!(buffered >= 229_999 && buffered <= 230_000);

        let clamped = buffered.clamp(MIN_CU_LIMIT, MAX_CU_LIMIT);
        assert!(clamped >= 229_999 && clamped <= 230_000);

        // Very small
        let small = (1000_f64 * 1.15) as u32;
        assert_eq!(small.clamp(MIN_CU_LIMIT, MAX_CU_LIMIT), MIN_CU_LIMIT);

        // Very large
        let large = (1_500_000_f64 * 1.15) as u32;
        assert_eq!(large.clamp(MIN_CU_LIMIT, MAX_CU_LIMIT), MAX_CU_LIMIT);
    }
}
