import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "BatteryIndicator"
  when: windowShown
  Component { id: indicator; Pods.BatteryIndicator { width: 64; height: implicitHeight } }
  Component { id: seats; Pods.SeatsView { width: 440; height: implicitHeight } }
  function test_healthyChargeStaysGreenAcrossThemes() {
    var item = createTemporaryObject(indicator, this, { level: 80 })
    compare(item.fillColor, Qt.hsla(0.4, 0.6, item.foreground.hslLightness, 1))
    for (var foreground of ["#000000", "#242424", "#ffffff"]) {
      item.foreground = foreground
      verify(item.fillColor.g > item.fillColor.r + 0.1)
      verify(item.fillColor.g > item.fillColor.b + 0.1)
      verify(item.fillColor.hslLightness >= 0.39)
      verify(item.fillColor.hslLightness <= 0.81)
      item.level = 9
      compare(item.fillColor, item.lowColor)
      item.level = 80
    }
  }
  function test_onlyLightThemesTintTheEmptyBand() {
    var item = createTemporaryObject(indicator, this, { level: 60 })
    for (var theme of [{ foreground: "#000000", tinted: true }, { foreground: "#242424", tinted: true },
                       { foreground: "#a9b1d6", tinted: false }, { foreground: "#ffffff", tinted: false }]) {
      item.foreground = theme.foreground
      compare(item.tintedTrack, theme.tinted, theme.foreground)
    }
  }
  function test_caseTransitionKeepsLayout_data() {
    return [{ tag: "left pod in case", state: "open-left" },
      { tag: "right pod in case", state: "open-right" }]
  }
  function test_caseTransitionKeepsLayout(data) {
    failOnWarning(/.*Cannot open.*/)
    var item = createTemporaryObject(seats, this, {
      podsVariant: "pro", caseState: data.state,
      leftBatteryLevel: 82, rightBatteryLevel: 8, caseBatteryLevel: 63
    })
    wait(50)
    var row = findChild(item, "podBatteryIndicators")
    var caseIndicator = findChild(item, "caseBatteryIndicator")
    var initial = [item.implicitHeight, row.y, caseIndicator.y]
    item.caseState = ""
    for (var elapsed = 0; elapsed < 1000; elapsed += 50) {
      wait(50)
      compare([item.implicitHeight, row.y, caseIndicator.y], initial)
    }
    item.caseState = data.state
    wait(500)
    compare([item.implicitHeight, row.y, caseIndicator.y], initial)
  }
  function test_unknownCaseAsksForADockedBud_data() {
    return [
      { tag: "both buds out", left: 75, right: 75, level: -1, caption: "Dock a bud" },
      { tag: "case reported", left: 75, right: 75, level: 40, caption: "CASE 40%" },
      { tag: "one bud unreported", left: 75, right: -1, level: -1, caption: "CASE --" }
    ]
  }
  function test_unknownCaseAsksForADockedBud(data) {
    var item = createTemporaryObject(seats, this, {
      podsVariant: "pro", leftBatteryLevel: data.left, rightBatteryLevel: data.right, caseBatteryLevel: data.level
    })
    compare(findChild(item, "caseBatteryLevel").caption, data.caption)
  }
  function test_quietCaseHidesTheRings() {
    var item = createTemporaryObject(seats, this, {
      podsVariant: "pro", leftBatteryLevel: -1, rightBatteryLevel: -1, caseBatteryLevel: -1
    })
    compare(item.showBattery, false)
    var closed = item.implicitHeight
    item.leftBatteryLevel = 30
    item.rightBatteryLevel = 38
    compare(item.showBattery, true)
    compare(findChild(item, "leftBatteryIndicator").caption, "L 30%")
    compare(findChild(item, "rightBatteryIndicator").caption, "R 38%")
    verify(item.implicitHeight > closed)
  }

  function test_lowBatteryBoundary_data() {
    return [
      { tag: "unknown", level: -1, fraction: 0, low: false },
      { tag: "empty", level: 0, fraction: 0, low: true },
      { tag: "below ten", level: 9, fraction: 0.09, low: true },
      { tag: "ten", level: 10, fraction: 0.1, low: false },
      { tag: "full", level: 100, fraction: 1, low: false }
    ]
  }
  function test_lowBatteryBoundary(data) {
    var item = createTemporaryObject(indicator, this, { level: data.level })
    compare(item.fraction, data.fraction)
    compare(item.known, data.level >= 0)
    compare(item.fillColor, data.low ? item.lowColor : item.healthyColor)
  }
}
