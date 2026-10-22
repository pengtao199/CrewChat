#!/usr/bin/env python3
"""Minimal ACP v1 agent used to smoke-test the cli-layer host.

Speaks newline-delimited JSON-RPC on stdio:
- initialize
- session/new
- session/prompt  -> one agent_message_chunk, then end_turn
- session/cancel (ignored for this one-turn fake)
"""

import json
import sys


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def read():
    line = sys.stdin.readline()
    if not line:
        return None
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        return None


def main():
    session_id = "fake-session"
    while True:
        message = read()
        if message is None:
            break
        method = message.get("method")
        request_id = message.get("id")

        if method == "initialize":
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {
                    "protocolVersion": 1,
                    "agentInfo": {"name": "fake-acp", "version": "0.1.0"},
                    "agentCapabilities": {},
                },
            })
        elif method == "session/new":
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {"sessionId": session_id},
            })
        elif method == "session/prompt":
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "hello from fake"},
                    },
                },
            })
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {"stopReason": "end_turn"},
            })
        elif method == "session/cancel":
            # Nothing in flight for this fake; the next prompt still works.
            pass
        elif request_id is not None:
            send({"jsonrpc": "2.0", "id": request_id, "result": {}})


if __name__ == "__main__":
    main()
