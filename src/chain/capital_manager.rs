use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tracing::info;

/// Tracks capital, profits, and dynamically sizes positions.
/// When auto_compound is enabled, realized profits increase available
/// trading capital automatically.
pub struct CapitalManager {
    rpc: Arc<RpcClient>,
    wallet_address: Pubkey,
    /// Starting balance when bot launched (lamports)
    initial_balance_lamports: u64,
    /// Current known balance (lamports)
    current_balance_lamports: u64,
    /// Cumulative realized profit (lamports, can be negative)
    cumulative_profit_lamports: i64,
    winning_trades: u64,
    losing_trades: u64,
    /// Maximum fraction of capital to risk per trade (0.0 - 1.0)
    max_risk_per_trade: f64,
    /// Minimum SOL to keep in wallet (gas, rent)
    reserve_lamports: u64,
    /// Whether profits are reinvested into trading capital
    auto_compound: bool,
    /// High-water mark for drawdown tracking
    high_water_mark: u64,
}

impl CapitalManager {
    pub fn new(
        rpc: Arc<RpcClient>,
        wallet_address: Pubkey,
    ) -> Self {
        Self {
            rpc,
            wallet_address,
            initial_balance_lamports: 0,
            current_balance_lamports: 0,
            cumulative_profit_lamports: 0,
            winning_trades: 0,
            losing_trades: 0,
            max_risk_per_trade: 0.10,
            reserve_lamports: 50_000_000, // 0.05 SOL
            auto_compound: true,
            high_water_mark: 0,
        }
    }

    pub fn with_risk_params(
        mut self,
        max_risk_per_trade: f64,
        reserve_sol: f64,
        auto_compound: bool,
    ) -> Self {
        self.max_risk_per_trade = max_risk_per_trade.clamp(0.01, 0.50);
        self.reserve_lamports = (reserve_sol * 1e9) as u64;
        self.auto_compound = auto_compound;
        self
    }

    /// Read current wallet balance and set initial values.
    pub fn initialize(&mut self) -> Result<()> {
        let balance = self.rpc.get_balance(&self.wallet_address)?;
        self.initial_balance_lamports = balance;
        self.current_balance_lamports = balance;
        self.high_water_mark = balance;
        info!(
            "CapitalManager initialized: balance={:.4} SOL, reserve={:.4} SOL, max_risk={:.1}%, auto_compound={}",
            balance as f64 / 1e9,
            self.reserve_lamports as f64 / 1e9,
            self.max_risk_per_trade * 100.0,
            self.auto_compound,
        );
        Ok(())
    }

    /// Refresh balance from chain.
    pub fn refresh_balance(&mut self) -> Result<u64> {
        let balance = self.rpc.get_balance(&self.wallet_address)?;
        self.current_balance_lamports = balance;
        if balance > self.high_water_mark {
            self.high_water_mark = balance;
        }
        Ok(balance)
    }

    /// Available capital = balance minus reserve. Profits are automatically
    /// included when auto_compound is enabled because the on-chain balance
    /// already reflects them.
    pub fn available_capital_lamports(&self) -> u64 {
        self.current_balance_lamports.saturating_sub(self.reserve_lamports)
    }

    /// Max position for a single trade, scaled by confidence.
    pub fn max_position_lamports(&self, confidence_score: u8) -> u64 {
        let available = self.available_capital_lamports();
        if available == 0 {
            return 0;
        }
        let confidence_factor = (confidence_score as f64 / 100.0).clamp(0.1, 1.0);
        let position = available as f64 * self.max_risk_per_trade * confidence_factor;
        position as u64
    }

    /// Size a position: min(suggested, max allowed).
    pub fn size_position(
        &self,
        suggested_input_lamports: u64,
        confidence_score: u8,
    ) -> u64 {
        let max_pos = self.max_position_lamports(confidence_score);
        if max_pos == 0 {
            return 0;
        }
        suggested_input_lamports.min(max_pos)
    }

    /// Record a completed trade and update profit tracking.
    pub fn record_trade(&mut self, input_lamports: u64, output_lamports: u64, success: bool) {
        if success {
            let profit = output_lamports as i64 - input_lamports as i64;
            self.cumulative_profit_lamports += profit;
            if profit >= 0 {
                self.winning_trades += 1;
            } else {
                self.losing_trades += 1;
            }
            // Refresh balance so compounded profits are available next trade
            if self.auto_compound {
                let _ = self.refresh_balance();
            }
        } else {
            self.losing_trades += 1;
        }
    }

    /// Drawdown from high-water mark as a percentage.
    pub fn drawdown_percent(&self) -> f64 {
        if self.high_water_mark == 0 {
            return 0.0;
        }
        let dd = self.high_water_mark.saturating_sub(self.current_balance_lamports);
        (dd as f64 / self.high_water_mark as f64) * 100.0
    }

    /// Whether drawdown exceeds threshold.
    pub fn should_pause_trading(&self, max_drawdown_pct: f64) -> bool {
        self.drawdown_percent() > max_drawdown_pct
    }

    pub fn win_rate(&self) -> f64 {
        let total = self.winning_trades + self.losing_trades;
        if total == 0 {
            return 0.0;
        }
        self.winning_trades as f64 / total as f64
    }

    /// Adjust risk per trade based on recent performance.
    pub fn adapt_risk(&mut self) {
        let total = self.winning_trades + self.losing_trades;
        if total < 10 {
            return;
        }
        let wr = self.win_rate();
        let base: f64 = 0.10;
        self.max_risk_per_trade = if wr > 0.70 {
            (base * 1.5).min(0.20)
        } else if wr > 0.55 {
            base
        } else if wr > 0.40 {
            base * 0.75
        } else {
            base * 0.50
        };
    }

    pub fn print_summary(&self) {
        let total = self.winning_trades + self.losing_trades;
        info!(
            "[Capital] balance={:.4} SOL | available={:.4} SOL | profit={:.4} SOL | trades={} (W:{} L:{}) | win_rate={:.1}% | drawdown={:.1}% | risk={:.1}%",
            self.current_balance_lamports as f64 / 1e9,
            self.available_capital_lamports() as f64 / 1e9,
            self.cumulative_profit_lamports as f64 / 1e9,
            total,
            self.winning_trades,
            self.losing_trades,
            self.win_rate() * 100.0,
            self.drawdown_percent(),
            self.max_risk_per_trade * 100.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager() -> CapitalManager {
        let rpc = Arc::new(RpcClient::new(
            "https://api.mainnet-beta.solana.com".to_string(),
        ));
        let mut mgr = CapitalManager::new(rpc, Pubkey::new_unique());
        mgr.current_balance_lamports = 5_000_000_000; // 5 SOL
        mgr.initial_balance_lamports = 5_000_000_000;
        mgr.high_water_mark = 5_000_000_000;
        mgr
    }

    #[test]
    fn test_available_capital() {
        let mgr = make_manager();
        assert_eq!(mgr.available_capital_lamports(), 4_950_000_000);
    }

    #[test]
    fn test_max_position_full_confidence() {
        let mgr = make_manager();
        let pos = mgr.max_position_lamports(100);
        assert_eq!(pos, 495_000_000);
    }

    #[test]
    fn test_max_position_low_confidence() {
        let mgr = make_manager();
        let pos = mgr.max_position_lamports(50);
        assert_eq!(pos, 247_500_000);
    }

    #[test]
    fn test_size_position_capped() {
        let mgr = make_manager();
        let sized = mgr.size_position(10_000_000_000, 100);
        assert_eq!(sized, 495_000_000);
    }

    #[test]
    fn test_record_winning_trade() {
        let mut mgr = make_manager();
        mgr.record_trade(1_000_000_000, 1_050_000_000, true);
        assert_eq!(mgr.winning_trades, 1);
        assert_eq!(mgr.cumulative_profit_lamports, 50_000_000);
    }

    #[test]
    fn test_drawdown() {
        let mut mgr = make_manager();
        mgr.high_water_mark = 5_000_000_000;
        mgr.current_balance_lamports = 4_500_000_000;
        assert!((mgr.drawdown_percent() - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_adapt_risk_high_winrate() {
        let mut mgr = make_manager();
        mgr.winning_trades = 8;
        mgr.losing_trades = 2;
        mgr.adapt_risk();
        assert!(mgr.max_risk_per_trade > 0.10);
    }

    #[test]
    fn test_adapt_risk_low_winrate() {
        let mut mgr = make_manager();
        mgr.winning_trades = 3;
        mgr.losing_trades = 7;
        mgr.adapt_risk();
        assert!(mgr.max_risk_per_trade < 0.10);
    }

    #[test]
    fn test_insufficient_balance() {
        let mut mgr = make_manager();
        mgr.current_balance_lamports = 10_000_000;
        assert_eq!(mgr.available_capital_lamports(), 0);
        assert_eq!(mgr.max_position_lamports(100), 0);
    }
}
