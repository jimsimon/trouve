#!/usr/bin/env python3
"""Qualify Codex app-server dynamic tools against the installed Codex CLI.

This is a live, manually invoked probe. It uses the CLI's existing login,
creates a disposable Codex thread, runs one dynamic-tool turn, restarts
app-server, resumes the thread without re-sending the tool schema, and runs a
second turn. The disposable thread is deleted unless --keep-thread is passed.
"""

from __future__ import annotations

import argparse
from collections import deque
import json
import os
from pathlib import Path
import queue
import shutil
import signal
import subprocess
import sys
import threading
import time
from typing import Any, Callable


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANAGED_CODEX = (
    Path.home() / ".local" / "share" / "trouve" / "cli" / "bin" / "codex"
)
TOOL_NAME = "trouve_qualification_echo"
TOOL_ARGUMENT = "codex-dynamic-tool-ok"
TOOL_RESULT = "TROUVE_CODEX_DYNAMIC_TOOL_OK"
IGNORED_ITEM_TYPES = {
    "",
    "agentMessage",
    "contextCompaction",
    "plan",
    "reasoning",
    "userMessage",
}


class QualificationError(RuntimeError):
    """A failed qualification assertion or app-server protocol operation."""


def remaining(deadline: float) -> float:
    value = deadline - time.monotonic()
    if value <= 0:
        raise QualificationError("timed out waiting for Codex app-server")
    return value


class CodexAppServer:
    """Minimal newline-delimited JSON-RPC client for `codex app-server`."""

    def __init__(self, binary: Path, cwd: Path, timeout: float) -> None:
        self.timeout = timeout
        self._next_id = 1
        self._messages: queue.Queue[dict[str, Any] | BaseException | None] = queue.Queue()
        self._pending: dict[int | str, dict[str, Any]] = {}
        self._stderr: deque[str] = deque(maxlen=40)
        self._write_lock = threading.Lock()
        self.process = subprocess.Popen(
            [str(binary), "app-server"],
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            start_new_session=os.name == "posix",
        )
        if self.process.stdin is None or self.process.stdout is None or self.process.stderr is None:
            self.close()
            raise QualificationError("failed to open Codex app-server stdio")
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        try:
            for line in self.process.stdout:
                if not line.strip():
                    continue
                try:
                    message = json.loads(line)
                except json.JSONDecodeError as error:
                    self._messages.put(
                        QualificationError(f"app-server emitted invalid JSON: {error}")
                    )
                    return
                if not isinstance(message, dict):
                    self._messages.put(
                        QualificationError("app-server emitted a non-object JSON-RPC message")
                    )
                    return
                self._messages.put(message)
        except BaseException as error:  # reader failures must reach the main thread
            self._messages.put(error)
        finally:
            self._messages.put(None)

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self._stderr.append(line.rstrip())

    def stderr_tail(self) -> str:
        return "\n".join(self._stderr)

    def _send(self, message: dict[str, Any]) -> None:
        if self.process.poll() is not None:
            raise QualificationError(self._exit_message())
        encoded = json.dumps(message, separators=(",", ":"))
        assert self.process.stdin is not None
        try:
            with self._write_lock:
                self.process.stdin.write(encoded + "\n")
                self.process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise QualificationError(self._exit_message()) from error

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def respond(self, request_id: int | str, result: dict[str, Any]) -> None:
        self._send({"jsonrpc": "2.0", "id": request_id, "result": result})

    def respond_error(self, request_id: int | str, message: str) -> None:
        self._send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32603, "message": message},
            }
        )

    def receive(self, deadline: float) -> dict[str, Any]:
        try:
            message = self._messages.get(timeout=remaining(deadline))
        except queue.Empty as error:
            raise QualificationError("timed out waiting for Codex app-server") from error
        if message is None:
            raise QualificationError(self._exit_message())
        if isinstance(message, BaseException):
            raise QualificationError(f"Codex app-server reader failed: {message}") from message
        return message

    def request(
        self,
        method: str,
        params: dict[str, Any],
        *,
        handler: Callable[[str, dict[str, Any]], dict[str, Any]] | None = None,
        notifications: list[dict[str, Any]] | None = None,
    ) -> dict[str, Any]:
        request_id = self._next_id
        self._next_id += 1
        self._send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }
        )
        deadline = time.monotonic() + self.timeout
        while True:
            response = self._pending.pop(request_id, None)
            if response is None:
                message = self.receive(deadline)
                if "method" not in message and "id" in message:
                    response_id = message["id"]
                    if response_id != request_id:
                        self._pending[response_id] = message
                        continue
                    response = message
                else:
                    self.dispatch(message, handler=handler, notifications=notifications)
                    continue
            if "error" in response:
                raise QualificationError(
                    f"{method} failed: {json.dumps(response['error'], sort_keys=True)}"
                )
            result = response.get("result")
            if not isinstance(result, dict):
                raise QualificationError(f"{method} returned a non-object result")
            return result

    def dispatch(
        self,
        message: dict[str, Any],
        *,
        handler: Callable[[str, dict[str, Any]], dict[str, Any]] | None,
        notifications: list[dict[str, Any]] | None,
    ) -> None:
        method = message.get("method")
        if not isinstance(method, str):
            raise QualificationError(f"unexpected JSON-RPC message: {message!r}")
        params = message.get("params", {})
        if not isinstance(params, dict):
            raise QualificationError(f"{method} carried non-object params")
        if "id" in message:
            request_id = message["id"]
            if handler is None:
                self.respond_error(request_id, f"unexpected server request {method}")
                raise QualificationError(f"unexpected Codex server request: {method}")
            try:
                result = handler(method, params)
            except BaseException as error:
                self.respond_error(request_id, str(error))
                raise
            self.respond(request_id, result)
        elif notifications is not None:
            notifications.append(message)

    def handshake(self) -> None:
        self.request(
            "initialize",
            {
                "clientInfo": {"name": "trouve-qualification", "version": "1"},
                "capabilities": {"experimentalApi": True},
            },
        )
        self.notify("initialized", {})

    def _exit_message(self) -> str:
        code = self.process.poll()
        detail = self.stderr_tail()
        message = f"Codex app-server exited unexpectedly (status {code})"
        return f"{message}:\n{detail}" if detail else message

    def close(self) -> None:
        if not hasattr(self, "process") or self.process.poll() is not None:
            return
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except OSError:
            pass
        try:
            if os.name == "posix":
                os.killpg(self.process.pid, signal.SIGTERM)
            else:
                self.process.terminate()
            self.process.wait(timeout=5)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            try:
                if os.name == "posix":
                    os.killpg(self.process.pid, signal.SIGKILL)
                else:
                    self.process.kill()
                self.process.wait(timeout=5)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                pass

    def __enter__(self) -> CodexAppServer:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


def dynamic_tool_spec() -> dict[str, Any]:
    return {
        "type": "function",
        "name": TOOL_NAME,
        "description": (
            "Qualification-only echo. When explicitly asked, call this tool exactly once "
            "with the supplied token and return the tool's text result."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {"token": {"type": "string"}},
            "required": ["token"],
            "additionalProperties": False,
        },
    }


def app_server(binary: Path, cwd: Path, timeout: float) -> CodexAppServer:
    server = CodexAppServer(binary, cwd, timeout)
    try:
        server.handshake()
    except BaseException:
        server.close()
        raise
    return server


def start_thread(server: CodexAppServer, cwd: Path, model: str | None) -> str:
    params: dict[str, Any] = {
        "cwd": str(cwd),
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "serviceName": "trouve-qualification",
        "developerInstructions": (
            "This thread qualifies a protocol callback. When the user explicitly names "
            f"{TOOL_NAME}, call that dynamic tool exactly once. Do not use any built-in, "
            "filesystem, shell, web, or MCP tool."
        ),
        "dynamicTools": [dynamic_tool_spec()],
    }
    if model:
        params["model"] = model
    result = server.request("thread/start", params)
    thread_id = result.get("thread", {}).get("id")
    if not isinstance(thread_id, str) or not thread_id:
        raise QualificationError("thread/start omitted result.thread.id")
    return thread_id


def resume_thread(
    server: CodexAppServer, thread_id: str, cwd: Path, model: str | None
) -> None:
    # dynamicTools is intentionally omitted: app-server promises to restore the
    # persisted schema when a client resumes a thread without overriding it.
    params: dict[str, Any] = {
        "threadId": thread_id,
        "cwd": str(cwd),
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "developerInstructions": (
            "This thread qualifies a protocol callback. When the user explicitly names "
            f"{TOOL_NAME}, call that dynamic tool exactly once. Do not use any built-in, "
            "filesystem, shell, web, or MCP tool."
        ),
    }
    if model:
        params["model"] = model
    result = server.request("thread/resume", params)
    resumed_id = result.get("thread", {}).get("id")
    if resumed_id != thread_id:
        raise QualificationError(
            f"thread/resume returned {resumed_id!r}, expected {thread_id!r}"
        )


def parse_arguments(value: Any) -> dict[str, Any]:
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except json.JSONDecodeError as error:
            raise QualificationError("dynamic tool arguments were invalid JSON") from error
    if not isinstance(value, dict):
        raise QualificationError("dynamic tool arguments were not an object")
    return value


def notification_turn_id(params: dict[str, Any]) -> str | None:
    value = params.get("turnId")
    if isinstance(value, str):
        return value
    turn = params.get("turn")
    if isinstance(turn, dict) and isinstance(turn.get("id"), str):
        return turn["id"]
    return None


def run_turn(server: CodexAppServer, thread_id: str, ordinal: int) -> dict[str, Any]:
    calls: list[dict[str, Any]] = []
    notifications: list[dict[str, Any]] = []
    started: dict[str, dict[str, Any]] = {}
    completed: dict[str, dict[str, Any]] = {}
    unexpected_items: list[str] = []
    assistant_text: list[str] = []

    def handle_server_request(method: str, params: dict[str, Any]) -> dict[str, Any]:
        if method != "item/tool/call":
            raise QualificationError(f"unexpected Codex server request during turn: {method}")
        if params.get("threadId") != thread_id:
            raise QualificationError("dynamic tool call used the wrong thread id")
        if params.get("tool") != TOOL_NAME:
            raise QualificationError(f"unexpected dynamic tool: {params.get('tool')!r}")
        arguments = parse_arguments(params.get("arguments"))
        if arguments != {"token": TOOL_ARGUMENT}:
            raise QualificationError(
                f"dynamic tool arguments differed: {json.dumps(arguments, sort_keys=True)}"
            )
        call_id = params.get("callId")
        if not isinstance(call_id, str) or not call_id:
            raise QualificationError("dynamic tool call omitted callId")
        calls.append(params)
        return {
            "success": True,
            "contentItems": [{"type": "inputText", "text": TOOL_RESULT}],
        }

    result = server.request(
        "turn/start",
        {
            "threadId": thread_id,
            "input": [
                {
                    "type": "text",
                    "text": (
                        f"Call `{TOOL_NAME}` exactly once with token `{TOOL_ARGUMENT}`. "
                        f"Do not call any other tool. After it returns, reply exactly `{TOOL_RESULT}`."
                    ),
                }
            ],
        },
        handler=handle_server_request,
        notifications=notifications,
    )
    turn_id = result.get("turn", {}).get("id")
    if not isinstance(turn_id, str) or not turn_id:
        raise QualificationError("turn/start omitted result.turn.id")

    terminal_status: str | None = None
    deadline = time.monotonic() + server.timeout
    while terminal_status is None:
        if notifications:
            message = notifications.pop(0)
        else:
            message = server.receive(deadline)
            if "method" not in message and "id" in message:
                server._pending[message["id"]] = message
                continue
            if "method" in message and "id" in message:
                server.dispatch(
                    message,
                    handler=handle_server_request,
                    notifications=notifications,
                )
                continue
        method = message.get("method")
        params = message.get("params", {})
        if not isinstance(method, str) or not isinstance(params, dict):
            raise QualificationError("malformed notification during Codex turn")
        message_turn_id = notification_turn_id(params)
        if message_turn_id is not None and message_turn_id != turn_id:
            continue
        if method == "item/agentMessage/delta":
            delta = params.get("delta")
            if isinstance(delta, str):
                assistant_text.append(delta)
        elif method in {"item/started", "item/completed"}:
            item = params.get("item")
            if not isinstance(item, dict):
                raise QualificationError(f"{method} omitted item")
            item_type = item.get("type", "")
            if item_type == "agentMessage" and method == "item/completed":
                text = item.get("text")
                if isinstance(text, str) and text not in assistant_text:
                    assistant_text.append(text)
            elif item_type == "dynamicToolCall":
                item_id = item.get("id")
                if not isinstance(item_id, str) or not item_id:
                    raise QualificationError(f"{method} dynamicToolCall omitted id")
                target = started if method == "item/started" else completed
                target[item_id] = item
            elif item_type not in IGNORED_ITEM_TYPES and method == "item/started":
                unexpected_items.append(str(item_type))
        elif method == "turn/completed":
            turn = params.get("turn")
            if not isinstance(turn, dict):
                raise QualificationError("turn/completed omitted turn")
            status = turn.get("status")
            terminal_status = status if isinstance(status, str) else ""

    if terminal_status != "completed":
        raise QualificationError(f"Codex turn ended with status {terminal_status!r}")
    if len(calls) != 1:
        raise QualificationError(f"expected one dynamic callback, observed {len(calls)}")
    call_id = calls[0]["callId"]
    if set(started) != {call_id} or set(completed) != {call_id}:
        raise QualificationError(
            "dynamic tool item lifecycle did not match the callback id "
            f"(started={sorted(started)}, completed={sorted(completed)}, call={call_id})"
        )
    completed_item = completed[call_id]
    if completed_item.get("status") != "completed" or completed_item.get("success") is not True:
        raise QualificationError(
            "completed dynamic tool item did not report successful completion"
        )
    if unexpected_items:
        raise QualificationError(
            f"Codex invoked unexpected built-in items: {sorted(set(unexpected_items))}"
        )
    rendered_text = "".join(assistant_text)
    if TOOL_RESULT not in rendered_text:
        raise QualificationError("assistant response did not contain the dynamic tool result")
    return {
        "turn": ordinal,
        "turn_id": turn_id,
        "tool_calls": len(calls),
        "item_started": True,
        "item_completed": True,
        "assistant_used_result": True,
        "built_in_tool_calls": 0,
    }


def resolve_codex(explicit: str | None) -> Path:
    candidates: list[str | Path | None] = [
        explicit,
        os.environ.get("TROUVE_CODEX_BIN"),
        shutil.which("codex"),
        DEFAULT_MANAGED_CODEX,
    ]
    for candidate in candidates:
        if candidate is None:
            continue
        path = Path(candidate).expanduser().resolve()
        if path.is_file() and os.access(path, os.X_OK):
            return path
    raise QualificationError(
        "Codex CLI not found; pass --codex or set TROUVE_CODEX_BIN"
    )


def codex_version(binary: Path) -> str:
    completed = subprocess.run(
        [str(binary), "--version"],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    )
    return (completed.stdout or completed.stderr).strip()


def qualify(args: argparse.Namespace) -> dict[str, Any]:
    binary = resolve_codex(args.codex)
    cwd = args.cwd.resolve()
    if not cwd.is_dir():
        raise QualificationError(f"working directory does not exist: {cwd}")
    model = args.model or os.environ.get("TROUVE_CODEX_QUALIFICATION_MODEL")
    thread_id: str | None = None
    active_server: CodexAppServer | None = None
    turns: list[dict[str, Any]] = []
    try:
        active_server = app_server(binary, cwd, args.timeout)
        thread_id = start_thread(active_server, cwd, model)
        turns.append(run_turn(active_server, thread_id, 1))

        # A new app-server proves the schema was persisted in the rollout, not
        # retained only in this process's in-memory thread cache.
        active_server.close()
        active_server = app_server(binary, cwd, args.timeout)
        resume_thread(active_server, thread_id, cwd, model)
        turns.append(run_turn(active_server, thread_id, 2))

        return {
            "candidate": "codex-dynamic-tools",
            "result": "pass",
            "codex_version": codex_version(binary),
            "experimental_api": True,
            "cold_resume": True,
            "exactly_once": True,
            "thread_id": thread_id if args.keep_thread else "deleted",
            "turns": turns,
        }
    finally:
        if active_server is not None:
            if thread_id is not None and not args.keep_thread:
                try:
                    active_server.request("thread/delete", {"threadId": thread_id})
                except BaseException as error:
                    print(f"warning: could not delete qualification thread: {error}", file=sys.stderr)
            active_server.close()


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", help="path to the Codex CLI")
    parser.add_argument(
        "--cwd",
        type=Path,
        default=REPOSITORY_ROOT,
        help="read-only working directory shown to the qualification agent",
    )
    parser.add_argument("--model", help="optional Codex model id")
    parser.add_argument(
        "--timeout",
        type=float,
        default=180.0,
        help="timeout in seconds for each app-server operation/turn",
    )
    parser.add_argument(
        "--keep-thread",
        action="store_true",
        help="keep and print the disposable Codex thread id",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        result = qualify(args)
    except (QualificationError, OSError, subprocess.SubprocessError) as error:
        print(f"Codex dynamic-tool qualification failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
