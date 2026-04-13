#!/bin/bash
set -e

AGENT_CONFIG="${REZOLUS_AGENT_CONFIG:-/etc/rezolus/agent.toml}"
AGENT_URL="http://127.0.0.1:4241/"

# Start the Rezolus agent in the background
echo "Starting Rezolus agent..."
rezolus "$AGENT_CONFIG" &
AGENT_PID=$!

# Wait for the agent to become ready
echo "Waiting for agent to be ready..."
for i in $(seq 1 30); do
    if curl -sf "$AGENT_URL" > /dev/null 2>&1; then
        echo "Agent is ready (pid $AGENT_PID)"
        break
    fi
    if ! kill -0 "$AGENT_PID" 2>/dev/null; then
        echo "Error: Agent process exited unexpectedly"
        exit 1
    fi
    sleep 1
done

if ! curl -sf "$AGENT_URL" > /dev/null 2>&1; then
    echo "Error: Agent failed to start within 30 seconds"
    exit 1
fi

# Execute the user's command, or default to keeping the container alive
if [ $# -eq 0 ]; then
    echo "Agent running on port 4241. Use 'rezolus-capture' to start a recording."
    wait "$AGENT_PID"
else
    exec "$@"
fi
