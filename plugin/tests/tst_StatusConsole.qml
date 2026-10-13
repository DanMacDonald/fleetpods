import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "StatusConsoleAttempt"
  when: windowShown
  visible: true
  width: 600
  height: 300

  ListModel { id: rows }

  Component {
    id: consoleView
    Pods.StatusConsole {
      width: 600
      height: implicitHeight
      messages: rows
      foreground: "white"
      urgent: "red"
      fontFamily: "monospace"
    }
  }

  function append(row) {
    row.device = row.device || ""
    row.retryPeer = row.retryPeer || ""
    row.retryAt = row.retryAt || 0
    row.eventKind = row.eventKind || ""
    rows.append(row)
  }

  function lineTexts(item) {
    var result = []
    var stack = [item]
    while (stack.length) {
      var node = stack.pop()
      if (node.objectName === "statusLine") result.push(node.text)
      for (var i = 0; i < node.children.length; ++i) stack.push(node.children[i])
      if (node.contentItem) stack.push(node.contentItem)
    }
    return result
  }

  // 2026-10-12: device names stand apart from the rest of a log line.
  function test_deviceNamesAreColoured() {
    rows.clear()
    append({ time: "16:53:43", message: "Omarchy Fleet connection switched to omarchy-xps.", kind: "status",
      live: false, detail: "", eventKind: "fleetSwitched" })
    var item = createTemporaryObject(consoleView, this, { deviceNames: ["omarchy-xps", "lg-gram"], nameColor: "#7aa2f7" })
    item.expanded = true
    wait(50)
    var lines = lineTexts(item)
    verify(lines.length > 0)
    for (var i = 0; i < lines.length; ++i)
      compare(lines[i], 'Omarchy Fleet connection switched to <font color="#7aa2f7">omarchy-xps</font>.')
  }

  function test_fleetRetryCountsDownWithoutReplayingTheEntrance() {
    rows.clear()
    var now = Date.now()
    append({ time: "16:00:00", message: "Waiting for laptop-a.", kind: "connection", live: true, detail: "",
      retryPeer: "laptop-a", retryAt: now + 3000 })
    var item = createTemporaryObject(consoleView, this)
    var label = findChild(item, "latestStatus")
    item.nowMs = now
    item.dots = 1
    compare(label.text, "Waiting for laptop-a. Retrying in 3s.  ")
    item.nowMs = now + 1000
    item.dots = 2
    compare(label.text, "Waiting for laptop-a. Retrying in 2s.. ")
    compare(label.entranceProgress, 1)
    item.nowMs = now + 3000
    item.dots = 3
    compare(label.text, "Retrying AirPods connection...")
    item.expanded = true
    verify(lineTexts(item).indexOf("Retrying AirPods connection...") >= 0)
    rows.setProperty(0, "live", false)
    rows.setProperty(0, "eventKind", "connected")
    rows.setProperty(0, "message", "Connecting to AirPods... Connected!")
    compare(label.text, "Connected to AirPods")
    compare(findChild(item, "headerSymbol").stateName, "success")
  }

  function test_disconnectWaitAnimatesAndBecomesConnectingInPlace() {
    rows.clear()
    append({ time: "16:00:00", message: "Waiting for lg-gram to disconnect...", kind: "connection",
      live: true, detail: "", eventKind: "waitingDisconnect" })
    var item = createTemporaryObject(consoleView, this)
    var label = findChild(item, "latestStatus")
    item.dots = 1
    compare(label.text, "Waiting for lg-gram to disconnect.")
    compare(findChild(item, "headerSymbol").stateName, "retry")
    item.dots = 3
    compare(label.text, "Waiting for lg-gram to disconnect...")
    rows.setProperty(0, "device", "omarchy-xps")
    rows.setProperty(0, "eventKind", "connecting")
    rows.setProperty(0, "message", "Connecting to AirPods...")
    compare(label.text, "Connecting omarchy-xps to AirPods...")
    compare(rows.count, 1)
  }

  function test_joinShowsTwoShips() {
    rows.clear()
    append({ time: "16:00:00", message: "laptop-b connected alongside laptop-a.", kind: "status", live: false, detail: "",
      eventKind: "pairedAlongside" })
    var item = createTemporaryObject(consoleView, this)
    compare(findChild(item, "headerSymbol").stateName, "fleetPair")
    item.expanded = true
    compare(item.messageState(rows.get(0)), "fleetPair")
  }

  // The console reads a row's event kind, so rewording a row cannot change its icon.
  function test_iconFollowsTheEventKindNotTheWords() {
    rows.clear()
    append({ time: "16:00:00", message: "Reworded fleet switch", kind: "status", live: false, detail: "", eventKind: "fleetSwitched" })
    append({ time: "16:00:01", message: "Omarchy Fleet connection switched to laptop-a.", kind: "status", live: false, detail: "" })
    var item = createTemporaryObject(consoleView, this)
    compare(item.messageState(rows.get(0)), "fleet")
    compare(item.messageState(rows.get(1)), "idle")
  }

  function test_audioRecoveryShowsSuccess() {
    rows.clear()
    append({ time: "16:00:00", message: "test-laptop audio connection restored.", kind: "success", live: false, detail: "" })
    var item = createTemporaryObject(consoleView, this)
    compare(findChild(item, "headerSymbol").stateName, "success")
    verify(lineTexts(item).indexOf("test-laptop audio connection restored.") >= 0)
  }

  function test_liveAttemptCyclesItsDots() {
    rows.clear()
    append({ time: "16:00:00", message: "Connecting to AirPods... (Retrying 1 of 2)", kind: "connection", live: true,
      detail: "(Retrying 1 of 2)" })
    append({ time: "16:00:01", message: "Connecting to AirPods... Connected!", kind: "connection", live: false, detail: "",
      eventKind: "connected" })
    var item = createTemporaryObject(consoleView, this, { expanded: true })
    item.dots = 1
    tryCompare(item, "dots", 1)
    var texts = lineTexts(item)
    verify(texts.indexOf("Connecting to AirPods.&nbsp;&nbsp;&nbsp;(Retrying 1 of 2)") >= 0, JSON.stringify(texts))
    verify(texts.indexOf("Connected to AirPods") >= 0, JSON.stringify(texts))
    item.dots = 2
    verify(lineTexts(item).indexOf("Connecting to AirPods..&nbsp;&nbsp;(Retrying 1 of 2)") >= 0)
  }

  function test_activeConnectionIconSpinsAndStops() {
    rows.clear()
    append({ time: "16:00:00", message: "Connecting to AirPods...", kind: "connection", live: true, detail: "" })
    var item = createTemporaryObject(consoleView, this)
    var symbol = findChild(item, "headerSymbol")
    wait(100)
    verify(symbol.rotation > 0, "The active connection icon should rotate")
    var angle = symbol.rotation
    wait(100)
    verify(symbol.rotation !== angle)
    rows.setProperty(0, "live", false)
    rows.setProperty(0, "eventKind", "connected")
    rows.setProperty(0, "message", "Connecting to AirPods... Connected!")
    tryCompare(symbol, "rotation", 0)
    wait(100)
    compare(symbol.rotation, 0)
    append({ time: "16:00:01", message: "Trying to connect again...", kind: "connection", live: false, detail: "",
      eventKind: "connectingAgain" })
    wait(100)
    compare(symbol.rotation, 0)
  }

  function test_activeHistoryIconSpinsAndStops() {
    rows.clear()
    append({ time: "16:00:00", message: "Connecting to AirPods...", kind: "connection", live: true,
      detail: "", eventKind: "connecting" })
    var item = createTemporaryObject(consoleView, this, { expanded: true })
    var log = findChild(item, "statusMessages")
    tryVerify(function () { return log.itemAtIndex(0) !== null })
    var symbol = findChild(log.itemAtIndex(0), "statusSymbol")
    verify(symbol)
    wait(100)
    verify(symbol.rotation > 0, "The live connection in recent activity should rotate")
    var angle = symbol.rotation
    wait(100)
    verify(symbol.rotation !== angle)
    rows.setProperty(0, "live", false)
    rows.setProperty(0, "eventKind", "connected")
    tryCompare(symbol, "rotation", 0)
    wait(100)
    compare(symbol.rotation, 0, "Completed history must stay still")
  }

  function test_newMessageEntersWithoutReplayingOnDotsOrExpand() {
    rows.clear()
    append({ time: "16:00:00", message: "AirPods audio moved to Laptop.", kind: "audio", live: false, detail: "", eventKind: "audioTaken" })
    var item = createTemporaryObject(consoleView, this)
    var latest = findChild(item, "latestStatus")
    wait(50)
    var restingY = latest.mapToItem(item, 0, 0).y
    append({ time: "16:00:01", message: "AirPods audio moved to iPhone.", kind: "audio", live: false, detail: "", eventKind: "audioTaken" })
    compare(latest.text, "AirPods audio moved to iPhone.")
    verify(latest.opacity < 1, "A changed message should fade in")
    verify(latest.mapToItem(item, 0, 0).y > restingY, "A changed message should slide up into place")
    tryCompare(latest, "opacity", 1)
    compare(latest.mapToItem(item, 0, 0).y, restingY)
    append({ time: "16:00:02", message: "Connecting to AirPods...", kind: "connection", live: true, detail: "" })
    tryCompare(latest, "opacity", 1)
    item.dots = 1
    compare(latest.opacity, 1)
    item.expanded = true
    wait(50)
    compare(latest.opacity, 1)
    compare(latest.mapToItem(item, 0, 0).y, restingY)
  }

  function test_collapsedLatestAndToggle() {
    rows.clear()
    append({ time: "16:00:00", message: "Older message", kind: "connection", live: false, detail: "" })
    append({ time: "16:00:01", message: "Latest message", kind: "error", live: false, detail: "" })
    var item = createTemporaryObject(consoleView, this)
    compare(item.expanded, false)
    var latest = findChild(item, "latestStatus")
    compare(latest.text, "Latest message")
    compare(latest.color, "#ff0000")
    compare(findChild(item, "statusMessages").visible, false)
    var collapsedHeight = item.height
    mouseClick(findChild(item, "statusToggle"))
    compare(item.expanded, true)
    verify(item.height > collapsedHeight)
    compare(findChild(item, "statusMessages").visible, true)
    tryVerify(function () { return lineTexts(item).indexOf("Older message") >= 0 })
    compare(latest.visible, true)
    compare(findChild(item, "statusEventCount").text, "2 events")
    var log = findChild(item, "statusMessages")
    compare(findChild(log.itemAtIndex(0), "statusLine").text, "Latest message")
    compare(findChild(log.itemAtIndex(0), "statusTimestamp").text, "16:00:01")
    mouseClick(findChild(item, "statusToggle"))
    compare(item.expanded, false)
    compare(item.height, collapsedHeight)
  }

  function test_caseGuidanceReplacesLogAtCollapsedHeight() {
    rows.clear()
    append({ time: "16:00:00", message: "Connected to AirPods", kind: "connection", live: false, detail: "" })
    var item = createTemporaryObject(consoleView, this, { width: 440 })
    var collapsedHeight = item.height
    item.expanded = true
    verify(item.height > collapsedHeight)
    item.guidanceText = "Open the lid on the AirPods case to allow Bluetooth connections to the pods."
    compare(item.height, collapsedHeight)
    compare(item.expanded, false)
    verify(!findChild(item, "statusToggle").visible)
    verify(!findChild(item, "statusMessages").visible)
    var guidance = findChild(item, "statusGuidance")
    verify(guidance.visible)
    compare(guidance.text, item.guidanceText)
    verify(guidance.height <= item.height)
    verify(guidance.y >= 0)
    verify(guidance.y + guidance.height <= item.height)
    item.guidanceText = ""
    compare(item.height, collapsedHeight)
    verify(!guidance.visible)
    verify(findChild(item, "statusToggle").visible)
  }

  function test_headerIconStaysBesideMainLine() {
    rows.clear()
    append({ time: "16:00:00", message: "AirPods audio moved to Laptop.", kind: "audio", live: false, detail: "", eventKind: "audioTaken" })
    var item = createTemporaryObject(consoleView, this, { playbackSummary: "Laptop · Playing" })
    var symbol = findChild(item, "headerSymbol")
    var latest = findChild(item, "latestStatus")
    function aligned() {
      return Math.abs(symbol.mapToItem(item, 0, symbol.height / 2).y
        - latest.mapToItem(item, 0, latest.height / 2).y) < 0.5
    }
    tryVerify(aligned)
    wait(50)
    var collapsedY = latest.mapToItem(item, 0, 0).y
    mouseClick(findChild(item, "statusToggle"))
    compare(item.expanded, true)
    wait(50)
    tryVerify(aligned)
    compare(latest.mapToItem(item, 0, 0).y, collapsedY)
    mouseClick(findChild(item, "statusToggle"))
    compare(item.expanded, false)
    wait(50)
    tryVerify(aligned)
    compare(latest.mapToItem(item, 0, 0).y, collapsedY)
  }

  function test_scrollMovesPromptlyAndKeepsHistoryPosition() {
    rows.clear()
    for (var i = 0; i < 30; ++i)
      append({ time: "16:00:00", message: "Message " + i, kind: "connection", live: false, detail: "" })
    var item = createTemporaryObject(consoleView, this, { expanded: true })
    var log = findChild(item, "statusMessages")
    wait(50)
    var start = log.contentY
    mouseWheel(log, log.width / 2, log.height / 2, 0, -120, Qt.NoButton)
    verify(log.contentY - start >= 70, "Scroll should move immediately by a larger step")
    compare(log.followLatest, false)
    var scrolled = log.contentY
    append({ time: "16:00:01", message: "New message", kind: "connection", live: false, detail: "" })
    wait(50)
    compare(log.contentY, scrolled)
    mouseWheel(log, log.width / 2, log.height / 2, 0, 12000, Qt.NoButton)
    tryCompare(log, "atYBeginning", true)
    compare(log.followLatest, true)
  }

  function test_collapsedLiveUpdates() {
    rows.clear()
    var item = createTemporaryObject(consoleView, this)
    var latest = findChild(item, "latestStatus")
    verify(latest !== null)
    compare(latest.text, "No recent activity")
    append({ time: "16:00:00", message: "Connecting", kind: "connection", live: true, detail: "(Retrying)" })
    item.dots = 1
    compare(latest.text, "Connecting to AirPods.&nbsp;&nbsp;&nbsp;(Retrying)")
    var lineWidth = latest.contentWidth
    item.dots = 2
    compare(latest.contentWidth, lineWidth)
    item.dots = 3
    compare(latest.contentWidth, lineWidth)
    rows.setProperty(0, "live", false)
    rows.setProperty(0, "message", "Connected!")
    compare(latest.text, "Connected!")
    append({ time: "16:00:01", message: "New message", kind: "connection", live: false, detail: "" })
    compare(latest.text, "New message")
    rows.clear()
    compare(latest.text, "No recent activity")
  }
}
