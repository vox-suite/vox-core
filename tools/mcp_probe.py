#!/usr/bin/env python3
"""Probe one remote MCP tools/call endpoint with a bounded deterministic request."""
import argparse
import json
import sys
from urllib.parse import urlsplit
from urllib.request import Request, build_opener, HTTPRedirectHandler, ProxyHandler

MAX_BYTES = 2 * 1024 * 1024


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *_args):
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("url")
    parser.add_argument("--tool", required=True)
    parser.add_argument("--arguments", default="{}", help="JSON object")
    parser.add_argument("--local-test", action="store_true", help="Allow loopback HTTP fixture")
    args = parser.parse_args()
    parsed = urlsplit(args.url)
    if parsed.username or parsed.password or parsed.fragment:
        parser.error("URL must not contain credentials or a fragment")
    if parsed.scheme != "https" and not (args.local_test and parsed.scheme == "http" and parsed.hostname in {"localhost", "127.0.0.1", "::1"}):
        parser.error("Use HTTPS; --local-test allows only loopback HTTP")
    try:
        arguments = json.loads(args.arguments)
        if not isinstance(arguments, dict):
            raise ValueError("arguments must be a JSON object")
        request_id = "vox-probe-1"
        body = json.dumps({
            "jsonrpc": "2.0", "id": request_id, "method": "tools/call",
            "params": {"name": args.tool, "arguments": arguments,
                       "_meta": {"io.modelcontextprotocol/clientInfo": {"name": "vox-probe", "version": "1"}}},
        }).encode()
        if len(body) > MAX_BYTES:
            raise ValueError("request exceeds 2 MiB")
        request = Request(args.url, data=body, method="POST", headers={
            "Content-Type": "application/json", "Accept": "application/json",
            "MCP-Protocol-Version": "2026-07-28", "MCP-Method": "tools/call", "MCP-Name": args.tool,
        })
        opener = build_opener(ProxyHandler({}), NoRedirect())
        with opener.open(request, timeout=10) as response:
            payload = response.read(MAX_BYTES + 1)
        if len(payload) > MAX_BYTES:
            raise ValueError("response exceeds 2 MiB")
        result = json.loads(payload)
        if result.get("jsonrpc") != "2.0" or result.get("id") != request_id:
            raise ValueError("JSON-RPC version or response id does not match")
        if result.get("error"):
            raise ValueError(f"server returned JSON-RPC error {result['error'].get('code')}")
        tool_result = result.get("result")
        if not isinstance(tool_result, dict) or not ("content" in tool_result or "structuredContent" in tool_result):
            raise ValueError("tool result needs content or structuredContent")
        print("MCP tool call conforms to Vox's current tools/call response requirements")
        print(json.dumps(tool_result, indent=2))
        return 0
    except Exception as error:
        print(f"MCP probe failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
