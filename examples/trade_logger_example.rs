// Example: Using the TradeLogger

use solana_mev_bot::chain::trade_logger::{TradeLogger, ExecutedTrade};
use chrono::Local;

#[tokio::main]
async fn main() {
    // Initialize the trade logger
    let logger = TradeLogger::new("trades.csv");

    logger.log_message("🤖 MEV Bot Trade Tracking Started").ok();

    // Example: Log an expected trade before execution
    let expected_trade = ExecutedTrade {
        timestamp: Local::now().to_rfc3339(),
        token_mint: "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d".to_string(), // USDC
        buy_dex: "raydium".to_string(),
        sell_dex: "pump".to_string(),
        input_sol: 1.0,
        expected_profit_sol: 0.007,
        expected_profit_pct: 0.7,
        tx_signature: "pending...".to_string(),
        status: "pending".to_string(),
        actual_profit_sol: None,
        notes: "USDC arbitrage - price difference detected".to_string(),
    };

    logger.log_trade(&expected_trade).ok();
    println!("✅ Logged pending trade");

    // Example: Update trade after execution
    let mut executed_trade = expected_trade.clone();
    executed_trade.timestamp = Local::now().to_rfc3339();
    executed_trade.tx_signature = "5gfnfnjkfnxxx123abc...".to_string();
    executed_trade.status = "success".to_string();
    executed_trade.actual_profit_sol = Some(0.0065); // Slightly less than expected due to slippage

    logger.log_trade(&executed_trade).ok();
    println!("✅ Logged executed trade");

    // Example: Log a failed trade
    let failed_trade = ExecutedTrade {
        timestamp: Local::now().to_rfc3339(),
        token_mint: "So11111111111111111111111111111111111111112".to_string(), // Wrapped SOL
        buy_dex: "whirlpool".to_string(),
        sell_dex: "raydium".to_string(),
        input_sol: 0.5,
        expected_profit_sol: 0.003,
        expected_profit_pct: 0.6,
        tx_signature: "5gfnfnjkfnyyy456def...".to_string(),
        status: "failed".to_string(),
        actual_profit_sol: None,
        notes: "Slippage exceeded threshold; reverted".to_string(),
    };

    logger.log_trade(&failed_trade).ok();
    println!("✅ Logged failed trade");

    // Print summary statistics
    logger.print_summary().ok();

    // Read and analyze all trades
    if let Ok(all_trades) = logger.read_all_trades() {
        println!("\n📊 Recent Trades:");
        for (i, trade) in all_trades.iter().enumerate() {
            println!(
                "{:2}. {} → {} | Input: {:.2} SOL | Status: {} | Expected: {:.4} SOL",
                i + 1,
                trade.buy_dex,
                trade.sell_dex,
                trade.input_sol,
                trade.status,
                trade.expected_profit_sol
            );
        }
    }

    logger.log_message("🤖 Trade tracking session completed").ok();
}

/*
Expected output:

🤖 MEV Bot Trade Tracking Started
✅ Logged pending trade
✅ Logged executed trade
✅ Logged failed trade

╔════════════════════════════════════════╗
║          TRADE STATISTICS              ║
╚════════════════════════════════════════╝
  📊 Total Trades:     3
  ✅ Successful:       1
  ❌ Failed:           1
  ⏳ Pending:          1
  
  💰 Total Profit:     0.006500 SOL
  📈 Expected Profit:  0.010000 SOL
  ⌀ Avg per Trade:     0.006500 SOL
  
  📈 Success Rate:     33.3%
  📁 Log File:         trades.csv

📊 Recent Trades:
 1. raydium → pump | Input: 1.00 SOL | Status: success | Expected: 0.0070 SOL
 2. whirlpool → raydium | Input: 0.50 SOL | Status: failed | Expected: 0.0030 SOL
 3. raydium → pump | Input: 1.00 SOL | Status: pending | Expected: 0.0070 SOL

🤖 Trade tracking session completed
*/
