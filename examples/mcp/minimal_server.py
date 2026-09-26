#!/usr/bin/env python3
"""Local read-only MCP fixture for Vox integration authors.

Run: python3 examples/mcp/minimal_server.py
Probe: python3 tools/mcp_probe.py http://127.0.0.1:8765/mcp --tool echo.read --arguments '{"text":"hello"}' --local-test
"""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/mcp":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("content-length", "0"))
            if length <= 0 or length > 2 * 1024 * 1024:
                raise ValueError("invalid request size")
            request = json.loads(self.rfile.read(length))
            if request.get("jsonrpc") != "2.0" or request.get("method") != "tools/call":
                raise ValueError("expected tools/call JSON-RPC request")
            if self.headers.get("MCP-Protocol-Version") != "2026-07-28":
                raise ValueError("unsupported MCP protocol version")
            params = request.get("params", {})
            if params.get("name") != "echo.read":
                raise ValueError("unknown tool")
            text = params.get("arguments", {}).get("text", "")
            if not isinstance(text, str) or len(text) > 500:
                raise ValueError("text must be at most 500 characters")
            response = {
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "content": [{"type": "text", "text": text}],
                    "structuredContent": {"echo": text},
                    "isError": False,
                },
            }
            payload = json.dumps(response).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except (ValueError, KeyError, json.JSONDecodeError):
            self.send_error(400, "invalid MCP request")


if __name__ == "__main__":
    print("MCP test fixture listening at http://127.0.0.1:8765/mcp")
    ThreadingHTTPServer(("127.0.0.1", 8765), Handler).serve_forever()
