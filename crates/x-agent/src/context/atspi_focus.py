"""Reads the focused control and its text selection from one app over AT-SPI.

Run as: python3 -I -c <this file> <pid>. Prints exactly one JSON object. Read-only: it
never sets focus, selection or text. Password fields are reported but their text is never
fetched. Bounded in nodes, depth and time so a huge page cannot stall it.
"""
import json
import sys
import time

MAX_TEXT = int(sys.argv[2]) if len(sys.argv) > 2 else 8000
MAX_NODES = 6000
MAX_DEPTH = 40
BUDGET = 2.0


def out(**kw):
    print(json.dumps(kw))
    sys.exit(0)


try:
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
except Exception as e:  # missing python-gobject or at-spi2-core typelib
    out(error="AT-SPI bindings are not installed (%s)" % type(e).__name__)

pid = int(sys.argv[1])
if pid == 0:
    out(probe=True)
deadline = time.monotonic() + BUDGET

try:
    desktop = Atspi.get_desktop(0)
    apps = []
    for i in range(desktop.get_child_count()):
        try:
            app = desktop.get_child_at_index(i)
            if app is not None and app.get_process_id() == pid:
                apps.append(app)
        except Exception:
            continue
except Exception as e:
    out(error="the accessibility bus is not reachable (%s)" % type(e).__name__)

if not apps:
    out(error="the focused app does not expose accessibility")

SHOWING = Atspi.StateType.SHOWING
FOCUSED = Atspi.StateType.FOCUSED
EDITABLE = Atspi.StateType.EDITABLE

# nodes[i] = (accessible, parent index, depth); depth-first, children in order.
nodes = []
stack = [(app, -1, 0) for app in reversed(apps)]
focused = -1
while stack and len(nodes) < MAX_NODES and time.monotonic() < deadline:
    node, parent, depth = stack.pop()
    try:
        states = node.get_state_set()
    except Exception:
        continue
    if depth > 0 and not states.contains(SHOWING):
        continue
    idx = len(nodes)
    nodes.append((node, parent, depth))
    if states.contains(FOCUSED) and depth > 0:
        focused = idx  # the last (deepest) focused node wins
    if depth >= MAX_DEPTH:
        continue
    try:
        count = node.get_child_count()
        for c in range(count - 1, -1, -1):
            child = node.get_child_at_index(c)
            if child is not None:
                stack.append((child, idx, depth + 1))
    except Exception:
        continue

if focused < 0:
    out(error="no focused control was found")


def text_selection(node):
    """The selected text of `node`, or None. Never called on password fields."""
    try:
        n = Atspi.Text.get_n_selections(node)
    except Exception:
        return None, False
    if n <= 0:
        return None, False
    parts = []
    total = 0
    truncated = False
    for i in range(n):
        try:
            r = Atspi.Text.get_selection(node, i)
            start, end = r.start_offset, r.end_offset
        except Exception:
            continue
        if end <= start:
            continue
        take = min(end, start + (MAX_TEXT - total) + 1)
        piece = Atspi.Text.get_text(node, start, take) or ""
        if take < end or total + len(piece) > MAX_TEXT:
            truncated = True
            piece = piece[: max(0, MAX_TEXT - total)]
        parts.append(piece)
        total += len(piece)
        if truncated:
            break
    text = "\n".join(parts)
    return (text if text.strip() else None), truncated


node, parent, _ = nodes[focused]
try:
    role = node.get_role_name() or ""
    name = (node.get_name() or "")[:120]
    password = node.get_role() == Atspi.Role.PASSWORD_TEXT
    editable = node.get_state_set().contains(EDITABLE)
except Exception as e:
    out(error="the focused control could not be read (%s)" % type(e).__name__)

result = dict(role=role, name=name, editable=editable, protected=password,
              selection=None, truncated=False, via_ancestor=False)
if not password:
    sel, trunc = text_selection(node)
    via = False
    hop = parent
    hops = 0
    while sel is None and hop >= 0 and hops < 3:
        anc = nodes[hop][0]
        try:
            if anc.get_role() == Atspi.Role.PASSWORD_TEXT:
                break
        except Exception:
            break
        sel, trunc = text_selection(anc)
        via = sel is not None
        hop = nodes[hop][1]
        hops += 1
    result.update(selection=sel, truncated=trunc, via_ancestor=via)
out(**result)
