#!/usr/bin/env python3
"""
Standalone AI Risk Manager for MEV Bot
This can be used alongside your existing Rust bot
"""

import json
import time
import hashlib
from datetime import datetime, timedelta
import numpy as np

class SimpleAIRiskManager:
    def __init__(self):
        self.trade_history = []
        self.pattern_weights = {}
        self.risk_parameters = {
            'max_position_size': 0.1,
            'min_profit_threshold': 0.001,
            'max_daily_loss': 0.5,
            'confidence_threshold': 0.7
        }
        self.performance = {
            'total_trades': 0,
            'profitable_trades': 0,
            'total_profit': 0.0,
            'total_loss': 0.0,
            'current_streak': 0
        }
        
    def evaluate_trade(self, trade_opportunity):
        """Evaluate whether to execute a trade"""
        confidence = self.calculate_confidence(trade_opportunity)
        
        # Check risk limits
        if not self.within_risk_limits():
            return False, "Risk limits exceeded"
            
        # Check confidence threshold
        if confidence < self.risk_parameters['confidence_threshold']:
            return False, f"Low confidence: {confidence:.2f}"
            
        return True, f"Approved with confidence: {confidence:.2f}"
    
    def calculate_confidence(self, opportunity):
        """Calculate confidence score for a trade opportunity"""
        base_confidence = min(opportunity.get('potential_profit_percent', 0) / 10.0, 1.0)
        
        # Check for similar historical patterns
        pattern_key = self.generate_pattern_key(opportunity)
        pattern_confidence = self.pattern_weights.get(pattern_key, 0.5)
        
        # Combine base confidence with learned pattern confidence
        final_confidence = (base_confidence * 0.4) + (pattern_confidence * 0.6)
        
        return final_confidence
    
    def generate_pattern_key(self, opportunity):
        """Generate a pattern key from trade opportunity"""
        key_data = f"{opportunity.get('token_mint', '')}|{opportunity.get('buy_dex', '')}|{opportunity.get('sell_dex', '')}"
        return hashlib.md5(key_data.encode()).hexdigest()[:12]
    
    def record_trade_result(self, opportunity, profit_loss, success=True):
        """Record trade outcome and learn from it"""
        trade_record = {
            'timestamp': datetime.now().isoformat(),
            'opportunity': opportunity,
            'profit_loss': profit_loss,
            'success': success,
            'pattern_key': self.generate_pattern_key(opportunity)
        }
        
        self.trade_history.append(trade_record)
        self.performance['total_trades'] += 1
        
        if success and profit_loss > 0:
            self.performance['profitable_trades'] += 1
            self.performance['total_profit'] += profit_loss
            self.performance['current_streak'] = max(0, self.performance['current_streak']) + 1
            
            # Reinforce successful pattern
            pattern_key = trade_record['pattern_key']
            current_weight = self.pattern_weights.get(pattern_key, 0.5)
            self.pattern_weights[pattern_key] = min(1.0, current_weight + 0.1)
            
        else:
            self.performance['total_loss'] += abs(profit_loss)
            self.performance['current_streak'] = min(0, self.performance['current_streak']) - 1
            
            # Penalize failed pattern
            pattern_key = trade_record['pattern_key']
            current_weight = self.pattern_weights.get(pattern_key, 0.5)
            self.pattern_weights[pattern_key] = max(0.0, current_weight - 0.2)
        
        # Adjust risk parameters based on performance
        self.adapt_risk_parameters()
        
        # Save state
        self.save_state()
    
    def within_risk_limits(self):
        """Check if we're within daily risk limits"""
        # Check daily loss limit
        today = datetime.now().date()
        today_trades = [t for t in self.trade_history 
                       if datetime.fromisoformat(t['timestamp']).date() == today]
        today_loss = sum(t['profit_loss'] for t in today_trades if t['profit_loss'] < 0)
        
        if abs(today_loss) >= self.risk_parameters['max_daily_loss']:
            return False
            
        # Check losing streak
        if self.performance['current_streak'] <= -3:
            return False
            
        return True
    
    def adapt_risk_parameters(self):
        """Adapt risk parameters based on performance"""
        if self.performance['total_trades'] == 0:
            return
            
        win_rate = self.performance['profitable_trades'] / self.performance['total_trades']
        
        # Adjust parameters based on performance
        if win_rate > 0.6 and self.performance['current_streak'] > 2:
            # Winning streak - be more aggressive
            self.risk_parameters['max_position_size'] = min(0.2, self.risk_parameters['max_position_size'] * 1.1)
            self.risk_parameters['min_profit_threshold'] = max(0.0005, self.risk_parameters['min_profit_threshold'] * 0.9)
            
        elif win_rate < 0.3 or self.performance['current_streak'] < -2:
            # Losing streak - be more conservative
            self.risk_parameters['max_position_size'] = max(0.02, self.risk_parameters['max_position_size'] * 0.7)
            self.risk_parameters['min_profit_threshold'] = min(0.005, self.risk_parameters['min_profit_threshold'] * 1.2)
            self.risk_parameters['confidence_threshold'] = min(0.9, self.risk_parameters['confidence_threshold'] + 0.1)
    
    def save_state(self):
        """Save risk manager state to file"""
        state = {
            'trade_history': self.trade_history[-1000:],  # Keep last 1000 trades
            'pattern_weights': self.pattern_weights,
            'risk_parameters': self.risk_parameters,
            'performance': self.performance,
            'last_updated': datetime.now().isoformat()
        }
        
        with open('ai_risk_state.json', 'w') as f:
            json.dump(state, f, indent=2)
    
    def load_state(self):
        """Load risk manager state from file"""
        try:
            with open('ai_risk_state.json', 'r') as f:
                state = json.load(f)
                
            self.trade_history = state.get('trade_history', [])
            self.pattern_weights = state.get('pattern_weights', {})
            self.risk_parameters = state.get('risk_parameters', self.risk_parameters)
            self.performance = state.get('performance', self.performance)
            
            print(f"✅ Loaded AI risk state: {len(self.trade_history)} historical trades")
            return True
            
        except FileNotFoundError:
            print("⚠️  No existing AI risk state found. Starting fresh.")
            return False
    
    def get_performance_report(self):
        """Generate performance report"""
        if self.performance['total_trades'] == 0:
            return "No trades executed yet."
            
        win_rate = (self.performance['profitable_trades'] / self.performance['total_trades']) * 100
        profit_factor = self.performance['total_profit'] / max(1, self.performance['total_loss'])
        
        report = f"""
🤖 AI RISK MANAGER PERFORMANCE REPORT
====================================
Total Trades: {self.performance['total_trades']}
Profitable Trades: {self.performance['profitable_trades']}
Win Rate: {win_rate:.1f}%
Total Profit: {self.performance['total_profit']:.6f} SOL
Total Loss: {self.performance['total_loss']:.6f} SOL
Profit Factor: {profit_factor:.2f}
Current Streak: {self.performance['current_streak']}
Patterns Learned: {len(self.pattern_weights)}

RISK PARAMETERS:
Max Position: {self.risk_parameters['max_position_size']:.3f} SOL
Min Profit Threshold: {self.risk_parameters['min_profit_threshold']:.4f} SOL
Confidence Threshold: {self.risk_parameters['confidence_threshold']:.2f}
        """
        return report

# Example usage
if __name__ == "__main__":
    risk_manager = SimpleAIRiskManager()
    risk_manager.load_state()
    
    # Example trade opportunity
    sample_opportunity = {
        'token_mint': 'So11111111111111111111111111111111111111112',
        'buy_dex': 'raydium',
        'sell_dex': 'pump',
        'potential_profit_percent': 1.5
    }
    
    # Evaluate the trade
    should_trade, reason = risk_manager.evaluate_trade(sample_opportunity)
    print(f"Trade decision: {should_trade} - {reason}")
    
    # Simulate a successful trade
    if should_trade:
        risk_manager.record_trade_result(sample_opportunity, 0.0025, success=True)
        print("✅ Recorded successful trade")
    
    print(risk_manager.get_performance_report())
