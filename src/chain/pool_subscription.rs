use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use chrono::Utc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolStateUpdate {
    pub pool_address: Pubkey,
    pub token_a: Pubkey,
    pub token_b: Pubkey,
    pub reserve_a: u128,
    pub reserve_b: u128,
    pub price: f64,
    pub liquidity: f64,
    pub timestamp: i64,
    pub slot: u64,
}

pub struct PoolSubscriptionManager {
    rpc: Arc<RpcClient>,
    pools: Arc<RwLock<HashMap<Pubkey, PoolStateUpdate>>>,
    update_tx: mpsc::UnboundedSender<PoolStateUpdate>,
    update_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<PoolStateUpdate>>>,
}

impl PoolSubscriptionManager {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            rpc,
            pools: Arc::new(RwLock::new(HashMap::new())),
            update_tx: tx,
            update_rx: Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }

    pub async fn subscribe_to_pool(&self, pool_address: Pubkey) -> Result<()> {
        let _account = self.rpc.get_account(&pool_address)?;
        
        let initial_state = PoolStateUpdate {
            pool_address,
            token_a: Pubkey::default(),
            token_b: Pubkey::default(),
            reserve_a: 0,
            reserve_b: 0,
            price: 0.0,
            liquidity: 0.0,
            timestamp: Utc::now().timestamp(),
            slot: 0,
        };

        let mut pools = self.pools.write().await;
        pools.insert(pool_address, initial_state.clone());

        let _ = self.update_tx.send(initial_state);
        
        println!("✅ Subscribed to pool: {}", pool_address);
        Ok(())
    }

    pub async fn subscribe_to_pools(&self, pool_addresses: Vec<Pubkey>) -> Result<()> {
        for address in pool_addresses {
            self.subscribe_to_pool(address).await?;
        }
        Ok(())
    }

    pub async fn get_pool_state(&self, pool_address: Pubkey) -> Result<PoolStateUpdate> {
        let pools = self.pools.read().await;
        pools.get(&pool_address)
            .cloned()
            .ok_or_else(|| anyhow!("Pool not found or not subscribed: {}", pool_address))
    }

    pub async fn get_all_pools(&self) -> Result<Vec<PoolStateUpdate>> {
        let pools = self.pools.read().await;
        Ok(pools.values().cloned().collect())
    }

    pub async fn update_pool_state(&self, update: PoolStateUpdate) -> Result<()> {
        let mut pools = self.pools.write().await;
        pools.insert(update.pool_address, update.clone());
        
        let _ = self.update_tx.send(update);
        Ok(())
    }

    pub async fn listen_for_updates(&self) -> Result<()> {
        let mut rx = self.update_rx.lock().await;
        
        println!("👂 Listening for pool updates...");
        
        while let Some(update) = rx.recv().await {
            println!("📊 Pool {} updated: price={:.6}, liquidity={:.2}", 
                update.pool_address, update.price, update.liquidity);
        }

        Ok(())
    }

    pub async fn get_recent_updates(&self, seconds: i64) -> Result<Vec<PoolStateUpdate>> {
        let now = Utc::now().timestamp();
        let pools = self.pools.read().await;
        
        let recent = pools
            .values()
            .filter(|p| (now - p.timestamp) <= seconds)
            .cloned()
            .collect();

        Ok(recent)
    }

    pub async fn detect_price_changes(&self, _threshold_percent: f64) -> Result<Vec<(Pubkey, f64, f64)>> {
        let pools = self.pools.read().await;
        let mut changes = Vec::new();

        for update in pools.values() {
            if update.liquidity > 10000.0 {
                changes.push((update.pool_address, update.price, update.liquidity));
            }
        }

        Ok(changes)
    }

    pub async fn estimate_next_block_opportunities(&self) -> Result<Vec<String>> {
        let pools = self.pools.read().await;
        let mut opportunities = Vec::new();

        let pools_vec: Vec<_> = pools.values().collect();
        
        for i in 0..pools_vec.len() {
            for j in (i + 1)..pools_vec.len() {
                let spread = (pools_vec[i].price - pools_vec[j].price).abs();
                if spread > 0.01 {
                    let opp = format!(
                        "Spread: {:.2}% between {} and {}",
                        spread * 100.0,
                        pools_vec[i].pool_address,
                        pools_vec[j].pool_address
                    );
                    opportunities.push(opp);
                }
            }
        }

        Ok(opportunities)
    }

    pub async fn cleanup_old_pools(&self, max_age_seconds: i64) -> Result<usize> {
        let now = Utc::now().timestamp();
        let mut pools = self.pools.write().await;
        
        let before = pools.len();
        pools.retain(|_, pool| (now - pool.timestamp) <= max_age_seconds);
        let removed = before - pools.len();

        println!("🧹 Cleaned up {} old pools", removed);
        Ok(removed)
    }

    pub async fn export_pool_state(&self, filename: &str) -> Result<()> {
        use std::fs::File;
        use std::io::Write;

        let pools = self.pools.read().await;
        let mut file = File::create(filename)?;

        writeln!(file, "pool_address,token_a,token_b,reserve_a,reserve_b,price,liquidity,timestamp")?;

        for pool in pools.values() {
            writeln!(
                file,
                "{},{},{},{},{},{:.6},{:.2},{}",
                pool.pool_address,
                pool.token_a,
                pool.token_b,
                pool.reserve_a,
                pool.reserve_b,
                pool.price,
                pool.liquidity,
                pool.timestamp
            )?;
        }

        println!("💾 Exported pool state to {}", filename);
        Ok(())
    }

    pub async fn print_summary(&self) -> Result<()> {
        let pools = self.pools.read().await;
        let total_liquidity: f64 = pools.values().map(|p| p.liquidity).sum();

        println!("\n📊 Pool Subscription Summary");
        println!("├─ Total Pools: {}", pools.len());
        println!("├─ Total Liquidity: ${:.2}", total_liquidity);
        println!("└─ Last Updated: {} UTC", Utc::now());

        for (idx, pool) in pools.values().enumerate().take(5) {
            println!("  {}. {} - Price: {:.6}, Liquidity: ${:.2}", 
                idx + 1, pool.pool_address, pool.price, pool.liquidity);
        }

        if pools.len() > 5 {
            println!("  ... and {} more pools", pools.len() - 5);
        }

        Ok(())
    }
}

pub struct PoolStateCache {
    cache: Arc<RwLock<HashMap<Pubkey, CachedPoolState>>>,
    ttl_seconds: u64,
}

#[derive(Clone)]
struct CachedPoolState {
    state: PoolStateUpdate,
    cached_at: i64,
}

impl PoolStateCache {
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl_seconds,
        }
    }

    pub async fn get(&self, pool: Pubkey) -> Option<PoolStateUpdate> {
        let cache = self.cache.read().await;
        
        if let Some(cached) = cache.get(&pool) {
            let age = Utc::now().timestamp() - cached.cached_at;
            if age <= self.ttl_seconds as i64 {
                return Some(cached.state.clone());
            }
        }

        None
    }

    pub async fn set(&self, state: PoolStateUpdate) {
        let mut cache = self.cache.write().await;
        cache.insert(state.pool_address, CachedPoolState {
            state,
            cached_at: Utc::now().timestamp(),
        });
    }

    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }

    pub async fn get_stats(&self) -> CacheStats {
        let cache = self.cache.read().await;
        let now = Utc::now().timestamp();
        
        let valid_entries = cache.values()
            .filter(|e| (now - e.cached_at) <= self.ttl_seconds as i64)
            .count();

        CacheStats {
            total_entries: cache.len(),
            valid_entries,
            expired_entries: cache.len() - valid_entries,
            ttl_seconds: self.ttl_seconds,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_entries: usize,
    pub valid_entries: usize,
    pub expired_entries: usize,
    pub ttl_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache() {
        let cache = PoolStateCache::new(60);
        
        let update = PoolStateUpdate {
            pool_address: Pubkey::default(),
            token_a: Pubkey::default(),
            token_b: Pubkey::default(),
            reserve_a: 1000,
            reserve_b: 2000,
            price: 0.5,
            liquidity: 3000.0,
            timestamp: Utc::now().timestamp(),
            slot: 0,
        };

        cache.set(update.clone()).await;
        let retrieved = cache.get(update.pool_address).await;
        assert!(retrieved.is_some());
    }
}
