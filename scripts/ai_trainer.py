#!/usr/bin/env python3
"""
AI Risk Manager Trainer
Trains the risk management model using historical trade data
"""

import json
import numpy as np
from datetime import datetime
import hashlib

class AIRiskTrainer:
    def __init__(self):
        self.pattern_database = {}
        self.performance_history = []
        
    def load_trade_history(self, file_path):
        """Load historical trade data for training"""
        try:
            with open(file_path, 'r') as f:
                data = json.load(f)
                return data.get('trade_history', [])
        except FileNotFoundError:
            return []
    
    def analyze_patterns(self, trade_history):
        """Analyze patterns in successful vs failed trades"""
        successful_trades = [t for t in trade_history if t.get('profit_loss', 0) > 0]
        failed_trades = [t for t in trade_history if t.get('profit_loss', 0) <= 0]
        
        patterns = {
            'successful': self.extract_patterns(successful_trades),
            'failed': self.extract_patterns(failed_trades)
        }
        
        return patterns
    
    def extract_patterns(self, trades):
        """Extract common patterns from trades"""
        patterns = {}
        
        for trade in trades:
            # Create pattern key based on trade characteristics
            pattern_key = self.create_pattern_key(trade)
            
            if pattern_key not in patterns:
                patterns[pattern_key] = {
                    'count': 0,
                    'total_profit': 0,
                    'avg_profit': 0,
                    'success_rate': 0
                }
            
            patterns[pattern_key]['count'] += 1
            patterns[pattern_key]['total_profit'] += trade.get('profit_loss', 0)
            patterns[pattern_key]['avg_profit'] = (
                patterns[pattern_key]['total_profit'] / 
                patterns[pattern_key]['count']
            )
        
        return patterns
    
    def create_pattern_key(self, trade):
        """Create a unique key identifying this trade pattern"""
        key_data = f"{trade.get('token_pair', '')}|{trade.get('dex_combination', '')}|{trade.get('strategy_used', '')}"
        return hashlib.md5(key_data.encode()).hexdigest()[:16]
    
    def train_model(self, patterns):
        """Train the AI model based on historical patterns"""
        model_weights = {}
        
        for pattern_type, pattern_data in patterns.items():
            for pattern_key, stats in pattern_data.items():
                success_rate = stats.get('avg_profit', 0)
                if success_rate > 0:
                    model_weights[pattern_key] = success_rate * 2  # Weight successful patterns higher
                else:
                    model_weights[pattern_key] = success_rate  # Penalize losing patterns
        
        return model_weights
    
    def optimize_parameters(self, trade_history):
        """Optimize risk parameters based on performance"""
        if not trade_history:
            return {
                'max_position_size': 0.1,
                'min_profit_threshold': 0.001,
                'confidence_threshold': 0.7
            }
        
        recent_trades = trade_history[-100:]  # Last 100 trades
        if not recent_trades:
            return {
                'max_position_size': 0.1,
                'min_profit_threshold': 0.001,
                'confidence_threshold': 0.7
            }
            
        win_rate = len([t for t in recent_trades if t.get('profit_loss', 0) > 0]) / len(recent_trades)
        
        # Adjust parameters based on performance
        if win_rate > 0.7:
            # High win rate - be more aggressive
            return {
                'max_position_size': 0.15,
                'min_profit_threshold': 0.0005,
                'confidence_threshold': 0.6
            }
        elif win_rate < 0.3:
            # Low win rate - be more conservative
            return {
                'max_position_size': 0.05,
                'min_profit_threshold': 0.002,
                'confidence_threshold': 0.8
            }
        else:
            # Moderate performance - balanced approach
            return {
                'max_position_size': 0.1,
                'min_profit_threshold': 0.001,
                'confidence_threshold': 0.7
            }

    def generate_training_report(self, patterns):
        """Generate a report of training results"""
        successful_patterns = patterns.get('successful', {})
        failed_patterns = patterns.get('failed', {})
        
        report = {
            'training_date': datetime.now().isoformat(),
            'successful_patterns_count': len(successful_patterns),
            'failed_patterns_count': len(failed_patterns),
            'top_successful_patterns': [],
            'top_failed_patterns': []
        }
        
        # Get top 5 successful patterns
        for pattern_key, stats in list(successful_patterns.items())[:5]:
            report['top_successful_patterns'].append({
                'pattern': pattern_key,
                'avg_profit': stats['avg_profit'],
                'count': stats['count']
            })
            
        # Get top 5 failed patterns
        for pattern_key, stats in list(failed_patterns.items())[:5]:
            report['top_failed_patterns'].append({
                'pattern': pattern_key,
                'avg_loss': stats['avg_profit'],
                'count': stats['count']
            })
            
        return report

if __name__ == "__main__":
    trainer = AIRiskTrainer()
    
    # Load existing trade history
    trade_history = trainer.load_trade_history('ai_risk_state.json')
    
    if trade_history:
        print(f"📊 Training on {len(trade_history)} historical trades...")
        
        # Analyze patterns
        patterns = trainer.analyze_patterns(trade_history)
        
        # Train model
        model_weights = trainer.train_model(patterns)
        
        # Optimize parameters
        optimal_params = trainer.optimize_parameters(trade_history)
        
        # Generate report
        report = trainer.generate_training_report(patterns)
        
        print("✅ AI Training Complete!")
        print(f"Optimal Parameters: {optimal_params}")
        print(f"Model Weights: {len(model_weights)} patterns learned")
        print(f"Successful Patterns: {report['successful_patterns_count']}")
        print(f"Failed Patterns: {report['failed_patterns_count']}")
        
        # Save training results
        with open('training_report.json', 'w') as f:
            json.dump(report, f, indent=2)
            
        print("📄 Training report saved to training_report.json")
    else:
        print("No historical data found. Starting with default parameters.")
