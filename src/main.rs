use solana_mev_bot::{
    chain::{
        pool_discovery::{PoolDiscovery, PoolDiscoveryConfig},
        token_fetch::{TokenFetchConfig, TokenFetcher},
        token_price::{MarketDataFetcher, PriceMonitor},
    },
    config::Config,
};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{signature::Keypair, signer::Signer};
use std::sync::Arc;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_line_number(true)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // Load configuration from environment variables
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to load configuration: {}", e);
            return;
        }
    };

    println!("Configuration loaded successfully!");
    println!("RPC URL: {}", config.rpc.url);
    println!("Compute unit limit: {}", config.bot.compute_unit_limit);

    // Parse wallet private key and derive wallet address
    let wallet_keypair = Keypair::from_base58_string(&config.wallet.private_key);
    let wallet_address = wallet_keypair.pubkey().to_string();
    println!("Wallet address: {}", wallet_address);

    // Initialize RPC client
    let rpc_client = Arc::new(RpcClient::new(config.rpc.url.clone()));

    // ── Pool discovery ────────────────────────────────────────────────────────
    let mut pool_discovery = PoolDiscovery::new(
        rpc_client.clone(),
        PoolDiscoveryConfig::default(),
    );

    println!("\nRunning pool discovery across all DEXs (this may take 15-60 s on public RPC)...");
    let _ = pool_discovery.discover_all_pools().await.map(|counts| {
        for (dex, pools) in &counts {
            println!("  Discovered {} pools on {}", pools.len(), dex);
        }
    }).map_err(|e| eprintln!("Pool discovery failed (falling back to hardcoded pools): {}", e));

    // ── Token fetcher ─────────────────────────────────────────────────────────
    let token_fetch_config = TokenFetchConfig {
        max_retries: 3,
        retry_delay_ms: 1000,
        batch_size: 10,
        timeout_seconds: 30,
        enable_caching: true,
        cache_ttl_seconds: 300,
    };

    let mut token_fetcher = TokenFetcher::new(rpc_client.clone(), token_fetch_config);

    // Initialize market data fetcher
    let mut market_fetcher = MarketDataFetcher::new(rpc_client.clone());

    // Initialize price monitor
    let mut price_monitor = PriceMonitor::new(rpc_client, 5000, 0.5);

    // ── Per-mint processing ───────────────────────────────────────────────────
    for mint_config in &config.routing.mint_config_list {
        println!("\nProcessing mint: {}", mint_config.mint);

        // Collect discovered pools for this mint, keyed by DEX name.
        let discovered = pool_discovery.get_pools_for_token(&mint_config.mint);

        // Merge hardcoded + discovered pool addresses, deduplicating.
        let merge = |hardcoded: Option<&Vec<String>>, dex: &str| -> Option<Vec<String>> {
            let mut pools: Vec<String> = hardcoded.cloned().unwrap_or_default();
            for p in discovered.iter().filter(|p| p.dex == dex) {
                if !pools.contains(&p.address) {
                    pools.push(p.address.clone());
                }
            }
            if pools.is_empty() { None } else { Some(pools) }
        };

        let raydium_pools     = merge(mint_config.raydium_pool_list.as_ref(),      "raydium_v4");
        let raydium_cp_pools  = merge(mint_config.raydium_cp_pool_list.as_ref(),   "raydium_cp");
        let pump_pools        = merge(mint_config.pump_pool_list.as_ref(),          "pump");
        let dlmm_pools        = merge(mint_config.meteora_dlmm_pool_list.as_ref(), "meteora_dlmm");
        let whirlpool_pools   = merge(mint_config.whirlpool_pool_list.as_ref(),     "whirlpool");

        println!(
            "  Pool counts → raydium_v4: {}, raydium_cp: {}, pump: {}, dlmm: {}, whirlpool: {}",
            raydium_pools.as_ref().map_or(0, |v| v.len()),
            raydium_cp_pools.as_ref().map_or(0, |v| v.len()),
            pump_pools.as_ref().map_or(0, |v| v.len()),
            dlmm_pools.as_ref().map_or(0, |v| v.len()),
            whirlpool_pools.as_ref().map_or(0, |v| v.len()),
        );

        match token_fetcher
            .initialize_pool_data(
                &mint_config.mint,
                &wallet_address,
                raydium_pools.as_ref(),
                raydium_cp_pools.as_ref(),
                pump_pools.as_ref(),
                dlmm_pools.as_ref(),
                whirlpool_pools.as_ref(),
                mint_config.raydium_clmm_pool_list.as_ref(),
                mint_config.meteora_damm_pool_list.as_ref(),
                mint_config.solfi_pool_list.as_ref(),
                mint_config.meteora_damm_v2_pool_list.as_ref(),
                mint_config.vertigo_pool_list.as_ref(),
            )
            .await
        {
            Ok(pool_data) => {
                println!("Successfully loaded pool data for mint: {}", mint_config.mint);
                println!("  - Raydium pools: {}", pool_data.raydium_pools.len());
                println!("  - Pump pools: {}", pool_data.pump_pools.len());
                println!("  - Whirlpool pools: {}", pool_data.whirlpool_pools.len());

                // Fetch token price
                match market_fetcher.fetch_token_price(&mint_config.mint).await {
                    Ok(price) => {
                        println!(
                            "Token price: ${:.6} USD, {:.6} SOL (source: {})",
                            price.price_usd, price.price_sol, price.source
                        );
                    }
                    Err(e) => println!("Failed to fetch token price: {}", e),
                }

                // Calculate arbitrage opportunities
                match market_fetcher
                    .calculate_arbitrage_opportunities(&pool_data)
                    .await
                {
                    Ok(opportunities) => {
                        if opportunities.is_empty() {
                            println!("No significant arbitrage opportunities found");
                        } else {
                            println!("Found {} arbitrage opportunities:", opportunities.len());
                            for (i, opp) in opportunities.iter().enumerate() {
                                println!(
                                    "  {}. {}: Buy on {} at {:.6}, Sell on {} at {:.6} ({}% profit)",
                                    i + 1,
                                    opp.token_mint,
                                    opp.best_buy_dex,
                                    opp.best_buy_price,
                                    opp.best_sell_dex,
                                    opp.best_sell_price,
                                    opp.potential_profit_percent
                                );
                            }
                        }
                    }
                    Err(e) => println!("Failed to calculate arbitrage opportunities: {}", e),
                }
            }
            Err(e) => {
                println!("Failed to load pool data for mint {}: {}", mint_config.mint, e);
            }
        }
    }

    // ── Price monitoring ──────────────────────────────────────────────────────
    println!("\nStarting price monitoring...");
    let _mints: Vec<String> = config
        .routing
        .mint_config_list
        .iter()
        .map(|mc| mc.mint.clone())
        .collect();

    // Uncomment to start continuous price monitoring:
    // price_monitor.start_monitoring(_mints).await;

    println!("Bot ready. Pool discovery active ({} total pools discovered).",
        pool_discovery.get_all_pools().len());

    // Optional: background pool refresh task
    // let mut pd = pool_discovery;
    // tokio::spawn(async move {
    //     loop {
    //         tokio::time::sleep(tokio::time::Duration::from_secs(
    //             pd.config.refresh_interval_minutes * 60,
    //         )).await;
    //         let _ = pd.discover_all_pools().await;
    //     }
    // });
}
