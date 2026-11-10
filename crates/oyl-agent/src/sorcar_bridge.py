"""Oyl's bridge to KISS Sorcar (https://github.com/ksenxx/kiss_ai).

Oyl runs this script once per agent turn with the interpreter of the
virtual environment it built for the ``kiss-agent-framework`` package:

    <data_dir>/sorcar/venv/bin/python -I -u sorcar_bridge.py

The request arrives as one JSON object on stdin.  Every event goes to
stdout as one JSON object per line; nothing else is ever written there
(KISS's own console output is redirected to stderr, which Oyl keeps the
tail of for error reports).  Events:

    {"type": "ready", "version": "..."}          kiss imported
    {"type": "session", "session": "..."}        chat id of this conversation
    {"type": "status", "message": "..."}
    {"type": "text_delta", "text": "..."}
    {"type": "thinking_delta", "text": "..."}
    {"type": "tool_started", "id": "...", "name": "...", "input": {...}}
    {"type": "tool_finished", "id": "...", "ok": true, "output": "..."}
    {"type": "usage", "cost_usd": 0.1, "input_tokens": 1, "output_tokens": 2}
    {"type": "completed", "success": true, "summary": "...", "session": "..."}
    {"type": "failed", "message": "..."}

Exactly one of ``completed`` / ``failed`` ends the stream.  SIGINT stops
the turn and still ends it with ``completed`` (``success: false``).

With ``--check`` the script only imports Sorcar and reports ``ready``.
"""

from __future__ import annotations

import json
import mimetypes
import os
import re
import sys
import threading
import traceback
from pathlib import Path
from typing import Any

# The protocol owns the real stdout.  File descriptor 1 is pointed at stderr
# so that nothing else -- KISS's console printers, a stray print(), a child
# process inheriting stdout -- can corrupt the event stream.
_OUT = os.fdopen(os.dup(1), "w", encoding="utf-8", buffering=1)
os.dup2(2, 1)
sys.stdout = sys.stderr
_OUT_LOCK = threading.Lock()

#: Longest string kept in a tool's input or output, in characters.
MAX_FIELD_CHARS = 20_000
#: Longest tool output forwarded to Oyl, in characters.
MAX_OUTPUT_CHARS = 8_000

#: The only built-in Sorcar tools an Oyl turn gets ("finish" is always
#: added by Sorcar).  Leaving out create_worktree / commit_and_merge also
#: turns off Sorcar's worktree write guard, so Edit and Write work on the
#: paper folder in place; checkpoint / rollback, sub-agents, browser,
#: memory, speech and model switching are left out as well.
ALLOWED_TOOLS = frozenset({
    "Bash", "bash_job", "run_commands_parallel", "Read", "Edit", "Write",
    "ask_user_question", "summary", "skill",
})

WORKTREE_OFF_NOTE = """

# Oyl: edit the paper in place
This run has no git worktree tools (create_worktree, commit_and_merge) and no
checkpoint / rollback: edit the files of the work dir in place with Edit and
Write. Oyl saves a version of the paper folder before and after every turn,
so the user can review and undo every change. Never run git commands that
change the repository (commit, checkout, reset, stash, branch, merge).
Your scratch directory (see Task Settings) is the only place under .oyl/
you may write; everything else under .oyl/ belongs to Oyl.
"""

ASK_USER_ANSWER = (
    "The user is not available to answer questions during this turn. "
    "Decide yourself: choose the most reasonable option, state the "
    "assumption in your final summary, and continue."
)


def emit(event: dict[str, Any]) -> None:
    """Write one protocol event to stdout."""
    line = json.dumps(event, ensure_ascii=False, default=str)
    with _OUT_LOCK:
        _OUT.write(line + "\n")
        _OUT.flush()


def _clip(text: str, limit: int) -> str:
    if len(text) <= limit:
        return text
    half = limit // 2
    return text[:half] + "\n… (truncated) …\n" + text[-half:]


def _json_safe(value: Any, depth: int = 0) -> Any:
    """A JSON-serialisable copy of *value*, long strings clipped."""
    if depth > 6:
        return str(value)
    if isinstance(value, str):
        return _clip(value, MAX_FIELD_CHARS)
    if value is None or isinstance(value, (bool, int, float)):
        return value
    if isinstance(value, dict):
        return {str(k): _json_safe(v, depth + 1) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_json_safe(v, depth + 1) for v in value]
    return _clip(str(value), MAX_FIELD_CHARS)


def _number(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    return None


def _strip_worktree_rules(prompt: str) -> str:
    """Drop the system-prompt bullets that mandate git worktrees."""
    kept = []
    for line in prompt.splitlines():
        stripped = line.lstrip()
        lowered = stripped.lower()
        if stripped.startswith("- ") and ("worktree" in lowered or "commit_and_merge" in lowered):
            continue
        kept.append(line)
    return "\n".join(kept) + WORKTREE_OFF_NOTE


def _html_to_text(html: str) -> str:
    """Sorcar summaries are HTML; Oyl shows Markdown-ish plain text."""
    try:
        from kiss.core.html_to_markdown import html_to_markdown

        return html_to_markdown(html).strip()
    except Exception:  # noqa: BLE001 - fall back to a plain tag strip
        text = re.sub(r"<(br|/p|/li|/h\d)\s*/?>", "\n", html, flags=re.I)
        text = re.sub(r"<[^<>]+>", "", text)
        return re.sub(r"\n{3,}", "\n\n", text).strip()


def _make_printer():  # noqa: ANN202 - the base class is imported lazily
    from kiss.core.printer import Printer

    class OylPrinter(Printer):
        """Maps Sorcar's printer callbacks to Oyl events."""

        def __init__(self) -> None:
            self._lock = threading.RLock()
            self._thinking = False
            self._text_emitted = False
            self._need_break = False
            self._pending: list[tuple[str, str]] = []
            self._next_id = 0
            self.cost_usd = 0.0
            self.input_tokens = 0
            self.output_tokens = 0
            # Sorcar publishes usage offsets here between sub-sessions.
            self.budget_offset = 0.0
            self.tokens_offset = 0
            self.steps_offset = 0

        # -- streaming -------------------------------------------------
        def token_callback(self, token: str) -> None:
            if not token:
                return
            with self._lock:
                if self._thinking:
                    emit({"type": "thinking_delta", "text": token})
                    return
                if self._need_break and self._text_emitted:
                    emit({"type": "text_delta", "text": "\n\n"})
                self._need_break = False
                self._text_emitted = True
                emit({"type": "text_delta", "text": token})

        def thinking_callback(self, is_start: bool) -> None:
            with self._lock:
                self._thinking = bool(is_start)

        # -- events ----------------------------------------------------
        def print(self, content: Any, type: str = "text", **kwargs: Any) -> None:  # noqa: A002
            with self._lock:
                handler = getattr(self, "_on_" + str(type), None)
                if handler is not None:
                    handler(content, kwargs)

        def _on_tool_call(self, content: Any, kwargs: dict[str, Any]) -> None:
            name = str(content)
            self._need_break = True
            if name == "finish":
                self._pending.append(("", name))
                return
            self._next_id += 1
            call_id = f"sorcar-{self._next_id}"
            self._pending.append((call_id, name))
            tool_input = kwargs.get("tool_input")
            emit({
                "type": "tool_started",
                "id": call_id,
                "name": name,
                "input": _json_safe(tool_input if isinstance(tool_input, dict) else {}),
            })

        def _on_tool_result(self, content: Any, kwargs: dict[str, Any]) -> None:
            name = str(kwargs.get("tool_name") or "")
            index = next(
                (i for i, (_, pending) in enumerate(self._pending) if pending == name),
                0 if self._pending else None,
            )
            if index is None:
                return
            call_id, _ = self._pending.pop(index)
            if not call_id:
                return
            emit({
                "type": "tool_finished",
                "id": call_id,
                "ok": not bool(kwargs.get("is_error")),
                "output": _clip(str(content or ""), MAX_OUTPUT_CHARS),
            })

        def _on_llm_call(self, _content: Any, kwargs: dict[str, Any]) -> None:
            self.input_tokens += int(
                (_number(kwargs.get("input_tokens")) or 0)
                + (_number(kwargs.get("cache_read")) or 0)
                + (_number(kwargs.get("cache_write")) or 0)
            )
            self.output_tokens += int(_number(kwargs.get("output_tokens")) or 0)
            self.cost_usd += _number(kwargs.get("cost")) or 0.0
            self.emit_usage()

        def _on_notification(self, content: Any, _kwargs: dict[str, Any]) -> None:
            self._status(content)

        def _on_text(self, content: Any, _kwargs: dict[str, Any]) -> None:
            self._status(content)

        def _on_system_prompt(self, content: Any, _kwargs: dict[str, Any]) -> None:
            # The run's system prompt is printed with this type; so are the
            # one-line notes on a model switch or a retry, which are worth
            # showing.
            text = str(content or "")
            if len(text) <= 240 and "\n" not in text.strip():
                self._status(text)

        def _status(self, content: Any) -> None:
            text = " ".join(str(content or "").split())
            if text:
                emit({"type": "status", "message": _clip(text, 500)})

        def emit_usage(self, cost: float | None = None) -> None:
            emit({
                "type": "usage",
                "cost_usd": self.cost_usd if cost is None else cost,
                "input_tokens": self.input_tokens or None,
                "output_tokens": self.output_tokens or None,
            })

    return OylPrinter()


def _attachments(paths: list[str]) -> list[Any]:
    from kiss.core.models.model import Attachment

    result = []
    for raw in paths:
        path = Path(raw)
        mime = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        if not (mime.startswith("image/") or mime == "application/pdf"):
            continue
        try:
            result.append(Attachment.from_bytes(path.read_bytes(), mime))
        except OSError as exc:
            emit({"type": "status", "message": f"Could not attach {path.name}: {exc}"})
    return result


def _make_agent_class():  # noqa: ANN202
    from kiss.agents.sorcar.chat_sorcar_agent import ChatSorcarAgent

    class OylSorcarAgent(ChatSorcarAgent):
        """Sorcar with its scratch files kept out of the paper's sources."""

        def _scratch_dir(self) -> str:
            return f"./.oyl/sorcar/{self.last_task_id or 'scratch'}/"

    return OylSorcarAgent


def run_turn(request: dict[str, Any]) -> None:
    import yaml

    from kiss.core._version import __version__

    try:
        from kiss.core.base import collecting_trajectories
    except ImportError:  # older releases: trajectories are just saved
        collecting_trajectories = None

    emit({"type": "ready", "version": __version__})

    printer = _make_printer()
    agent = _make_agent_class()("Oyl")
    session = str(request.get("session") or "")
    if session:
        agent.resume_chat_by_id(session)
        emit({"type": "session", "session": session})

    def on_task_allocated(_task_id: str, chat_id: str) -> None:
        if chat_id:
            emit({"type": "session", "session": chat_id})

    def ask_user(question: str) -> str:
        emit({"type": "status", "message": "Sorcar asked: " + " ".join(str(question).split())[:300]})
        return ASK_USER_ANSWER

    def tools_hook(tools: list[Any]) -> list[Any]:
        return [tool for tool in tools if getattr(tool, "__name__", "") in ALLOWED_TOOLS]

    kwargs: dict[str, Any] = dict(
        prompt_template=str(request["prompt"]),
        model_name=request.get("model") or None,
        work_dir=str(request["work_dir"]),
        printer=printer,
        web_tools=False,
        is_parallel=False,
        use_memory=False,
        current_editor_file=request.get("active_file") or None,
        attachments=_attachments(list(request.get("attachments") or [])) or None,
        ask_user_question_callback=ask_user,
        tools_hook=tools_hook,
        system_prompt_hook=_strip_worktree_rules,
        _history_prompt=str(request.get("history_prompt") or "") or None,
        _open_bare_path=False,
        _on_task_id_allocated=on_task_allocated,
    )
    if request.get("max_budget") is not None:
        kwargs["max_budget"] = float(request["max_budget"])

    try:
        if collecting_trajectories is not None:
            with collecting_trajectories(save=False):
                result = agent.run(**kwargs)
        else:
            result = agent.run(**kwargs)
    except KeyboardInterrupt:
        emit({"type": "completed", "success": False, "summary": "Stopped.",
              "session": agent.chat_id or session or None})
        return

    try:
        budget, _tokens, _steps = agent.usage_snapshot()
        printer.emit_usage(cost=float(budget))
    except Exception:  # noqa: BLE001 - usage is best effort
        traceback.print_exc()

    try:
        parsed = yaml.safe_load(result)
    except Exception:  # noqa: BLE001
        parsed = None
    if isinstance(parsed, dict):
        success = parsed.get("success") is True
        summary = _html_to_text(str(parsed.get("summary") or ""))
    else:
        success = False
        summary = _html_to_text(str(result or ""))
    emit({
        "type": "completed",
        "success": success,
        "summary": summary,
        "session": agent.chat_id or session or None,
    })


def main() -> int:
    if "--check" in sys.argv[1:]:
        from kiss.core._version import __version__

        import kiss.agents.sorcar.chat_sorcar_agent  # noqa: F401 - import check

        emit({"type": "ready", "version": __version__})
        return 0
    try:
        request = json.loads(sys.stdin.read())
        if not isinstance(request, dict) or "prompt" not in request or "work_dir" not in request:
            raise ValueError("the request needs 'prompt' and 'work_dir'")
    except (ValueError, json.JSONDecodeError) as exc:
        emit({"type": "failed", "message": f"Bad request from Oyl: {exc}"})
        return 2
    try:
        run_turn(request)
    except KeyboardInterrupt:
        emit({"type": "completed", "success": False, "summary": "Stopped.",
              "session": request.get("session") or None})
    except BaseException as exc:  # noqa: BLE001 - report every failure
        traceback.print_exc()
        emit({"type": "failed", "message": f"Sorcar failed: {type(exc).__name__}: {exc}"})
        return 1
    return 0


def _exit(code: int) -> None:
    """Leave at once: the turn is over and Sorcar has saved its chat history.

    Sorcar keeps non-daemon helper threads alive; a normal interpreter
    shutdown would wait for them and keep the turn's process around.
    """
    try:
        _OUT.flush()
        sys.stderr.flush()
    finally:
        os._exit(code)


if __name__ == "__main__":
    _exit(main())
