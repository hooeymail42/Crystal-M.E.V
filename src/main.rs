mod ai;
mod chain;
mod config;
mod dex;

use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::str::FromStr;
use anyhow::{Result, bail};
use tracing::{info, warn, error};
use spl_associated_token_account::get_associated_token_address;

use crate::ai::{TradeMemory, TradeRecord, OpportunityScorer, ScoringFeatures, MarketAnalyzer, AdaptiveParams};
use chrono::Timelike;
use crate::config::BotConfig;
use crate::chain::pools::{MintPoolData, PoolData};
use crate::chain::opportunity_detector::OpportunityDetector;
use crate::chain::refresh::PoolRefreshManager;
use crate::chain::transaction::TransactionBuilder;
use crate::chain::wallet_integration::WalletConfig;
use crate::chain::constants::SOL_MINT;
use crate::chain::trade_logger::{TradeLogger, ExecutedTrade};
use crate::chain::gas_fee::GasFeeConfig;
use crate::chain::volume_weighted_slippage::VolumeWeightedSlippagePredictor;
use crate::chain::pool_subscription::{WebSocketPoolSubscriber, RawAccountUpdate, start_websocket_subscriber};
use crate::chain::capital_manager::CapitalManager;
use crate::chain::pool_discovery::{PoolDiscovery, PoolDiscoveryConfig};
use crate::chain::yellowstone_stream::{YellowstoneConfig, start_yellowstone_stream, add_pool_subscriptions};
use crate::chain::backrun::{BackrunConfig, BackrunDetector, BackrunBundleBuilder};
use crate::dex::lst::{LstConfig, LstArbitrageScanner};

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
    let wallet_address = wallet.address();
    info!("Wallet: {}", wallet_address);

    match rpc.get_balance(&wallet_address) {
        Ok(balance) => info!("Wallet balance: {:.4} SOL", balance as f64 / 1e9),
        Err(e) => warn!("Could not fetch balance: {}", e),
    }

    let payer = Arc::new(wallet.keypair);

    // Load Address Lookup Tables
    let alt_accounts = if !config.alt_addresses.is_empty() {
        let alt_keys: Vec<Pubkey> = config.alt_addresses.iter()
            .filter_map(|s| Pubkey::from_str(s).ok())
            .collect();
        TransactionBuilder::load_lookup_tables(&rpc, &alt_keys)
    } else {
        vec![]
    };
    info!("Loaded {} Address Lookup Tables", alt_accounts.len());

    let mut tx_builder = TransactionBuilder::new(
        rpc.clone(),
        payer,
        config.compute_unit_limit,
        config.priority_fee_lamports,
        config.spam_rpc_urls.clone(),
        config.enable_real_execution,
    )
    .with_jito(
        config.jito_enabled,
        config.jito_tip_lamports,
        config.jito_block_engine_url.clone(),
    )
    .with_dynamic_cu(config.dynamic_cu_enabled, config.cu_buffer_pct)
    .with_dynamic_fee(config.dynamic_fee_enabled, config.fee_percentile)
    .with_lookup_tables(alt_accounts);

    if config.jito_enabled {
        info!("Jito MEV bundles enabled: tip={} lamports, engine={}",
            config.jito_tip_lamports, config.jito_block_engine_url);
    }

    if config.dynamic_cu_enabled {
        info!("Dynamic CU estimation enabled: buffer={:.0}%", config.cu_buffer_pct * 100.0);
    }

    if config.dynamic_fee_enabled {
        info!("Dynamic priority fees enabled: percentile=p{}", config.fee_percentile);
    }

    // Initialize capital manager for auto-compounding and dynamic position sizing
    let mut capital_manager = CapitalManager::new(
        rpc.clone(),
        wallet_address,
    ).with_risk_params(0.10, 0.05, true); // 10% risk per trade, 0.05 SOL reserve, auto-compound on

    if let Err(e) = capital_manager.initialize() {
        warn!("CapitalManager init failed (will retry): {}", e);
    } else {
        capital_manager.print_summary();
    }

    // ── AI / ML engine ────────────────────────────────────────────────────────
    // Ensure data/ directory exists for persistence files
    let _ = std::fs::create_dir_all("data");
    let mut trade_memory = TradeMemory::load();
    let mut scorer = OpportunityScorer::load();
    let mut market_analyzer = MarketAnalyzer::new();
    let mut adaptive_params = AdaptiveParams::load();
    info!("[AI] Engine loaded: trade_records={} scorer_updates={} adaptation_cycles={}",
        trade_memory.records.len(), scorer.total_updates, adaptive_params.adaptation_count);

    let mut refresh_manager = PoolRefreshManager::new(rpc.clone());

    // Load MintPoolData from config
    let mut mint_pool_datas: Vec<MintPoolData> = Vec::new();

    for mint_config in &config.mints {
        let mut mpd = MintPoolData::new(
            &mint_config.mint.to_string(),
            &wallet_address.to_string(),
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

        for pool_addr in &mint_config.raydium_cp_pools {
            if let Err(e) = mpd.add_raydium_cp_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Raydium CP pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.raydium_clmm_pools {
            if let Err(e) = mpd.add_raydium_clmm_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),  // amm_config
                &Pubkey::new_unique().to_string(),  // observation_state
                &Pubkey::new_unique().to_string(),  // x_vault
                &Pubkey::new_unique().to_string(),  // y_vault
                vec![],                              // tick_arrays
                None,                                // memo_program
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Raydium CLMM pool {}: {}", pool_addr, e);
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

        for pool_addr in &mint_config.phoenix_pools {
            if let Err(e) = mpd.add_phoenix_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Phoenix pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.lifinity_pools {
            if let Err(e) = mpd.add_lifinity_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Lifinity pool {}: {}", pool_addr, e);
            }
        }

        for pool_addr in &mint_config.heaven_pools {
            if let Err(e) = mpd.add_heaven_pool(
                pool_addr,
                &Pubkey::new_unique().to_string(),
                &Pubkey::new_unique().to_string(),
                &mint_config.mint.to_string(),
                SOL_MINT,
            ) {
                warn!("Failed to add Heaven pool {}: {}", pool_addr, e);
            }
        }

        info!("Loaded {} pools for mint {}", mpd.pools.len(), mint_config.mint);
        mint_pool_datas.push(mpd);
    }

    // ── Phase 1: Dynamic Pool Discovery ──────────────────────────────────────
    // Discover pools on-chain via getProgramAccounts; merge with hardcoded .env pools.
    let discovery_config = PoolDiscoveryConfig::from_env();
    let mut pool_discovery = PoolDiscovery::new(rpc.clone(), discovery_config);

    if pool_discovery.config.enabled {
        info!("[PoolDiscovery] Starting initial pool discovery (this may take 10-30s)...");
        match pool_discovery.discover_all_pools() {
            Ok(cache) => {
                info!("[PoolDiscovery] Initial discovery complete: {} pools across {} tokens",
                    cache.total_pools(), cache.pools_by_token.len());
                pool_discovery.log_summary();

                // Merge discovered pools into MintPoolData for each configured mint
                for mpd in &mut mint_pool_datas {
                    let mint_str = mpd.mint.to_string();
                    let discovered = pool_discovery.get_pools_for_token(&mint_str);
                    let mut added = 0usize;
                    for dp in discovered {
                        // Skip duplicates already loaded from .env
                        let already_loaded = mpd.pools.iter().any(|p| p.pool_address() == &dp.address);
                        if already_loaded {
                            continue;
                        }
                        let added_ok = match dp.dex.as_str() {
                            "raydium_v4" => mpd.add_raydium_pool(
                                &dp.address.to_string(),
                                &dp.token_vault.to_string(),
                                &dp.sol_vault.to_string(),
                                &dp.token_mint.to_string(),
                                SOL_MINT,
                            ).is_ok(),
                            "raydium_cp" => mpd.add_raydium_cp_pool(
                                &dp.address.to_string(),
                                &Pubkey::new_unique().to_string(),
                                &Pubkey::new_unique().to_string(),
                                &dp.token_vault.to_string(),
                                &dp.sol_vault.to_string(),
                                &dp.token_mint.to_string(),
                                SOL_MINT,
                            ).is_ok(),
                            "whirlpool" => mpd.add_whirlpool_pool(
                                &dp.address.to_string(),
                                &Pubkey::new_unique().to_string(),
                                &dp.token_vault.to_string(),
                                &dp.sol_vault.to_string(),
                                vec![],
                                None,
                                &dp.token_mint.to_string(),
                                SOL_MINT,
                            ).is_ok(),
                            "meteora_dlmm" => mpd.add_dlmm_pool(
                                &dp.address.to_string(),
                                &dp.token_vault.to_string(),
                                &dp.sol_vault.to_string(),
                                &Pubkey::new_unique().to_string(),
                                vec![],
                                None,
                                &dp.token_mint.to_string(),
                                SOL_MINT,
                            ).is_ok(),
                            "pump" => mpd.add_pump_pool(
                                &dp.address.to_string(),
                                &Pubkey::new_unique().to_string(),
                                &dp.token_vault.to_string(),
                                &dp.sol_vault.to_string(),
                                &Pubkey::new_unique().to_string(),
                                &Pubkey::new_unique().to_string(),
                                &dp.token_mint.to_string(),
                                SOL_MINT,
                            ).is_ok(),
                            _ => false,
                        };
                        if added_ok {
                            added += 1;
                        }
                    }
                    if added > 0 {
                        info!("[PoolDiscovery] Added {} discovered pools for mint {}", added, mint_str);
                    }
                }
            }
            Err(e) => {
                warn!("[PoolDiscovery] Initial discovery failed (falling back to .env pools): {}", e);
            }
        }
    } else {
        info!("[PoolDiscovery] Disabled via POOL_DISCOVERY_ENABLED=false");
    }

    // ── Background pool re-discovery task ────────────────────────────────────
    // Re-runs discovery every refresh_interval_minutes to pick up new pools.
    // Uses a separate RPC clone so it doesn't block the main trading loop.
    {
        let bg_rpc = rpc.clone();
        let bg_config = PoolDiscoveryConfig::from_env();
        let refresh_secs = bg_config.refresh_interval_minutes * 60;
        if bg_config.enabled {
            tokio::spawn(async move {
                let mut bg_discovery = PoolDiscovery::new(bg_rpc, bg_config);
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(refresh_secs)).await;
                    info!("[PoolDiscovery BG] Refreshing pool list...");
                    match bg_discovery.discover_all_pools() {
                        Ok(cache) => {
                            info!("[PoolDiscovery BG] Refresh complete: {} pools", cache.total_pools());
                        }
                        Err(e) => {
                            warn!("[PoolDiscovery BG] Refresh failed: {}", e);
                        }
                    }
                }
            });
        }
    }

    // Initialize trade logger
    let trade_logger = TradeLogger::new("trades.csv");
    info!("Trade logger initialized: {}", trade_logger.get_file_path());

    // Initialize gas fee config
    let gas_fee_config = GasFeeConfig::default();
    info!("Gas fee config: min_profit={:.4} SOL, aggressive={}", gas_fee_config.min_profit_to_execute_sol, gas_fee_config.aggressive_mode);

    // Initialize volume-weighted slippage predictor
    let slippage_predictor = VolumeWeightedSlippagePredictor::new("Raydium".to_string());

    // Base config — adaptive_params will tune these each cycle
    adaptive_params.base_min_profit_pct = config.min_profit_sol * 100.0;
    adaptive_params.base_max_trade_sol = 5.0;

    let slippage_bps = (config.max_slippage_pct * 100.0) as u64; // convert % to bps

    // Pre-create ATAs for all token mints
    let mut user_token_accounts: HashMap<String, Pubkey> = HashMap::new();
    let sol_mint_pubkey = Pubkey::from_str(SOL_MINT)?;
    let wsol_ata = get_associated_token_address(&wallet_address, &sol_mint_pubkey);
    user_token_accounts.insert(SOL_MINT.to_string(), wsol_ata);
    info!("WSOL ATA: {}", wsol_ata);

    for mpd in &mint_pool_datas {
        let ata = get_associated_token_address(&wallet_address, &mpd.mint);
        user_token_accounts.insert(mpd.mint.to_string(), ata);
        info!("ATA for mint {}: {}", mpd.mint, ata);
    }
    info!("Pre-derived {} user token accounts", user_token_accounts.len());

    // Start WebSocket pool subscriber if WS URL is configured
    let mut ws_update_rx: Option<tokio::sync::mpsc::UnboundedReceiver<RawAccountUpdate>> = None;
    if !config.ws_url.is_empty() {
        let mut ws_subscriber = WebSocketPoolSubscriber::new(config.ws_url.clone());

        // Register all pool addresses with their DEX names
        for mpd in &mint_pool_datas {
            for pool in &mpd.pools {
                ws_subscriber.add_pool(*pool.pool_address(), pool.get_dex_name().to_string());
            }
        }

        let pool_count = ws_subscriber.pool_count();
        if pool_count > 0 {
            let rx = start_websocket_subscriber(ws_subscriber);
            ws_update_rx = Some(rx);
            info!("WebSocket subscriber started for {} pools (ws_url={})", pool_count, config.ws_url);
        } else {
            info!("No pools to subscribe via WebSocket");
        }
    } else {
        info!("WS_URL not configured, WebSocket subscriptions disabled");
    }

    // ── Phase 2: Yellowstone gRPC / WebSocket Streaming ─────────────────────
    // Start Yellowstone account streaming for lower-latency pool updates.
    // When YELLOWSTONE_WS_URL is set, real-time updates replace polling for
    // all subscribed DEX accounts, cutting latency from 300-500ms to 50-100ms.
    let mut yellowstone_rx = {
        let mut ys_config = YellowstoneConfig::from_env();
        // Add all pool addresses from the loaded mint data so we get per-pool
        // updates even when the WS doesn't support programSubscribe.
        let all_pool_pubkeys: Vec<Pubkey> = mint_pool_datas.iter()
            .flat_map(|mpd| mpd.pools.iter().map(|p| *p.pool_address()))
            .collect();
        add_pool_subscriptions(&mut ys_config, &all_pool_pubkeys);
        info!("[Yellowstone] Registered {} pool addresses for streaming", all_pool_pubkeys.len());
        start_yellowstone_stream(ys_config)
    };

    // ── Phase 4: LST Arbitrage Scanner ───────────────────────────────────────
    // Monitors mSOL/JitoSOL/bSOL spreads between DEX prices and protocol rates.
    // Disabled by default — enable with LST_ARB_ENABLED=true in .env.
    let lst_config = LstConfig::from_env();
    let mut lst_scanner = LstArbitrageScanner::new(rpc.clone(), lst_config.clone());
    if lst_config.enabled {
        info!("[LST] LST arbitrage scanning enabled (min_spread={:.2}%)", lst_config.min_spread_pct);
        // Initial state fetch
        lst_scanner.refresh_state();
    } else {
        info!("[LST] LST arbitrage scanning disabled (set LST_ARB_ENABLED=true to enable)");
    }

    // ── Phase 5: Jito Backrun Detector ───────────────────────────────────────
    // Detects large DEX swaps from account update streams and logs backrun opportunities.
    // Disabled by default — enable with BACKRUN_ENABLED=true in .env.
    let backrun_config = BackrunConfig::from_env();
    let mut backrun_detector = BackrunDetector::new(backrun_config.clone());
    let backrun_bundle_builder = BackrunBundleBuilder::new(
        backrun_config.enabled && config.enable_real_execution,
        backrun_config.jito_tip_lamports,
    );
    if backrun_config.enabled {
        info!(
            "[Backrun] Backrun detection enabled (min_swap={:.1} SOL, max_pos={:.1} SOL)",
            backrun_config.min_swap_sol, backrun_config.max_position_sol
        );
    } else {
        info!("[Backrun] Backrun detection disabled (set BACKRUN_ENABLED=true to enable)");
    }

    let spam_enabled = config.spam_enabled;
    let has_spam_rpcs = !config.spam_rpc_urls.is_empty();

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
    let mut consecutive_failures: u32 = 0;
    let mut circuit_breaker_until: Option<tokio::time::Instant> = None;
    let mut opp_config = adaptive_params.to_opportunity_config();
    // Track loop timing for congestion detection
    let mut last_loop_start = tokio::time::Instant::now();

    loop {
        interval.tick().await;
        loop_count += 1;
        let loop_ms = last_loop_start.elapsed().as_millis() as f64;
        market_analyzer.record_loop_duration_ms(loop_ms);
        last_loop_start = tokio::time::Instant::now();

        // Circuit breaker: if paused, skip this iteration
        if let Some(resume_at) = circuit_breaker_until {
            if tokio::time::Instant::now() < resume_at {
                if loop_count % 10 == 0 {
                    warn!("[Circuit Breaker] Trading paused due to {} consecutive failures, waiting...", consecutive_failures);
                }
                continue;
            } else {
                info!("[Circuit Breaker] Pause period ended, resuming trading");
                circuit_breaker_until = None;
            }
        }

        // Drawdown protection: pause if down >20% from high-water mark
        if capital_manager.should_pause_trading(20.0) {
            if loop_count % 50 == 0 {
                warn!("[Drawdown Protection] Paused: {:.1}% drawdown from peak", capital_manager.drawdown_percent());
            }
            continue;
        }

        // Refresh dynamic priority fee periodically (every 20 loops ~10s at 500ms interval)
        if config.dynamic_fee_enabled && loop_count % 20 == 0 {
            tx_builder.refresh_priority_fee(&[]);
        }

        // Periodic balance monitoring (every 50 loops)
        if loop_count % 50 == 0 {
            match rpc.get_balance(&wallet_address) {
                Ok(balance) => {
                    let sol_balance = balance as f64 / 1e9;
                    info!("[Loop {}] Wallet balance: {:.4} SOL", loop_count, sol_balance);
                    if sol_balance < 0.01 && config.enable_real_execution {
                        warn!("[Loop {}] Balance too low ({:.4} SOL < 0.01 SOL), skipping trading this iteration", loop_count, sol_balance);
                        continue;
                    }
                }
                Err(e) => {
                    warn!("[Loop {}] Could not fetch balance: {}", loop_count, e);
                }
            }
        }

        // Drain pending WebSocket updates and apply to refresh manager
        if let Some(ref mut rx) = ws_update_rx {
            let mut ws_updates_applied = 0u32;
            while let Ok(update) = rx.try_recv() {
                refresh_manager.apply_account_update(
                    update.pool_address,
                    &update.dex_name,
                    &update.data,
                );
                ws_updates_applied += 1;
            }
            if ws_updates_applied > 0 && loop_count % 20 == 0 {
                info!("[Loop {}] Applied {} WebSocket pool updates", loop_count, ws_updates_applied);
            }
        }

        // ── Phase 2: Drain Yellowstone gRPC/WS account updates ───────────────
        // Yellowstone delivers account data with lower latency than HTTP polling.
        // Route updates through the PoolRefreshManager exactly like WS updates.
        if let Some(ref mut ys_rx) = yellowstone_rx {
            let mut ys_updates = 0u32;
            while let Ok(update) = ys_rx.try_recv() {
                // Determine DEX name from known program IDs
                let dex_name = if let Some(owner) = &update.owner {
                    match owner.to_string().as_str() {
                        "675kPX9MHTjS2zt1qfr1NYHuzeLXFQM5p84CmjZrtsm" => "Raydium",
                        "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" => "RaydiumCp",
                        "whirLbMiicVdio4KfQ7QV1mKpQ2dB6A8mEy93gVe5t"   => "Whirlpool",
                        "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"  => "DLMM",
                        "6EF8rQNwhS2q7s7D3F7p4CevG5vQTGSwbDVefyxE7tE"  => "Pump",
                        _ => "Unknown",
                    }
                } else {
                    "Unknown"
                };
                refresh_manager.apply_account_update(
                    update.pubkey,
                    dex_name,
                    &update.data,
                );

                // ── Phase 5: Backrun detection on every account update ────────
                if let Some(opp) = backrun_detector.process_update(&update, &refresh_manager) {
                    backrun_bundle_builder.handle_opportunity(&opp);
                }

                ys_updates += 1;
            }
            if ys_updates > 0 && loop_count % 20 == 0 {
                info!("[Loop {}] Applied {} Yellowstone account updates", loop_count, ys_updates);
            }
        }

        // ── Phase 4: LST state refresh (every 100 loops ~50s at 500ms interval) ──
        if lst_config.enabled && loop_count % 100 == 0 {
            lst_scanner.refresh_state();
            // Run LST spread scan with empty pool rates for now.
            // When SOL/mSOL pools are configured, pass live quotes here.
            let lst_opps = lst_scanner.scan(&[], &[]);
            if !lst_opps.is_empty() {
                info!("[LST] {} spread opportunities detected this cycle", lst_opps.len());
                for opp in lst_opps.iter().take(3) {
                    info!("[LST]  {} — spread={:.3}% est_profit_10sol={:.4}",
                        opp.description, opp.spread_pct, opp.estimated_profit_sol_10);
                }
            }
        }

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
        info!("[Loop {}] Scanning {} mints / {} pools | failures={} | executions={}",
            loop_count,
            mint_pool_datas.len(),
            mint_pool_datas.iter().map(|m| m.pools.len()).sum::<usize>(),
            consecutive_failures,
            total_executions,
        );

        for mpd in &mint_pool_datas {
            let mut detector = OpportunityDetector::new(opp_config.clone(), &rpc);
            let opportunities = detector.find_opportunities(mpd, &refresh_manager);

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

                // Record best price ratio for market analysis
                if let Some(best_opp) = opportunities.first() {
                    let ratio = best_opp.expected_output_sol / best_opp.input_amount_sol.max(1e-9);
                    market_analyzer.record_price_ratio(ratio);
                }

                // Execute best opportunity
                if let Some(best) = opportunities.first() {
                    let num_hops = best.path.len();
                    let buy_dex = best.path.first().map(|p| p.dex.as_str()).unwrap_or("unknown");
                    let sell_dex = best.path.last().map(|p| p.dex.as_str()).unwrap_or("unknown");

                    // ── AI scoring filter ─────────────────────────────────────
                    let sol_reserve_est = (best.input_amount_sol * 50.0).min(500.0); // rough estimate
                    let features = ScoringFeatures {
                        profit_pct_norm: (best.profit_percent / 20.0).min(1.0),
                        liquidity_norm: (sol_reserve_est / 500.0).min(1.0),
                        dex_a_rep: ScoringFeatures::dex_reputation(buy_dex),
                        dex_b_rep: ScoringFeatures::dex_reputation(sell_dex),
                        pair_history: trade_memory.pair_win_rate(buy_dex, sell_dex),
                        hour_score: {
                            let h = chrono::Utc::now().hour() as u8;
                            trade_memory.hour_win_rate(h)
                        },
                        congestion_score: market_analyzer.congestion_score(),
                    };
                    let (passes, ai_score) = scorer.passes(&features);
                    if !passes && scorer.total_updates > 30 {
                        info!("[AI] Opportunity rejected by scorer (score={:.3}, threshold={:.3}): {} -> {} profit={:.2}%",
                            ai_score, scorer.threshold, buy_dex, sell_dex, best.profit_percent);
                        continue;
                    }
                    info!("[AI] Opportunity accepted: score={:.3} {} -> {} profit={:.2}%",
                        ai_score, buy_dex, sell_dex, best.profit_percent);
                    // Save features for post-trade update
                    let saved_features = features.clone();

                    // Use gas fee config for cost estimation
                    let mut est_gas_cost = gas_fee_config.estimate_cost_sol(buy_dex, num_hops);
                    if config.jito_enabled {
                        est_gas_cost += config.jito_tip_lamports as f64 / 1e9;
                    }
                    let net_profit = best.gross_profit_sol - est_gas_cost;

                    // Dynamic position sizing via capital manager
                    let sized_input = capital_manager.size_position(
                        (best.input_amount_sol * 1e9) as u64,
                        best.confidence_score,
                    );
                    if sized_input == 0 {
                        warn!("Insufficient capital for trade, skipping");
                        continue;
                    }
                    let sized_input_sol = sized_input as f64 / 1e9;

                    // Use gas fee config's should_execute check
                    if gas_fee_config.should_execute(net_profit, num_hops) {
                        // Apply slippage prediction filter if volume data is available
                        let mut skip_due_to_slippage = false;
                        if !best.pool_addresses.is_empty() {
                            if let Ok(pool_pubkey) = Pubkey::from_str(&best.pool_addresses[0]) {
                                if let Ok(predicted_slippage) = slippage_predictor.predict_slippage(
                                    pool_pubkey,
                                    best.input_amount_sol,
                                    best.expected_output_sol,
                                ) {
                                    if predicted_slippage > opp_config.max_slippage_percent {
                                        info!("Skipping: predicted slippage {:.2}% > max {:.2}%",
                                            predicted_slippage, opp_config.max_slippage_percent);
                                        skip_due_to_slippage = true;
                                    }
                                }
                                // If predict_slippage fails (no volume history), proceed without filter
                            }
                        }

                        if skip_due_to_slippage {
                            continue;
                        }

                        info!("Profitable after gas: net={:.6} SOL (gas={:.6} SOL), building TX...", net_profit, est_gas_cost);

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
                                        // Build transaction: flash loan > Jito bundle > regular
                                        let tx_result = if config.flashloan_enabled
                                            && !config.flashloan_reserve.is_empty()
                                            && !config.flashloan_reserve_vault.is_empty()
                                            && !config.flashloan_fee_receiver.is_empty()
                                        {
                                            let fl_reserve = Pubkey::from_str(&config.flashloan_reserve).unwrap();
                                            let fl_vault = Pubkey::from_str(&config.flashloan_reserve_vault).unwrap();
                                            let fl_fee = Pubkey::from_str(&config.flashloan_fee_receiver).unwrap();
                                            // Optimize flash loan borrow amount
                                            let (borrow_lamports, _est_fl_profit) = tx_builder
                                                .calculate_optimal_flashloan(
                                                    sized_input_sol,
                                                    best.profit_percent,
                                                    capital_manager.available_capital_lamports(),
                                                )
                                                .unwrap_or(((sized_input_sol * 1e9) as u64, 0));
                                            info!("Using Kamino flash loan: borrow={:.4} SOL (optimized)", borrow_lamports as f64 / 1e9);
                                            tx_builder.build_flashloan_transaction(
                                                swap_ixs, borrow_lamports, &fl_reserve, &fl_vault, &fl_fee, blockhash,
                                            )
                                        } else if tx_builder.is_jito_enabled() {
                                            tx_builder.build_jito_bundle(swap_ixs, blockhash)
                                        } else {
                                            tx_builder.build_swap_transaction(swap_ixs, blockhash)
                                        };
                                        match tx_result {
                                            Ok(tx) => {
                                                match tx_builder.simulate(&tx) {
                                                    Ok(true) => {
                                                        info!("Simulation passed, executing...");
                                                        // Submit via Jito bundle, parallel spam, or standard RPC
                                                let send_result = if tx_builder.is_jito_enabled() {
                                                    tx_builder.submit_jito_bundle(&tx)
                                                } else if spam_enabled && has_spam_rpcs {
                                                    tx_builder.parallel_submit(&tx).await
                                                } else {
                                                    tx_builder.send_and_confirm(&tx)
                                                };
                                                match send_result {
                                                            Ok(sig) => {
                                                                total_executions += 1;
                                                                consecutive_failures = 0;
                                                                let profitable = best.expected_output_sol > best.input_amount_sol;
                                                                capital_manager.record_trade(
                                                                    (best.input_amount_sol * 1e9) as u64,
                                                                    (best.expected_output_sol * 1e9) as u64,
                                                                    true,
                                                                );
                                                                // ── AI learning: success ─────
                                                                scorer.update(&saved_features, profitable, best.profit_percent);
                                                                let h = chrono::Utc::now().hour() as u8;
                                                                trade_memory.record(TradeRecord {
                                                                    timestamp_utc: chrono::Utc::now().timestamp(),
                                                                    hour_utc: h,
                                                                    dex_a: buy_dex.to_string(),
                                                                    dex_b: sell_dex.to_string(),
                                                                    token_mint: best.token_mint.clone(),
                                                                    input_sol: best.input_amount_sol,
                                                                    output_sol: best.expected_output_sol,
                                                                    profit_pct: best.profit_percent,
                                                                    ai_score,
                                                                    submitted: true,
                                                                    confirmed: true,
                                                                    profitable,
                                                                });
                                                                trade_memory.save();
                                                                scorer.save();
                                                                info!("TX confirmed: {} (total executions: {})", sig, total_executions);

                                                                let trade = ExecutedTrade {
                                                                    timestamp: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f").to_string(),
                                                                    token_mint: best.token_mint.clone(),
                                                                    buy_dex: buy_dex.to_string(),
                                                                    sell_dex: sell_dex.to_string(),
                                                                    input_sol: best.input_amount_sol,
                                                                    expected_profit_sol: best.gross_profit_sol,
                                                                    expected_profit_pct: best.profit_percent,
                                                                    tx_signature: sig.to_string(),
                                                                    status: "success".to_string(),
                                                                    actual_profit_sol: Some(net_profit),
                                                                    notes: format!("hops={} confidence={}", num_hops, best.confidence_score),
                                                                };
                                                                if let Err(e) = trade_logger.log_trade(&trade) {
                                                                    warn!("Failed to log successful trade: {}", e);
                                                                }
                                                            }
                                                            Err(e) => {
                                                                consecutive_failures += 1;
                                                                capital_manager.record_trade(
                                                                    (best.input_amount_sol * 1e9) as u64,
                                                                    0,
                                                                    false,
                                                                );
                                                                // ── AI learning: tx failure ──
                                                                scorer.update(&saved_features, false, 0.0);
                                                                let h = chrono::Utc::now().hour() as u8;
                                                                trade_memory.record(TradeRecord {
                                                                    timestamp_utc: chrono::Utc::now().timestamp(),
                                                                    hour_utc: h,
                                                                    dex_a: buy_dex.to_string(),
                                                                    dex_b: sell_dex.to_string(),
                                                                    token_mint: best.token_mint.clone(),
                                                                    input_sol: best.input_amount_sol,
                                                                    output_sol: 0.0,
                                                                    profit_pct: 0.0,
                                                                    ai_score,
                                                                    submitted: true,
                                                                    confirmed: false,
                                                                    profitable: false,
                                                                });
                                                                trade_memory.save();
                                                                scorer.save();
                                                                warn!("TX send failed (consecutive_failures={}): {}", consecutive_failures, e);
                                                                if consecutive_failures >= 50 {
                                                                    error!("Circuit breaker: 50 consecutive failures, shutting down");
                                                                    bail!("Circuit breaker triggered: 50 consecutive failures");
                                                                } else if consecutive_failures >= 10 {
                                                                    warn!("Circuit breaker: {} consecutive failures, pausing for 30s", consecutive_failures);
                                                                    circuit_breaker_until = Some(tokio::time::Instant::now() + tokio::time::Duration::from_secs(30));
                                                                }
                                                                let trade = ExecutedTrade {
                                                                    timestamp: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f").to_string(),
                                                                    token_mint: best.token_mint.clone(),
                                                                    buy_dex: buy_dex.to_string(),
                                                                    sell_dex: sell_dex.to_string(),
                                                                    input_sol: best.input_amount_sol,
                                                                    expected_profit_sol: best.gross_profit_sol,
                                                                    expected_profit_pct: best.profit_percent,
                                                                    tx_signature: String::new(),
                                                                    status: "failed".to_string(),
                                                                    actual_profit_sol: None,
                                                                    notes: format!("TX send error: {}", e),
                                                                };
                                                                if let Err(le) = trade_logger.log_trade(&trade) {
                                                                    warn!("Failed to log failed trade: {}", le);
                                                                }
                                                            }
                                                        }
                                                    }
                                                    Ok(false) => {
                                                        consecutive_failures += 1;
                                                        warn!("Simulation failed (consecutive_failures={}), skipping", consecutive_failures);
                                                        if consecutive_failures >= 50 {
                                                            error!("Circuit breaker: 50 consecutive failures, shutting down");
                                                            bail!("Circuit breaker triggered: 50 consecutive failures");
                                                        } else if consecutive_failures >= 10 {
                                                            warn!("Circuit breaker: {} consecutive failures, pausing for 30s", consecutive_failures);
                                                            circuit_breaker_until = Some(tokio::time::Instant::now() + tokio::time::Duration::from_secs(30));
                                                        }
                                                        let trade = ExecutedTrade {
                                                            timestamp: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f").to_string(),
                                                            token_mint: best.token_mint.clone(),
                                                            buy_dex: buy_dex.to_string(),
                                                            sell_dex: sell_dex.to_string(),
                                                            input_sol: best.input_amount_sol,
                                                            expected_profit_sol: best.gross_profit_sol,
                                                            expected_profit_pct: best.profit_percent,
                                                            tx_signature: String::new(),
                                                            status: "failed".to_string(),
                                                            actual_profit_sol: None,
                                                            notes: "Simulation failed".to_string(),
                                                        };
                                                        if let Err(e) = trade_logger.log_trade(&trade) {
                                                            warn!("Failed to log failed trade: {}", e);
                                                        }
                                                    }
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
            if let Err(e) = trade_logger.print_summary() {
                warn!("Failed to print trade summary: {}", e);
            }
            // Refresh balance, adapt risk, print capital summary
            if let Err(e) = capital_manager.refresh_balance() {
                warn!("Failed to refresh capital balance: {}", e);
            }
            capital_manager.adapt_risk();
            capital_manager.print_summary();

            // ── AI self-update cycle ───────────────────────────────────────
            let win_rate = trade_memory.overall_win_rate();
            adaptive_params.adapt(win_rate, &market_analyzer);
            opp_config = adaptive_params.to_opportunity_config();
            scorer.adapt_threshold(win_rate);
            scorer.save();
            trade_memory.print_summary();
            scorer.print_status();
            market_analyzer.print_status();
        }
    }
}
