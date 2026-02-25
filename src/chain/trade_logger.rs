#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Represents a single executed trade
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutedTrade {
    /// ISO timestamp when trade was executed
    pub timestamp: String,
    /// The token being arbitraged (mint address)
    pub token_mint: String,
    /// DEX where token was bought
    pub buy_dex: String,
    /// DEX where token was sold
    pub sell_dex: String,
    /// Input capital in SOL
    pub input_sol: f64,
    /// Expected profit before execution (SOL)
    pub expected_profit_sol: f64,
    /// Expected profit percentage
    pub expected_profit_pct: f64,
    /// Transaction signature on Solana blockchain
    pub tx_signature: String,
    /// Trade status: "pending", "success", "failed", "reverted"
    pub status: String,
    /// Actual profit after execution (SOL) - filled after execution
    pub actual_profit_sol: Option<f64>,
    /// Additional notes (errors, observations, etc)
    pub notes: String,
}

/// Simple CSV-based trade logger
pub struct TradeLogger {
    log_file: String,
}

impl TradeLogger {
    /// Create a new trade logger. Initializes CSV file with headers if it doesn't exist.
    pub fn new(filename: &str) -> Self {
        // Create file with headers if it doesn't exist
        if !Path::new(filename).exists() {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .open(filename)
                .expect("Failed to create log file");

            let header = "timestamp,token_mint,buy_dex,sell_dex,input_sol,expected_profit_sol,expected_profit_pct,tx_signature,status,actual_profit_sol,notes\n";
            file.write_all(header.as_bytes())
                .expect("Failed to write header");
        }

        TradeLogger {
            log_file: filename.to_string(),
        }
    }

    /// Log a single trade to CSV file
    pub fn log_trade(&self, trade: &ExecutedTrade) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.log_file)?;

        let csv_line = format!(
            "{},{},{},{},{:.6},{:.6},{:.4},{},{},{},{}\n",
            trade.timestamp,
            trade.token_mint,
            trade.buy_dex,
            trade.sell_dex,
            trade.input_sol,
            trade.expected_profit_sol,
            trade.expected_profit_pct,
            trade.tx_signature,
            trade.status,
            trade.actual_profit_sol.unwrap_or(0.0),
            trade.notes.replace(",", ";") // Replace commas with semicolons to avoid CSV issues
        );

        file.write_all(csv_line.as_bytes())?;
        Ok(())
    }

    /// Log a simple message with timestamp
    pub fn log_message(&self, message: &str) -> Result<(), Box<dyn std::error::Error>> {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        println!("[{}] {}", timestamp, message);
        Ok(())
    }

    /// Get file path
    pub fn get_file_path(&self) -> &str {
        &self.log_file
    }

    /// Read all trades from CSV file
    pub fn read_all_trades(&self) -> Result<Vec<ExecutedTrade>, Box<dyn std::error::Error>> {
        use std::fs;
        
        if !Path::new(&self.log_file).exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&self.log_file)?;
        let mut trades = Vec::new();

        for (i, line) in content.lines().enumerate() {
            if i == 0 {
                continue; // Skip header
            }

            let parts: Vec<&str> = line.splitn(11, ',').collect();
            if parts.len() == 11 {
                let trade = ExecutedTrade {
                    timestamp: parts[0].to_string(),
                    token_mint: parts[1].to_string(),
                    buy_dex: parts[2].to_string(),
                    sell_dex: parts[3].to_string(),
                    input_sol: parts[4].parse().unwrap_or(0.0),
                    expected_profit_sol: parts[5].parse().unwrap_or(0.0),
                    expected_profit_pct: parts[6].parse().unwrap_or(0.0),
                    tx_signature: parts[7].to_string(),
                    status: parts[8].to_string(),
                    actual_profit_sol: parts[9].parse().ok().filter(|&v: &f64| v != 0.0),
                    notes: parts[10].replace(";", ",").to_string(),
                };
                trades.push(trade);
            }
        }

        Ok(trades)
    }

    /// Calculate statistics from trade history
    pub fn calculate_stats(&self) -> Result<TradeStats, Box<dyn std::error::Error>> {
        let trades = self.read_all_trades()?;

        let successful = trades
            .iter()
            .filter(|t| t.status == "success")
            .collect::<Vec<_>>();

        let failed = trades
            .iter()
            .filter(|t| t.status == "failed")
            .collect::<Vec<_>>();

        let total_profit: f64 = successful
            .iter()
            .filter_map(|t| t.actual_profit_sol)
            .sum();

        let expected_total_profit: f64 = trades.iter().map(|t| t.expected_profit_sol).sum();

        let avg_profit = if !successful.is_empty() {
            total_profit / successful.len() as f64
        } else {
            0.0
        };

        let success_rate = if !trades.is_empty() {
            (successful.len() as f64 / trades.len() as f64) * 100.0
        } else {
            0.0
        };

        Ok(TradeStats {
            total_trades: trades.len(),
            successful_trades: successful.len(),
            failed_trades: failed.len(),
            pending_trades: trades.iter().filter(|t| t.status == "pending").count(),
            total_profit_sol: total_profit,
            expected_total_profit_sol: expected_total_profit,
            average_profit_sol: avg_profit,
            success_rate,
        })
    }

    /// Get summary statistics as formatted string
    pub fn print_summary(&self) -> Result<(), Box<dyn std::error::Error>> {
        let stats = self.calculate_stats()?;

        println!("\n╔════════════════════════════════════════╗");
        println!("║          TRADE STATISTICS              ║");
        println!("╚════════════════════════════════════════╝");
        println!("  📊 Total Trades:     {}", stats.total_trades);
        println!("  ✅ Successful:       {}", stats.successful_trades);
        println!("  ❌ Failed:           {}", stats.failed_trades);
        println!("  ⏳ Pending:          {}", stats.pending_trades);
        println!("  ");
        println!("  💰 Total Profit:     {:.6} SOL", stats.total_profit_sol);
        println!("  📈 Expected Profit:  {:.6} SOL", stats.expected_total_profit_sol);
        println!("  ⌀ Avg per Trade:     {:.6} SOL", stats.average_profit_sol);
        println!("  ");
        println!("  📈 Success Rate:     {:.1}%", stats.success_rate);
        println!("  📁 Log File:         {}", self.log_file);
        println!();

        Ok(())
    }
}

/// Trade statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeStats {
    pub total_trades: usize,
    pub successful_trades: usize,
    pub failed_trades: usize,
    pub pending_trades: usize,
    pub total_profit_sol: f64,
    pub expected_total_profit_sol: f64,
    pub average_profit_sol: f64,
    pub success_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_and_read_trade() {
        let logger = TradeLogger::new("test_trades.csv");

        let trade = ExecutedTrade {
            timestamp: "2026-01-09T12:00:00Z".to_string(),
            token_mint: "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d".to_string(),
            buy_dex: "raydium".to_string(),
            sell_dex: "pump".to_string(),
            input_sol: 1.0,
            expected_profit_sol: 0.01,
            expected_profit_pct: 1.0,
            tx_signature: "5gfnfnjkfn...".to_string(),
            status: "success".to_string(),
            actual_profit_sol: Some(0.009),
            notes: "Test trade".to_string(),
        };

        assert!(logger.log_trade(&trade).is_ok());

        let trades = logger.read_all_trades().unwrap();
        assert!(!trades.is_empty());

        // Clean up
        let _ = std::fs::remove_file("test_trades.csv");
    }

    #[test]
    fn test_csv_escaping() {
        let logger = TradeLogger::new("test_trades_escape.csv");

        let trade = ExecutedTrade {
            timestamp: "2026-01-09T12:00:00Z".to_string(),
            token_mint: "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d".to_string(),
            buy_dex: "raydium".to_string(),
            sell_dex: "pump".to_string(),
            input_sol: 1.0,
            expected_profit_sol: 0.01,
            expected_profit_pct: 1.0,
            tx_signature: "5gfnfnjkfn...".to_string(),
            status: "success".to_string(),
            actual_profit_sol: Some(0.009),
            notes: "Test, with, commas".to_string(),
        };

        assert!(logger.log_trade(&trade).is_ok());

        let trades = logger.read_all_trades().unwrap();
        assert_eq!(trades[0].notes, "Test, with, commas");

        // Clean up
        let _ = std::fs::remove_file("test_trades_escape.csv");
    }
}
