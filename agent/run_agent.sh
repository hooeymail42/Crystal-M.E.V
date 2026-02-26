#!/bin/bash

# MEV Bot Agent Controller
# Usage: ./run_agent.sh [start|stop|status|monitor]

AGENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG_DIR="$AGENT_DIR/logs"
PID_FILE="$AGENT_DIR/agent.pid"

start_agent() {
    echo "🤖 Starting MEV Bot Agent..."
    
    # Create log directory
    mkdir -p "$LOG_DIR"
    
    # Start monitoring agent
    nohup python3 "$AGENT_DIR/scripts/health_monitor.py" > "$LOG_DIR/monitor.log" 2>&1 &
    echo $! > "$PID_FILE"
    
    echo "✅ Agent started with PID: $(cat $PID_FILE)"
    echo "📊 Logs: $LOG_DIR/monitor.log"
}

stop_agent() {
    if [ -f "$PID_FILE" ]; then
        PID=$(cat "$PID_FILE")
        echo "🛑 Stopping MEV Bot Agent (PID: $PID)..."
        kill $PID
        rm "$PID_FILE"
        echo "✅ Agent stopped"
    else
        echo "❌ No running agent found"
    fi
}

agent_status() {
    if [ -f "$PID_FILE" ]; then
        PID=$(cat "$PID_FILE")
        if ps -p $PID > /dev/null; then
            echo "✅ MEV Bot Agent is running (PID: $PID)"
        else
            echo "❌ Agent PID file exists but process not running"
            rm "$PID_FILE"
        fi
    else
        echo "❌ MEV Bot Agent is not running"
    fi
}

case "$1" in
    start)
        start_agent
        ;;
    stop)
        stop_agent
        ;;
    status)
        agent_status
        ;;
    monitor)
        python3 "$AGENT_DIR/scripts/health_monitor.py"
        ;;
    *)
        echo "Usage: $0 {start|stop|status|monitor}"
        exit 1
        ;;
esac
