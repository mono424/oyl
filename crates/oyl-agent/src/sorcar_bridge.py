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

#: What the Claude CLI streams in place of a native ``<invoke>`` tag a model
#: writes as text.  Sorcar can never parse it, and a model that starts
#: writing it tends to repeat it until the turn is stopped.
BROKEN_CALL_MARK = "<invoke_tool_placeholder>"
#: Broken calls tolerated in one turn before the bridge gives up on it.
MAX_BROKEN_CALLS = 8
BROKEN_CALLS_MESSAGE = (
    "Sorcar stopped the turn: the model kept writing tool calls in a form "
    "Sorcar cannot read. Try a stronger model or a higher effort."
)

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
            self._broken_calls = 0
            self._stream_tail = ""

        # -- streaming -------------------------------------------------
        def _count_broken_calls(self, token: str) -> str:
            """Count broken calls (split across tokens too) and return *token*
            without them; end the turn once the model is stuck on them."""
            combined = self._stream_tail + token
            self._broken_calls += combined.count(BROKEN_CALL_MARK)
            self._stream_tail = combined.rsplit(BROKEN_CALL_MARK, 1)[-1][-(len(BROKEN_CALL_MARK) - 1):]
            if self._broken_calls >= MAX_BROKEN_CALLS:
                emit({"type": "failed", "message": BROKEN_CALLS_MESSAGE})
                _exit(1)
            return token.replace(BROKEN_CALL_MARK, "").replace("</invoke_tool_placeholder>", "")

        def token_callback(self, token: str) -> None:
            if not token:
                return
            with self._lock:
                token = self._count_broken_calls(token)
                if not token:
                    return
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


#: Sorcar's specialised agents (SEAs) behind Oyl's capabilities.
CAPABILITY_SEAS = {
    "discover": "discover",
    "write_paper": "write_paper",
    "review": "review_paper",
}

#: Tools a capability run cannot use outside Sorcar's daemon (sub-task
#: dispatch, speech, the model picker).
NO_DAEMON_TOOLS = frozenset({
    "run_agent", "agent_job", "run_parallel", "number_of_cores", "talk", "set_model",
})

#: The worktree and rollback tools, left out of runs that edit the paper in
#: place.
WORKTREE_TOOLS = frozenset({"create_worktree", "commit_and_merge", "checkpoint", "rollback"})

DISCOVER_NOTE = """

# Oyl: discovery run
Run unattended: nobody can answer questions. There is no browser in this
run: search the state of the art with curl (arXiv API, Semantic Scholar,
GitHub search). Sub-agents (run_agent, run_parallel) are not available.
"""

STATE_POLL_SECONDS = 1.0


def _make_agent_class(oyl_scratch: bool = True):  # noqa: ANN202
    from kiss.agents.sorcar.chat_sorcar_agent import ChatSorcarAgent

    if not oyl_scratch:
        return ChatSorcarAgent

    class OylSorcarAgent(ChatSorcarAgent):
        """Sorcar with its scratch files kept out of the paper's sources."""

        def _scratch_dir(self) -> str:
            return f"./.oyl/sorcar/{self.last_task_id or 'scratch'}/"

    return OylSorcarAgent


def _pick_model(request: dict[str, Any]) -> str | None:
    """The model to run: the requested id when Sorcar's catalog has it,
    else its fallback (the same model without the effort level)."""
    model = request.get("model") or None
    fallback = request.get("model_fallback") or None
    if not model:
        return None
    try:
        from kiss.core.models.model_info import MODEL_INFO
    except Exception:  # noqa: BLE001 - let Sorcar report a bad name itself
        return model
    if model not in MODEL_INFO and fallback and fallback in MODEL_INFO:
        emit({"type": "status",
              "message": f"{model} is not in Sorcar's catalog; running {fallback} at its default effort."})
        return fallback
    return model


def _sea_run(name: str, task: str):  # noqa: ANN202
    """The configuration of a run of Sorcar's SEA *name* on *task*."""
    from kiss.agents.sorcar.sea_commands import evaluate_sea, get_command, sea_layers

    path = get_command(name)
    if path is None:
        raise RuntimeError(f"this Sorcar has no {name} agent")
    return evaluate_sea(sea_layers(path), task)


def _sea_kwargs(run: Any, in_place: bool) -> dict[str, Any]:
    """The run() arguments a SEA configuration contributes. The SEA's own
    model ("bestrouter", a daemon-side router) is not used: the run keeps
    the model the user picked."""
    drop = NO_DAEMON_TOOLS | (WORKTREE_TOOLS if in_place else frozenset())

    def tools_hook(tools: list[Any]) -> list[Any]:
        return [tool for tool in run.tools_hook(tools) if getattr(tool, "__name__", "") not in drop]

    def system_prompt_hook(prompt: str) -> str:
        prompt = run.system_prompt_hook(prompt)
        return _strip_worktree_rules(prompt) if in_place else prompt + DISCOVER_NOTE

    settings = run.settings or {}
    return dict(
        prompt_template=str(run.prompt),
        tools_hook=tools_hook,
        system_prompt_hook=system_prompt_hook,
        llm_call_hook=run.llm_call_hook,
        tool_call_hook=run.tool_call_hook,
        llm_response_hook=run.llm_response_hook,
        tool_profile=str(settings.get("tool_profile") or ""),
        compaction=bool(settings.get("compaction", True)),
    )


def _watch_state_dir(agent: Any, stop: threading.Event) -> None:
    """Report where a discovery run keeps its state files: the agent's
    scratch dir under its current work dir, which moves into a worktree."""
    last = None
    while not stop.wait(STATE_POLL_SECONDS):
        try:
            task_id = agent.last_task_id
            work_dir = getattr(agent, "work_dir", None)
            if not task_id or not work_dir:
                continue
            path = str((Path(work_dir) / agent._scratch_dir()).resolve())
        except Exception:  # noqa: BLE001 - the agent is mid-change; retry
            continue
        if path != last:
            last = path
            emit({"type": "state_dir", "path": path})


def _progress(phase: str, phases: list[str], step: int, total: int, fraction: float,
              metrics: list[tuple[str, str]], state_dir: str | None = None) -> None:
    emit({"type": "progress", "progress": {
        "phase": phase, "phases": phases, "step": step, "total": total,
        "fraction": max(0.0, min(1.0, fraction)),
        "metrics": [list(m) for m in metrics], "state_dir": state_dir,
    }})


def _result(result: str) -> tuple[bool, str]:
    import yaml

    try:
        parsed = yaml.safe_load(result)
    except Exception:  # noqa: BLE001
        parsed = None
    if isinstance(parsed, dict):
        return parsed.get("success") is True, _html_to_text(str(parsed.get("summary") or ""))
    return False, _html_to_text(str(result or ""))


class _Turn:
    """One turn's shared pieces: the printer, the callbacks, the request."""

    def __init__(self, request: dict[str, Any]) -> None:
        self.request = request
        self.printer = _make_printer()
        self.session = str(request.get("session") or "")
        self.model = _pick_model(request)

    def on_task_allocated(self, _task_id: str, chat_id: str) -> None:
        if chat_id:
            emit({"type": "session", "session": chat_id})

    @staticmethod
    def ask_user(question: str) -> str:
        emit({"type": "status", "message": "Sorcar asked: " + " ".join(str(question).split())[:300]})
        return ASK_USER_ANSWER

    def base_kwargs(self, work_dir: str) -> dict[str, Any]:
        request = self.request
        kwargs: dict[str, Any] = dict(
            model_name=self.model,
            work_dir=work_dir,
            printer=self.printer,
            web_tools=False,
            is_parallel=False,
            use_memory=False,
            current_editor_file=request.get("active_file") or None,
            attachments=_attachments(list(request.get("attachments") or [])) or None,
            ask_user_question_callback=self.ask_user,
            _history_prompt=str(request.get("history_prompt") or "") or None,
            _open_bare_path=False,
            _on_task_id_allocated=self.on_task_allocated,
        )
        if request.get("max_budget") is not None:
            kwargs["max_budget"] = float(request["max_budget"])
        return kwargs

    def agent(self, oyl_scratch: bool = True, resume: bool = True):  # noqa: ANN201
        agent = _make_agent_class(oyl_scratch)("Oyl")
        if resume and self.session:
            agent.resume_chat_by_id(self.session)
            emit({"type": "session", "session": self.session})
        return agent

    def run(self, agent: Any, kwargs: dict[str, Any]) -> str:
        try:
            from kiss.core.base import collecting_trajectories
        except ImportError:  # older releases: trajectories are just saved
            return agent.run(**kwargs)
        with collecting_trajectories(save=False):
            return agent.run(**kwargs)

    def finish(self, agent: Any, success: bool, summary: str) -> None:
        try:
            budget, _tokens, _steps = agent.usage_snapshot()
            self.printer.emit_usage(cost=float(budget))
        except Exception:  # noqa: BLE001 - usage is best effort
            traceback.print_exc()
        emit({"type": "completed", "success": success, "summary": summary,
              "session": agent.chat_id or self.session or None})


def run_turn(request: dict[str, Any]) -> None:
    from kiss.core._version import __version__

    emit({"type": "ready", "version": __version__})
    turn = _Turn(request)
    capability = str(request.get("capability") or "")
    if capability == "revise_and_review":
        _run_revise(turn)
        return
    work_dir = str(request["work_dir"])
    agent = turn.agent(oyl_scratch=capability != "discover")
    kwargs = turn.base_kwargs(work_dir)
    if capability:
        if capability not in CAPABILITY_SEAS:
            emit({"type": "failed", "message": f"Sorcar has no {capability!r} mode."})
            return
        run = _sea_run(CAPABILITY_SEAS[capability], str(request["prompt"]))
        kwargs.update(_sea_kwargs(run, in_place=capability != "discover"))
    else:
        def tools_hook(tools: list[Any]) -> list[Any]:
            return [tool for tool in tools if getattr(tool, "__name__", "") in ALLOWED_TOOLS]

        kwargs.update(prompt_template=str(request["prompt"]), tools_hook=tools_hook,
                      system_prompt_hook=_strip_worktree_rules)

    stop = threading.Event()
    if capability == "discover":
        threading.Thread(target=_watch_state_dir, args=(agent, stop), daemon=True).start()
    try:
        result = turn.run(agent, kwargs)
    except KeyboardInterrupt:
        stop.set()
        emit({"type": "completed", "success": False, "summary": "Stopped.",
              "session": agent.chat_id or turn.session or None})
        return
    stop.set()
    success, summary = _result(result)
    if capability == "review":
        output = Path(str(request.get("review_output") or ""))
        if output.is_file():
            review = output.read_text(encoding="utf-8", errors="replace").strip()
            emit({"type": "text_delta", "text": review})
            summary = review
        elif success:
            success = False
            summary = (summary + "\n\n" if summary else "") + f"The review was not written to {output}."
    turn.finish(agent, success, summary)


def _split_blocks(task: str) -> tuple[str, str]:
    """The writing and the review instructions of a revise-and-review task
    ("Writing: … Review: …"); without blocks the whole text is both."""
    import re

    match = re.search(r"(?im)^\s*(?:writing|write|paper)\s*:\s*(.*?)(?=^\s*(?:review|reviewing)\s*:|\Z)", task, re.S)
    review = re.search(r"(?im)^\s*(?:review|reviewing)\s*:\s*(.*)", task, re.S)
    writing_text = match.group(1).strip() if match else task.strip()
    review_text = review.group(1).strip() if review else (
        "Review it for the venue the writing instructions name, as a careful reviewer would.")
    return writing_text, review_text


def _run_revise(turn: _Turn) -> None:
    """Oyl's revise-and-review loop: the write_paper agent writes or revises,
    a fresh review_paper agent reviews, until the review says strong accept,
    the round cap, or two rounds without improvement (Sorcar's loop_status
    decides). Sorcar's own coordinator needs its daemon for sub-agents; this
    runs the same rounds one after the other."""
    from kiss.agents.seas.revise_and_review_paper import revise_and_review_paper_sea as rr

    request = turn.request
    writing, review_instructions = _split_blocks(str(request["prompt"]))
    max_rounds = int(request.get("max_rounds") or rr.DEFAULT_MAX_ROUNDS)
    work_dir = str(request["work_dir"])
    reports = Path(str(request.get("reports_dir") or Path(work_dir) / ".oyl" / "reviews" / "revise"))
    reports.mkdir(parents=True, exist_ok=True)
    main = Path(str(request.get("main_file") or ""))
    stem = main.stem or "paper"
    phases = ["Writing", "Reviewing"]
    verdict, review_path, status = "", "", ""
    writer = turn.agent(resume=True)
    success = False
    rounds_done = 0
    for round_number in range(1, max_rounds + 1):
        metrics = [("draft", str(round_number)), ("last verdict", verdict or "none yet")]
        _progress("Writing", phases, round_number, max_rounds, (round_number - 1) / max_rounds,
                  metrics, str(reports))
        try:
            kwargs = turn.base_kwargs(work_dir)
            kwargs.update(_sea_kwargs(_sea_run("write_paper", rr.writer_task(
                writing, round_number, review_path, "")), in_place=True))
            turn.run(writer, kwargs)
            paper = main.with_suffix(".pdf") if main.with_suffix(".pdf").is_file() else main
            output = reports / f"{stem}-round{round_number}-review.txt"
            _progress("Reviewing", phases, round_number, max_rounds, (round_number - 0.5) / max_rounds,
                      metrics, str(reports))
            reviewer = turn.agent(resume=False)
            kwargs = turn.base_kwargs(work_dir)
            kwargs.update(_sea_kwargs(_sea_run("review_paper", rr.reviewer_task(
                review_instructions, str(paper), str(output), round_number)), in_place=True))
            turn.run(reviewer, kwargs)
        except KeyboardInterrupt:
            emit({"type": "completed", "success": False, "summary": "Stopped.",
                  "session": writer.chat_id or turn.session or None})
            return
        rounds_done = round_number
        pattern = str(reports / f"{stem}-round*-review.txt")
        status = rr.loop_status(pattern, max_rounds)
        if "CHECK:" in status:
            status = rr.loop_status(pattern, max_rounds, overrides=f"{round_number}=borderline")
        if output.is_file():
            verdict = rr._verdict(output.read_text(encoding="utf-8", errors="replace")) or verdict
            review_path = str(output)
        emit({"type": "status", "message": f"Round {round_number}: {verdict or 'no verdict'}."})
        decision = status.strip().splitlines()[-1] if status.strip() else ""
        if decision.startswith("STOP") or not decision.startswith("CONTINUE"):
            success = verdict == rr.TARGET_VERDICT or decision.startswith("STOP")
            break
    _progress("Reviewing", phases, rounds_done, max_rounds, 1.0,
              [("draft", str(rounds_done)), ("last verdict", verdict or "none")], str(reports))
    summary = f"{status.strip()}\n\nReviews are in {reports}."
    turn.finish(writer, success, summary)


def main() -> int:
    if "--check" in sys.argv[1:]:
        from kiss.core._version import __version__

        import kiss.agents.sorcar.chat_sorcar_agent  # noqa: F401 - import check
        from kiss.agents.sorcar.sea_commands import get_command
        from kiss.agents.seas.revise_and_review_paper import revise_and_review_paper_sea as rr

        missing = [name for name in CAPABILITY_SEAS.values() if get_command(name) is None]
        missing += [name for name in ("writer_task", "reviewer_task", "loop_status", "_verdict")
                    if not hasattr(rr, name)]
        if missing:
            emit({"type": "failed", "message": "this Sorcar lacks " + ", ".join(missing)})
            return 1
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
