#!/usr/bin/env python3
"""ACP v1 fake that asks for permission before answering a prompt."""

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
    session_id = "fake-permission-session"
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
                    "agentInfo": {"name": "fake-permission", "version": "0.1.0"},
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
                "id": 9001,
                "method": "session/request_permission",
                "params": {
                    "sessionId": session_id,
                    "toolCall": {
                        "toolCallId": "tool-1",
                        "title": "Read a file",
                        "kind": "read",
                        "status": "pending",
                    },
                    "options": [
                        {"optionId": "allow_once", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"},
                    ],
                },
            })
            outcome = "cancelled"
            while True:
                response = read()
                if response is None:
                    return
                if response.get("id") == 9001:
                    result = response.get("result") or {}
                    selected = result.get("outcome") or {}
                    if selected.get("outcome") == "selected":
                        outcome = selected.get("optionId", "selected")
                    break
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": f"permission {outcome}"},
                    },
                },
            })
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {"stopReason": "end_turn"},
            })
        elif request_id is not None:
            send({"jsonrpc": "2.0", "id": request_id, "result": {}})


if __name__ == "__main__":
    main()
