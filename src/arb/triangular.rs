use anyhow::{anyhow, Result};
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr;
use tracing::{debug, info};

/// Represents a single edge in the liquidity graph (a pool that enables a swap)
#[derive(Debug, Clone)]
pub struct PoolEdge {
    pub pool_address: String,
    pub token_a: Pubkey,
    pub token_b: Pubkey,
    pub dex: String,
}

/// A complete arbitrage route
#[derive(Debug, Clone)]
pub struct ArbitrageRoute {
    pub path: Vec<Pubkey>,                  // Token path: [SOL, USDC, BONK, SOL]
    pub pools: Vec<PoolEdge>,               // Pool path: [Raydium, Meteora, Pump.fun]
    pub profit_lamports: u64,
    pub profit_percent: f64,
    pub num_hops: usize,
}

/// Triangular/N-leg arbitrage finder
pub struct TriangularArbFinder {
    /// Map: (token_a, token_b) -> Vec<PoolEdge>
    edges: HashMap<(Pubkey, Pubkey), Vec<PoolEdge>>,
    all_tokens: HashSet<Pubkey>,
}

impl TriangularArbFinder {
    pub fn new() -> Self {
        Self {
            edges: HashMap::new(),
            all_tokens: HashSet::new(),
        }
    }

    /// Add a pool to the graph
    pub fn add_pool(&mut self, pool: PoolEdge) {
        // Bidirectional: can swap A->B or B->A
        let key_ab = (pool.token_a, pool.token_b);
        let key_ba = (pool.token_b, pool.token_a);

        self.edges.entry(key_ab).or_insert_with(Vec::new).push(pool.clone());
        self.edges.entry(key_ba).or_insert_with(Vec::new).push(pool.clone());

        self.all_tokens.insert(pool.token_a);
        self.all_tokens.insert(pool.token_b);
    }

    /// Find all 3-leg cycles starting from a given token
    pub fn find_3leg_routes(&self, start_token: Pubkey, min_profit_lamports: u64) -> Vec<ArbitrageRoute> {
        let mut routes = Vec::new();

        // BFS to find 3-leg paths: start -> A -> B -> start
        let mut queue = VecDeque::new();
        queue.push_back((vec![start_token], vec![])); // (path, pools_used)

        let mut visited = HashSet::new();

        while let Some((path, pools_used)) = queue.pop_front() {
            let current_token = *path.last().unwrap();

            // If we've gone 3 hops and can return to start, check for profitability
            if path.len() == 4 && current_token == start_token && pools_used.len() == 3 {
                // Found a 3-leg cycle
                // Calculate profit (simplified: assume 0.3% per hop, minus 0.5% fees)
                let gross_profit_percent = 0.001; // 0.1% per hop * 3
                let fees_percent = 0.005; // 0.5% total fees
                let net_profit_percent = gross_profit_percent - fees_percent;

                if net_profit_percent > 0.0 {
                    let route = ArbitrageRoute {
                        path: path.clone(),
                        pools: pools_used.clone(),
                        profit_lamports: ((1e9 as f64) * net_profit_percent) as u64,
                        profit_percent: net_profit_percent * 100.0,
                        num_hops: 3,
                    };

                    if route.profit_lamports >= min_profit_lamports {
                        routes.push(route);
                    }
                }
                continue;
            }

            // Don't go deeper than 3 hops
            if path.len() > 3 {
                continue;
            }

            // Explore neighbors
            if let Some(pool_edges) = self.edges.get(&(current_token, *self.all_tokens.iter().next().unwrap())) {
                for pool in pool_edges {
                    let next_token = if pool.token_a == current_token {
                        pool.token_b
                    } else {
                        pool.token_a
                    };

                    // Avoid revisiting tokens except for returning to start on final hop
                    let is_return_home = path.len() == 3 && next_token == start_token;
                    let already_visited = path.contains(&next_token);

                    if !already_visited || is_return_home {
                        let mut new_path = path.clone();
                        new_path.push(next_token);

                        let mut new_pools = pools_used.clone();
                        new_pools.push(pool.clone());

                        let state = (new_path, new_pools);
                        if !visited.contains(&state.0) {
                            visited.insert(state.0.clone());
                            queue.push_back(state);
                        }
                    }
                }
            }
        }

        info!("Found {} 3-leg routes from {}", routes.len(), start_token);
        routes
    }

    /// Find all N-leg cycles (generalized version)
    pub fn find_nleg_routes(
        &self,
        start_token: Pubkey,
        max_hops: usize,
        min_profit_lamports: u64,
    ) -> Vec<ArbitrageRoute> {
        let mut routes = Vec::new();

        // BFS for N-leg paths
        let mut queue = VecDeque::new();
        queue.push_back((vec![start_token], vec![])); // (path, pools_used)

        while let Some((path, pools_used)) = queue.pop_front() {
            let current_token = *path.last().unwrap();

            // Check if we have a complete cycle
            if path.len() > 2 && current_token == start_token && pools_used.len() <= max_hops {
                // Found a cycle
                let net_profit_percent = 0.001 * (pools_used.len() as f64) - 0.005;

                if net_profit_percent > 0.0 {
                    let route = ArbitrageRoute {
                        path: path.clone(),
                        pools: pools_used.clone(),
                        profit_lamports: ((1e9 as f64) * net_profit_percent) as u64,
                        profit_percent: net_profit_percent * 100.0,
                        num_hops: pools_used.len(),
                    };

                    if route.profit_lamports >= min_profit_lamports {
                        routes.push(route);
                    }
                }
                continue;
            }

            // Don't exceed hop limit
            if pools_used.len() >= max_hops {
                continue;
            }

            // Explore all neighbors
            for (token_pair, pool_edges) in &self.edges {
                for pool in pool_edges {
                    let next_token = if pool.token_a == current_token {
                        pool.token_b
                    } else if pool.token_b == current_token {
                        pool.token_a
                    } else {
                        continue;
                    };

                    // Can return to start if we have at least 2 hops
                    if next_token == start_token && pools_used.len() >= 2 {
                        let mut new_path = path.clone();
                        new_path.push(next_token);

                        let mut new_pools = pools_used.clone();
                        new_pools.push(pool.clone());

                        // Already found; add to routes if profitable
                        let num_hops = new_pools.len();
                        let net_profit_percent =
                            0.001 * (num_hops as f64) - 0.005;

                        if net_profit_percent > 0.0 {
                            let route = ArbitrageRoute {
                                path: new_path,
                                pools: new_pools,
                                profit_lamports: ((1e9 as f64) * net_profit_percent) as u64,
                                profit_percent: net_profit_percent * 100.0,
                                num_hops,
                            };

                            if route.profit_lamports >= min_profit_lamports {
                                routes.push(route);
                            }
                        }
                    } else if !path.contains(&next_token) {
                        // Avoid cycles within the path (except return to start)
                        let mut new_path = path.clone();
                        new_path.push(next_token);

                        let mut new_pools = pools_used.clone();
                        new_pools.push(pool.clone());

                        queue.push_back((new_path, new_pools));
                    }
                }
            }
        }

        info!("Found {} N-leg routes from {}", routes.len(), start_token);
        routes
    }

    /// Get the number of tokens in the graph
    pub fn token_count(&self) -> usize {
        self.all_tokens.len()
    }

    /// Get the number of pools (edges)
    pub fn pool_count(&self) -> usize {
        self.edges.len()
    }
}

/// Helper to detect if a route likely has positive profit
pub fn is_profitable_route(route: &ArbitrageRoute, fees_bps: u64) -> bool {
    // fees_bps = basis points (0.25% = 25 bps)
    let fees_percent = fees_bps as f64 / 10000.0;
    let gross_profit_percent = 0.0005 * (route.num_hops as f64); // 0.05% per hop

    gross_profit_percent > fees_percent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finder_creation() {
        let finder = TriangularArbFinder::new();
        assert_eq!(finder.token_count(), 0);
        assert_eq!(finder.pool_count(), 0);
    }

    #[test]
    fn test_add_pool() {
        let mut finder = TriangularArbFinder::new();
        let pool = PoolEdge {
            pool_address: "pool1".to_string(),
            token_a: Pubkey::new_unique(),
            token_b: Pubkey::new_unique(),
            dex: "raydium".to_string(),
        };

        finder.add_pool(pool);
        assert_eq!(finder.token_count(), 2);
    }

    #[test]
    fn test_profitable_route() {
        let route = ArbitrageRoute {
            path: vec![Pubkey::new_unique(); 4],
            pools: vec![],
            profit_lamports: 1000000,
            profit_percent: 0.5,
            num_hops: 3,
        };

        assert!(is_profitable_route(&route, 25)); // 0.25% fees
    }
}
