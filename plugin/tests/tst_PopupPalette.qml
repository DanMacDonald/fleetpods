import QtQuick
import QtTest
import qs.Commons as Commons
import ".." as Pods

TestCase {
  name: "PopupPalette"
  when: windowShown
  visible: true
  width: 200
  height: 200

  Component { id: palette; Pods.PopupPalette {} }
  Component { id: artwork; Pods.PixelPods { caseState: "open-right" } }

  function test_pairingLightFollowsObservedMode() {
    var item = createTemporaryObject(artwork, this, {
      width: 128, height: 128, foregroundColor: "#a9b1d6", backgroundColor: "#1a1b26",
      pairingMode: false, caseState: "open"
    })
    var light = findChild(item, "pairingLight")
    verify(!light.visible)
    item.pairingMode = true
    verify(light.visible)
    var initial = light.opacity
    tryVerify(function() { return Math.abs(light.opacity - initial) > 0.1 }, 1500)
    tryVerify(function() { return light.opacity < 0.01 }, 2500)
    item.pairingMode = false
    verify(!light.visible)
    item.pairingMode = true
    item.caseState = "closed"
    verify(!light.visible)
    item.caseState = ""
    verify(!light.visible)
  }

  function test_caseBackgroundMatchesPopup() {
    var item = createTemporaryObject(artwork, this, {
      foregroundColor: Commons.Color.foreground, backgroundColor: Commons.Color.popups.background
    })
    compare(item.shadowColor, Commons.Color.popups.background)
  }

  function test_popupThemeColors() {
    var item = createTemporaryObject(palette, this)
    tryCompare(item, "foreground", Commons.Color.foreground)
    tryCompare(item, "backgroundColor", Commons.Color.popups.background)
  }

  function test_themeChange() {
    var item = createTemporaryObject(palette, this)
    var oldForeground = Commons.Color.foreground
    var oldBackground = Commons.Color.popups.background
    try {
      Commons.Color.foreground = "#575279"
      Commons.Color.popups.background = "#faf4ed"
      tryCompare(item, "foreground", "#575279")
      tryCompare(item, "backgroundColor", "#faf4ed")
    } finally {
      Commons.Color.foreground = oldForeground
      Commons.Color.popups.background = oldBackground
    }
  }
}
