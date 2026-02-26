#![allow(dead_code)]
use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Default percentile for priority fee selection (0-100)
#[allow(dead_code)]
const DEFAULT_PERCENTILE: usize = 75;

/// How often to refresh priority fee data from the network
#[allow(dead_code)]
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Minimum fee in microlamports per CU (floor)
const MIN_FEE_MICROLAMPORTS: u64 = 100;

/// Maximum fee in microlamports per CU (ceiling to prevent overspend)
const MAX_FEE_MICROLAMPORTS: u64 = 1_000_000; // 1 lamport per CU

/// Dynamically fetches and caches recent prioritization fees from the network.
/// Uses `getRecentPrioritizationFees` RPC to query fees for specific writable
/// accounts, then selects a fee at the configured percentile.
#[allow(dead_code)]
pub struct PriorityFeeEstimator {
    rpc: Arc<RpcClient>,
    percentile: usize,
    cached_fee: u64,
    last_refresh: Option<Instant>,
    refresh_interval: Duration,
}

impl PriorityFeeEstimator {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            percentile: DEFAULT_PERCENTILE,
            cached_fee: MIN_FEE_MICROLAMPORTS,
            last_refresh: None,
            refresh_interval: REFRESH_INTERVAL,
        }
    }

    pub fn with_percentile(mut self, percentile: usize) -> Self {
        self.percentile = percentile.min(100);
        self
    }

    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = interval;
        self
    }

    /// Get the recommended priority fee in microlamports per CU.
    /// Returns cached value if fresh, otherwise fetches from network.
    /// `writable_accounts` are the pool/vault accounts the TX will write to.
    pub fn get_priority_fee(&mut self, writable_accounts: &[Pubkey]) -> u64 {
        if self.should_refresh() {
            match self.fetch_priority_fee(writable_accounts) {
                Ok(fee) => {
                    self.cached_fee = fee;
                    self.last_refresh = Some(Instant::now());
                    info!("Priority fee updated: {} microlamports/CU (p{})", fee, self.percentile);
                }
                Err(e) => {
                    warn!("Failed to fetch priority fees, using cached {}: {}", self.cached_fee, e);
                }
            }
        }
        self.cached_fee
    }

    /// Force a refresh regardless of cache age
    pub fn refresh(&mut self, writable_accounts: &[Pubkey]) -> u64 {
        self.last_refresh = None;
        self.get_priority_fee(writable_accounts)
    }

    /// Get the cached fee without triggering a refresh
    pub fn cached_fee(&self) -> u64 {
        self.cached_fee
    }

    fn should_refresh(&self) -> bool {
        match self.last_refresh {
            None => true,
            Some(last) => last.elapsed() >= self.refresh_interval,
        }
    }

    /// Fetch recent prioritization fees from the RPC for the given accounts.
    fn fetch_priority_fee(&self, writable_accounts: &[Pubkey]) -> Result<u64> {
        // Limit to 128 accounts (RPC constraint)
        let accounts_owned: Vec<Pubkey> = writable_accounts.iter().take(128).copied().collect();
        let response = self.rpc.get_recent_prioritization_fees(&accounts_owned)?;

        if response.is_empty() {
            return Ok(MIN_FEE_MICROLAMPORTS);
        }

        // Extract non-zero fees and sort them
        let mut fees: Vec<u64> = response
            .iter()
            .map(|f| f.prioritization_fee)
            .filter(|&f| f > 0)
            .collect();

        if fees.is_empty() {
            return Ok(MIN_FEE_MICROLAMPORTS);
        }

        fees.sort_unstable();

        // Select fee at the configured percentile
        let idx = (fees.len() * self.percentile / 100).min(fees.len() - 1);
        let selected = fees[idx];

        Ok(selected.clamp(MIN_FEE_MICROLAMPORTS, MAX_FEE_MICROLAMPORTS))
    }

    /// Convert a microlamports-per-CU fee into total lamports for a given CU budget.
    pub fn fee_for_cu(microlamports_per_cu: u64, compute_units: u32) -> u64 {
        (microlamports_per_cu as u128 * compute_units as u128 / 1_000_000) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_for_cu() {
        // 1000 microlamports/CU * 200,000 CU = 200,000,000 microlamports = 200 lamports
        assert_eq!(PriorityFeeEstimator::fee_for_cu(1000, 200_000), 200);

        // 100 microlamports/CU * 400,000 CU = 40 lamports
        assert_eq!(PriorityFeeEstimator::fee_for_cu(100, 400_000), 40);

        // Edge case: 0
        assert_eq!(PriorityFeeEstimator::fee_for_cu(0, 400_000), 0);
    }

    #[test]
    fn test_percentile_selection() {
        let mut fees = vec![100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];
        fees.sort_unstable();

        // 75th percentile of 10 items = index 7 = 800
        let idx = (fees.len() * 75 / 100).min(fees.len() - 1);
        assert_eq!(fees[idx], 800);

        // 50th percentile = index 5 = 600
        let idx50 = (fees.len() * 50 / 100).min(fees.len() - 1);
        assert_eq!(fees[idx50], 600);
    }
}
