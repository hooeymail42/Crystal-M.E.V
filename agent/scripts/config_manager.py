#!/usr/bin/env python3
import json
import toml
import os
from datetime import datetime

class ConfigManager:
    def __init__(self):
        self.bot_config_path = "../config.toml"
        self.agent_config_path = "config/agent_config.json"
    
    def load_bot_config(self):
        """Load current bot configuration"""
        if os.path.exists(self.bot_config_path):
            with open(self.bot_config_path, 'r') as f:
                return toml.load(f)
        return {}
    
    def update_bot_config(self, updates):
        """Update bot configuration dynamically"""
        try:
            config = self.load_bot_config()
            
            # Apply updates
            for key_path, value in updates.items():
                keys = key_path.split('.')
                current = config
                for key in keys[:-1]:
                    if key not in current:
                        current[key] = {}
                    current = current[key]
                current[keys[-1]] = value
            
            # Save updated config
            with open(self.bot_config_path, 'w') as f:
                toml.dump(config, f)
            
            print(f"Configuration updated: {updates}")
            return True
            
        except Exception as e:
            print(f"Error updating config: {e}")
            return False
    
    def optimize_parameters(self, performance_data):
        """Automatically optimize trading parameters based on performance"""
        optimizations = {}
        
        # Example optimization logic
        if performance_data.get('success_rate', 0) < 0.3:
            optimizations['risk.max_position_size_sol'] = 0.05  # Reduce position size
            optimizations['strategies.min_profit_threshold_sol'] = 0.02  # Increase profit threshold
        
        if performance_data.get('slippage_avg', 0) > 0.1:
            optimizations['risk.max_slippage_bps'] = 50  # Reduce max slippage
        
        return optimizations

# Example usage
if __name__ == "__main__":
    manager = ConfigManager()
    
    # Example: Reduce position size if losing
    manager.update_bot_config({
        'risk.max_position_size_sol': 0.1,
        'risk.max_slippage_bps': 75
    })
