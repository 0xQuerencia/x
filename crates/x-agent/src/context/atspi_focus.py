"""Reads, and for edit ops changes, the focused text control of one app over AT-SPI.

Run as: python3 -I -c <this file> <pid> <max_text> [op] [json-params]. Prints exactly one
JSON object. Ops:
  selection (default)  the focused control and its selected text
  field                caret, selection range and the text around the caret
  replace              compare-and-replace a range: nothing changes unless the range
                       still holds exactly `expect`
  insert               insert text at an offset
  select               select a range
Password fields are reported but their text is never fetched or changed. Every op is
bounded in nodes, depth and time so a huge page cannot stall it.
"""
import json
import sys
import time

MAX_TEXT = int(sys.argv[2]) if len(sys.argv) > 2 else 8000
OP = sys.argv[3] if len(sys.argv) > 3 else "selection"
PARAMS = json.loads(sys.argv[4]) if len(sys.argv) > 4 else {}
MAX_NODES = 6000
MAX_DEPTH = 40
BUDGET = 2.0
FIELD_LIMIT = 200000  # most characters a field read will look at


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

base = dict(role=role, name=name, editable=editable, protected=password)


def selection_op():
    result = dict(base, selection=None, truncated=False, via_ancestor=False)
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
    return result


def need_text_field():
    if password:
        out(error="that field looks sensitive, so it was not touched", **base)
    try:
        count = Atspi.Text.get_character_count(node)
    except Exception:
        out(error="the focused control exposes no text", **base)
    if count < 0 or count > FIELD_LIMIT:
        out(error="the field is too large to check safely", **base)
    return count


def field_op():
    count = need_text_field()
    window = int(PARAMS.get("window", 400))
    caret = Atspi.Text.get_caret_offset(node)
    sel_start = sel_end = None
    try:
        if Atspi.Text.get_n_selections(node) > 0:
            r = Atspi.Text.get_selection(node, 0)
            if r.end_offset > r.start_offset:
                sel_start, sel_end = r.start_offset, r.end_offset
    except Exception:
        pass
    lo = max(0, caret - window)
    hi = min(count, caret + window)
    selected = None
    if sel_start is not None and sel_end - sel_start <= MAX_TEXT:
        selected = Atspi.Text.get_text(node, sel_start, sel_end)
    try:
        can_edit = node.get_editable_text_iface() is not None and editable
    except Exception:
        can_edit = False
    range_text = None
    if "range" in PARAMS:
        r0, r1 = int(PARAMS["range"][0]), int(PARAMS["range"][1])
        if 0 <= r0 <= r1 <= count and r1 - r0 <= FIELD_LIMIT:
            range_text = Atspi.Text.get_text(node, r0, r1) or ""
    return dict(
        base,
        range_text=range_text,
        count=count,
        caret=caret,
        sel_start=sel_start,
        sel_end=sel_end,
        selected=selected,
        before=Atspi.Text.get_text(node, lo, caret) or "",
        before_start=lo,
        after=Atspi.Text.get_text(node, caret, hi) or "",
        can_edit=can_edit,
    )


def editable_iface():
    if not editable:
        out(error="the focused control is not editable", **base)
    try:
        iface = node.get_editable_text_iface()
    except Exception:
        iface = None
    if iface is None:
        out(error="the focused control cannot be edited through accessibility", **base)
    return iface


def replace_op():
    count = need_text_field()
    iface = editable_iface()
    start, end = int(PARAMS["start"]), int(PARAMS["end"])
    expect, text = PARAMS["expect"], PARAMS["text"]
    if not (0 <= start <= end <= count):
        out(error="the range is outside the field", **base)
    if (Atspi.Text.get_text(node, start, end) or "") != expect:
        out(error="the text there has changed", changed=True, **base)
    if end > start and not Atspi.EditableText.delete_text(iface, start, end):
        out(error="the app refused the deletion", **base)
    if text and not Atspi.EditableText.insert_text(iface, start, text, len(text)):
        out(error="the app refused the insertion (the original text may be gone)", partial=True, **base)
    return dict(base, done=True)


def insert_op():
    count = need_text_field()
    iface = editable_iface()
    at, text = int(PARAMS["at"]), PARAMS["text"]
    if not (0 <= at <= count):
        out(error="the position is outside the field", **base)
    if not Atspi.EditableText.insert_text(iface, at, text, len(text)):
        out(error="the app refused the insertion", **base)
    return dict(base, done=True)


def select_op():
    need_text_field()
    start, end = int(PARAMS["start"]), int(PARAMS["end"])
    if not Atspi.Text.set_selection(node, 0, start, end):
        try:
            ok = Atspi.Text.add_selection(node, start, end)
        except Exception:
            ok = False
        if not ok:
            out(error="the app refused the selection", **base)
    return dict(base, done=True)


OPS = dict(selection=selection_op, field=field_op, replace=replace_op,
           insert=insert_op, select=select_op)
if OP not in OPS:
    out(error="unknown operation")
try:
    out(**OPS[OP]())
except SystemExit:
    raise
except Exception as e:
    out(error="the accessibility call failed (%s)" % type(e).__name__)
