# Builds recorder copies of Service.qml and Panel.qml in OUT; the repo copies are left untouched.
# Usage: python3 plugin/tests/replays/record.py plugin OUT
import os, sys
src, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
def rec(expr):
    return 'console.log("fleetpods-replay " + JSON.stringify(' + expr + '))'
def patch(name, pairs, base=None):
    s = open(os.path.join(base or src, name)).read()
    for a, b in pairs:
        assert s.count(a) == 1, (name, a[:60])
        s = s.replace(a, b, 1)
    open(os.path.join(out, name), 'w').write(s)
patch('Service.qml', [
  ('  function applyLine(raw) {\n', '  function applyLine(raw) {\n    ' + rec('{ t: Date.now(), ev: "status", raw: raw }') + '\n'),
  ('  function _send(verb, field, optimistic) {\n', '  function _send(verb, field, optimistic) {\n    ' + rec('{ t: Date.now(), ev: "send", verb: verb }') + '\n'),
  ('    activityProcess.running = true\n', '    ' + rec('{ t: Date.now(), ev: "activity" }') + '\n    activityProcess.running = true\n'),
])
patch('Service.qml', [
  ('        root.stepStatusLog({ ev: "reply", error: Model.elideError(commandErr.text || "fleetpods rejected the command") })\n',
   '        root.stepStatusLog({ ev: "reply", error: Model.elideError(commandErr.text || "fleetpods rejected the command") })\n        '
   + rec('{ t: Date.now(), ev: "reply", error: Model.elideError(commandErr.text || "fleetpods rejected the command") }') + '\n'),
], base=out)
patch('Panel.qml', [
  ('  function beginOmarchyClaim(origin) {\n', '  function beginOmarchyClaim(origin) {\n    ' + rec('{ t: Date.now(), ev: "click", origin: origin }') + '\n'),
])
# Row changes come from the status log the service steps; each is recorded as the old console's append, rewrite,
# close or clear, so replay.test.js reads any recording the same way.
patch('Service.qml', [
  ('    statusLogState = Model.statusLog(statusLogState, event)\n',
   '    var before = statusLogState.rows\n    statusLogState = Model.statusLog(statusLogState, event)\n'
   '    var after = statusLogState.rows, first = after.length ? after[0].id : Infinity\n'
   '    if (before.length && (!after.length || before[0].id < first && before[before.length - 1].id < first)) '
   + rec('{ t: event.t, ev: "clear" }') + '\n'
   '    for (var r = 0; r < after.length; ++r) {\n'
   '      var was = before.filter(function (b) { return b.id === after[r].id })[0]\n'
   '      if (!was) ' + rec('{ t: event.t, ev: "row", kind: after[r].kind, message: after[r].message, live: after[r].live }') + '\n'
   '      else if (was.message !== after[r].message) ' + rec('{ t: event.t, ev: "rewrite", index: r, message: after[r].message, live: after[r].live }') + '\n'
   '      else if (was.live && !after[r].live) ' + rec('{ t: event.t, ev: "close", index: r }') + '\n'
   '    }\n'),
], base=out)
print("ok")
