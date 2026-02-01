mod chain;
mod config;
mod dex;

use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use std::collections::HashMap;
use std::sync::Arc;
use std::str::FromStr;
use anyhow::Result;
use tracing::{info, warn};

use crate::config::BotConfig;
use crate::chain::pools::{MintPoolData, PoolData};
use crate::chain::opportunity_detector::{OpportunityDetector, OpportunityConfig};
use crate::chain::refresh::PoolRefreshManager;
use crate::chain::transaction::TransactionBuilder;
use crate::chain::wallet_integration::WalletConfig;
use crate::chain::constants::SOL_MINT;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("solana_mev_bot=info".parse().unwrap())
        )
        .init();

    info!("Solana MEV Arbitrage Bot starting...");

    // Load config
    let config = BotConfig::from_env()?;
    info!("Config loaded: RPC={}, mints={}, real_execution={}",
        config.rpc_url, config.mints.len(), config.enable_real_execution);

    let rpc = Arc::new(RpcClient::new(config.rpc_url.clone()));

    // Load wallet
    let wallet = if config.wallet_private_key.is_empty() {
        info!("No wallet key configured, using test wallet (demo mode)");
        WalletConfig::test_wallet()
    } else {
        WalletConfig::from_env().unwrap_or_else(|e| {
            warn!("Failed to load wallet from env: {}, using test wallet", e);
            WalletConfig::test_wallet()
        })
    };
    info!("Wallet: {}", wallet.address());

    match rpc.get_balance(&wallet.address()) {
        Ok(balance) => info!("Wallet balance: {:.4} SOL", balance as f64 / 1e9),
        Err(e) => warn!("Could not fetch balance: {}", e),
    }

    let payer = Arc::new(Keypair::new());

    let tx_builder = TransactionBuilder::new(
        rpc.clone(),
        payer,
        config.compute_unit_limit,
        config.priority_fee_lamports,
        config.spam_rpc_urls.clone(),
        config.enable_real_execution,
    );

    let mut refresh_manager = PoolRefreshManager::new(rpc.clone());

    // Load MintPoolData from config
    let mut mint_pool_datas: Vec<MintPoolData> = Vec::new();

    for mint_config in &config.mints {
        let mut mpd = MintPoolData::new(
            &mint_config.mint.to_string(),
            &wallet.address().to_string(),
            spl_token::id(),
        )?;

        for pool_addr in &mint_config.raydium_pools {
            if let Err(e) = mpd.add_raydium_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Raydium pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.pump_pools {
            if let Err(e) = mpd.add_pump_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Pump pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.dlmm_pools {
            if let Err(e) = mpd.add_dlmm_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                vec![],
                None,
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add DLMM pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.whirlpool_pools {
            if let Err(e) = mpd.add_whirlpool_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                vec![],
                None,
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Whirlpool pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.meteora_damm_v2_pools {
            if let Err(e) = mpd.add_meteora_damm_v2_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add DAMM V2 pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.solfi_pools {
            if let Err(e) = mpd.add_solfi_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add SolFi pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.vertigo_pools {
            if let Err(e) = mpd.add_vertigo_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Vertigo pool {}: {}", pool_addr, e);
            }
        }

        info!("Loaded {} pools for mint {}", mpd.pools.len(), mint_config.mint);
        mint_pool_datas.push(mpd);
    }

    let opp_config = OpportunityConfig {
        min_profit_percent: config.min_profit_sol * 100.0,
        min_liquidity_sol: 1.0,
        max_slippage_percent: config.max_slippage_pct,
        max_volatility_percent: 10.0,
    };

    let slippage_bps = (config.max_slippage_pct * 100.0) as u64; // convert % to bps
    let user_token_accounts: HashMap<String, Pubkey> = HashMap::new();

    info!("Starting main trading loop (interval={}ms)", config.loop_interval_ms);

    if !config.enable_real_execution {
        info!("[DEMO MODE] Set ENABLE_REAL_EXECUTION=true to go live");
    }

    let mut interval = tokio::time::interval(
        tokio::time::Duration::from_millis(config.loop_interval_ms)
    );
    let mut loop_count: u64 = 0;
    let mut total_opportunities: u64 = 0;
    let mut total_executions: u64 = 0;

    loop {
        interval.tick().await;
        loop_count += 1;

        // Full refresh: deserialize pool accounts + fetch vault balances
        for mpd in &mint_pool_datas {
            match refresh_manager.refresh_all(mpd) {
                Ok(count) => {
                    if loop_count % 20 == 0 && count > 0 {
                        info!("[Loop {}] Refreshed {} pools for mint {}", loop_count, count, mpd.mint);
                    }
                }
                Err(e) => {
                    if loop_count % 100 == 0 {
                        warn!("Refresh error for mint {}: {}", mpd.mint, e);
                    }
                }
            }
        }

        // Detect opportunities
        for mpd in &mint_pool_datas {
            let mut detector = OpportunityDetector::new(opp_config.clone(), &rpc);
            let opportunities = detector.find_opportunities(mpd);

            if !opportunities.is_empty() {
                total_opportunities += opportunities.len() as u64;
                info!("[Loop {}] Found {} opportunities for mint {}",
                    loop_count, opportunities.len(), mpd.mint);

                for (i, opp) in opportunities.iter().enumerate().take(3) {
                    info!(
                        "  #{}: {} -> {} | profit={:.6} SOL ({:.2}%) | confidence={}",
                        i + 1,
                        opp.path.first().map(|p| p.dex.as_str()).unwrap_or("?"),
                        opp.path.last().map(|p| p.dex.as_str()).unwrap_or("?"),
                        opp.gross_profit_sol,
                        opp.profit_percent,
                        opp.confidence_score,
                    );
                }

                // Execute best opportunity
                if let Some(best) = opportunities.first() {
                    let est_gas_cost = config.priority_fee_lamports as f64 / 1e9;
                    let net_profit = best.gross_profit_sol - est_gas_cost;

                    if net_profit > config.min_profit_sol {
                        info!("Profitable after gas: net={:.6} SOL, building TX...", net_profit);

                        // Build swap instructions from opportunity path
                        match tx_builder.build_instructions_from_opportunity(
                            best,
                            &refresh_manager,
                            &user_token_accounts,
                            slippage_bps,
                        ) {
                            Ok(swap_ixs) => {
                                info!("Built {} swap instructions", swap_ixs.len());

                                match tx_builder.get_recent_blockhash() {
                                    Ok(blockhash) => {
                                        match tx_builder.build_swap_transaction(swap_ixs, blockhash) {
                                            Ok(tx) => {
                                                match tx_builder.simulate(&tx) {
                                                    Ok(true) => {
                                                        info!("Simulation passed, executing...");
                                                        match tx_builder.send_and_confirm(&tx) {
                                                            Ok(sig) => {
                                                                total_executions += 1;
                                                                info!("TX confirmed: {} (total executions: {})", sig, total_executions);
                                                            }
                                                            Err(e) => warn!("TX send failed: {}", e),
                                                        }
                                                    }
                                                    Ok(false) => warn!("Simulation failed, skipping"),
                                                    Err(e) => warn!("Simulation error: {}", e),
                                                }
                                            }
                                            Err(e) => warn!("Failed to build tx: {}", e),
                                        }
                                    }
                                    Err(e) => warn!("Failed to get blockhash: {}", e),
                                }
                            }
                            Err(e) => {
                                warn!("Failed to build swap IXs: {}", e);
                            }
                        }
                    }
                }
            }
        }

        if loop_count % 100 == 0 {
            info!(
                "[Status] Loop={}, Opportunities={}, Executions={}, Mints={}",
                loop_count, total_opportunities, total_executions, mint_pool_datas.len()
            );
        }
    }
}
