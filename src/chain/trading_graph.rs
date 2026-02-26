use crate::chain::pools::{Pool, PoolData};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;

pub struct TradingGraph {
    pub nodes: Vec<Pubkey>,
    edges: HashMap<Pubkey, Vec<(Pubkey, Pool)>>, // From token_mint to (to_token_mint, pool)
}

impl TradingGraph {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: HashMap::new(),
        }
    }

    pub fn add_pool(&mut self, pool: &Pool) {
        let token_a_mint = *pool.token_mint();
        let token_b_mint = *pool.base_mint();

        if !self.nodes.contains(&token_a_mint) {
            self.nodes.push(token_a_mint);
        }
        if !self.nodes.contains(&token_b_mint) {
            self.nodes.push(token_b_mint);
        }

        // Add edge from token_a to token_b using this pool
        self.edges.entry(token_a_mint).or_default().push((token_b_mint, pool.clone()));
        // Add edge from token_b to token_a using this pool (reverse trade)
        self.edges.entry(token_b_mint).or_default().push((token_a_mint, pool.clone()));
    }

    pub fn find_cycles(&self, start_node: Pubkey, length: usize) -> Vec<Vec<(Pubkey, Pubkey, Pool)>> {
        let mut cycles = Vec::new();
        let mut path = Vec::new();
        let mut visited = HashMap::new();

        self._dfs(start_node, start_node, length, &mut path, &mut visited, &mut cycles);

        cycles
    }

    fn _dfs(
        &self,
        current_node: Pubkey,
        start_node: Pubkey,
        length: usize,
        path: &mut Vec<(Pubkey, Pubkey, Pool)>,
        visited: &mut HashMap<Pubkey, usize>, // node -> index in path
        cycles: &mut Vec<Vec<(Pubkey, Pubkey, Pool)>>,
    ) {
        visited.insert(current_node, path.len());

        if let Some(outgoing_edges) = self.edges.get(&current_node) {
            for (next_node, pool) in outgoing_edges {
                let from_token = current_node;
                let to_token = *next_node;

                if to_token == start_node && path.len() + 1 == length {
                    let mut cycle = path.clone();
                    cycle.push((from_token, to_token, pool.clone()));
                    cycles.push(cycle);
                } else if !visited.contains_key(&to_token) && path.len() + 1 < length {
                    path.push((from_token, to_token, pool.clone()));
                    self._dfs(to_token, start_node, length, path, visited, cycles);
                    path.pop();
                }
            }
        }
        visited.remove(&current_node);
    }
}
