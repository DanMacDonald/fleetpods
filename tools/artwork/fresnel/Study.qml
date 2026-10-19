import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Window
import "../../../plugin/artwork/AirPodsMesh.js" as Asset
import "Mesh.js" as Experiment
import "Blend.js" as Blend

Window {
  id: window
  visible: true
  width: 1240; height: 920
  title: "FleetPods · Highlight art study"
  color: "#191b27"
  property color foreground: "#afb4d8"
  property color shadow: "#252938"
  property string scene: Qt.application.arguments.indexOf("--empty") >= 0 ? "open-empty" : ""
  property real yaw: 0
  property var settings: ({strength: strength.value, power: power.value,
    white: white.value, fill: fill.value, face: face.value, fresnelOnly: only.checked,
    baselineFill: baselineFill.value, baselineFace: baselineFace.value})
  readonly property int grid: scene === "" ? 47 : 71
  readonly property bool capture: Qt.application.arguments.indexOf("--capture") >= 0

  component LabelText: Text {
    color: window.foreground
    textFormat: Text.PlainText
  }

  component Artwork: Item {
    id: artwork
    property int mode: 0
    property real overlay: blendAmount.value
    property var options: window.settings
    property string stateName: window.scene
    property real angle: window.yaw
    property color tint: window.foreground
    onOptionsChanged: requestPaint()
    onOverlayChanged: requestPaint()
    onStateNameChanged: requestPaint()
    onAngleChanged: requestPaint()
    onTintChanged: requestPaint()
    onWidthChanged: requestPaint()
    onHeightChanged: requestPaint()
    function requestPaint() { canvas.requestPaint() }
    Canvas {
    id: canvas
    width: parent.width * 2; height: parent.height * 2
    scale: 0.5
    transformOrigin: Item.TopLeft
    smooth: true
    onPaint: {
      var ctx = getContext("2d")
      ctx.reset()
      ctx.scale(2, 2)
      var n = window.grid, renderer = Experiment
      var baseOptions = Object.assign({}, artwork.options, {baseline: true})
      var renderOptions = artwork.mode === 1 ? artwork.options : baseOptions
      var lid = artwork.stateName === "closed" ? 0 : 1
      var values = renderer.frame(Asset.geometry, artwork.stateName, lid, 0, artwork.angle, 0.24,
        n, n, false, undefined, 0.98 * (artwork.stateName === "" ? 1.38 : 1),
        undefined, 1, artwork.stateName === "", undefined, renderOptions)
      var style = renderer.dotStyle(false, 3, window.color, artwork.tint, window.shadow, renderOptions)
      var reflection, reflectionStyle
      if (artwork.mode === 2) {
        reflection = Experiment.frame(Asset.geometry, artwork.stateName, lid, 0, artwork.angle, 0.24,
          n, n, false, undefined, 0.98 * (artwork.stateName === "" ? 1.38 : 1),
          undefined, 1, artwork.stateName === "", undefined, artwork.options)
        reflectionStyle = Experiment.dotStyle(false, 3, window.color, artwork.tint, window.shadow, artwork.options)
      }
      var background = [window.color.r, window.color.g, window.color.b]
      var pitch = artwork.width / n, dot = pitch / 2
      for (var i = 0; i < values.length; i++) {
        var band = style.level(values[i])
        var reflectionBand = reflection ? reflectionStyle.level(reflection[i]) : 0
        if (!band && !reflectionBand) continue
        var c = band ? style.colors[band] : background
        if (artwork.mode === 2) c = Blend.combine(
          reflectionBand ? reflectionStyle.colors[reflectionBand] : background, c, artwork.overlay)
        ctx.fillStyle = Qt.rgba(c[0], c[1], c[2], 1)
        var left = Math.round(((i % n + 0.5) * 3 - 0.75) * 2) / 2
        var top = Math.round(((Math.floor(i / n) + 0.5) * 3 - 0.75) * 2) / 2
        ctx.fillRect(left * pitch / 3, top * pitch / 3, dot, dot)
      }
    }
    }
  }

  Rectangle {
  id: content
  anchors.fill: parent
  color: window.color
  ColumnLayout {
    anchors.fill: parent
    anchors.margins: 24
    spacing: 10
    LabelText { text: "Highlight art study"; font.pixelSize: 24 }
    LabelText { text: "Compare curves at the plugin's dot density. Changes here stay in this study." }
    RowLayout {
      ComboBox {
        model: ["Pods", "Open case", "Closed case", "Right pod out", "Empty case"]
        currentIndex: ["", "open", "closed", "open-left", "open-empty"].indexOf(window.scene)
        onActivated: window.scene = ["", "open", "closed", "open-left", "open-empty"][currentIndex]
      }
      LabelText { text: "View" }
      Slider { from: -0.8; to: 0.8; value: 0; onMoved: window.yaw = value; Layout.fillWidth: true }
      Button { text: "Reset lighting"; onClicked: { strength.value = 1; power.value = 1.5; white.value = 0.65; fill.value = 0.20; face.value = 0.85; blendAmount.value = 0.5; baselineFill.value = 0.57; baselineFace.value = 0.73 } }
    }
    RowLayout {
      CheckBox { id: only; text: "Fresnel only"; checked: true }
      LabelText { text: only.checked ? "View-angle reflection only · no fill or key light" : "Combined surface lighting and reflection" }
    }
    RowLayout {
      Layout.fillWidth: true
      Repeater {
        model: 3
        ColumnLayout {
          required property int index
          Layout.fillWidth: true
          Layout.preferredWidth: (window.width - 64) / 3
          LabelText { text: ["Adjustable baseline", "Fresnel experiment", "Fresnel + baseline"][index]; font.pixelSize: 18 }
          Artwork { mode: index; Layout.alignment: Qt.AlignHCenter; Layout.preferredWidth: window.grid * (window.scene === "" ? 6 : 4); Layout.preferredHeight: Layout.preferredWidth }
          Artwork { mode: index; Layout.alignment: Qt.AlignHCenter; Layout.preferredWidth: window.grid * 3; Layout.preferredHeight: window.grid * 3 }
        }
      }
    }
    LabelText { text: "Lower row: actual 3 px dot pitch. Upper row: the same sampled dots enlarged." }
    GridLayout {
      columns: 3
      Layout.fillWidth: true
      LabelText { text: "Reflection strength" }
      Slider { id: strength; from: 0; to: only.checked ? 4 : 1; value: 1; Layout.fillWidth: true }
      LabelText { text: strength.value.toFixed(2) + "×" }
      LabelText { text: "Edge falloff" }
      Slider { id: power; from: 0.5; to: 6; value: 1.5; Layout.fillWidth: true }
      LabelText { text: power.value.toFixed(2) }
      LabelText { text: "White in highlight" }
      Slider { id: white; from: 0; to: 1; value: 0.65; Layout.fillWidth: true }
      LabelText { text: Math.round(white.value * 100) + "%" }
      LabelText { text: "Baseline overlay" }
      Slider { id: blendAmount; from: 0; to: 1; value: 0.5; Layout.fillWidth: true }
      LabelText { text: Math.round(blendAmount.value * 100) + "%" }
      LabelText { text: "Baseline frontal fill" }
      Slider { id: baselineFill; from: 0; to: 1; value: 0.57; Layout.fillWidth: true }
      LabelText { text: baselineFill.value.toFixed(2) }
      LabelText { text: "Baseline face brightness" }
      Slider { id: baselineFace; from: 0; to: 2; value: 0.73; Layout.fillWidth: true }
      LabelText { text: baselineFace.value.toFixed(2) + "×" }
      LabelText { text: "Experiment frontal fill"; visible: !only.checked }
      Slider { id: fill; from: 0; to: 0.6; value: 0.20; visible: !only.checked; Layout.fillWidth: true }
      LabelText { text: fill.value.toFixed(2); visible: !only.checked }
      LabelText { text: "Experiment face brightness"; visible: !only.checked }
      Slider { id: face; from: 0.5; to: 1; value: 0.85; visible: !only.checked; Layout.fillWidth: true }
      LabelText { text: face.value.toFixed(2); visible: !only.checked }
    }
  }
  }
  Timer {
    interval: 1200; running: window.capture
    onTriggered: content.grabToImage(function(result) {
      result.saveToFile("/tmp/fleetpods-highlight-study.png")
      Qt.quit()
    })
  }
}
