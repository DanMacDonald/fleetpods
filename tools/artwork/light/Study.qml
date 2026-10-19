import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtCore
import "../../../plugin/artwork/AirPodsMesh.js" as Asset
import "Parameters.js" as Parameters
import "Mesh.js" as Renderer

ApplicationWindow {
  id: window
  visible: true
  width: 1280; height: 900
  minimumWidth: 1050; minimumHeight: 740
  title: "FleetPods · Light renderer study"
  color: "#f3f1ed"
  palette.window: "#f3f1ed"
  palette.windowText: "#292724"
  palette.base: "#ffffff"
  palette.text: "#292724"
  palette.button: "#e7e4dd"
  palette.buttonText: "#292724"
  palette.highlight: "#4d6f82"
  palette.highlightedText: "#ffffff"

  readonly property bool capture: Qt.application.arguments.indexOf("--capture") >= 0
  property var settings: Parameters.defaults()
  property string scene: Qt.application.arguments.indexOf("--empty") >= 0 ? "open-empty" : ""
  property real yaw: 0
  property real pitch: 0.24
  property real lid: 1
  property real pose: 1
  property bool warmPaper: false
  readonly property color paper: warmPaper ? "#faf6ed" : "#ffffff"
  readonly property color ink: warmPaper ? "#57516f" : "#000000"
  property var baselineFrame: null
  property var experimentFrame: null
  property bool loaded: false
  property string presetError: ""

  Settings {
    id: saved
    // A separate study preference file, never the plugin's configuration.
    location: StandardPaths.writableLocation(StandardPaths.GenericConfigLocation)
      + "/fleetpods-light-study.ini"
    property string parameters: ""
  }
  function refreshArt() { if (!redraw.running) redraw.start() }
  function setParameter(key, value) {
    var next = Object.assign({}, settings)
    next[key] = value
    settings = Parameters.normalize(next)
  }
  onSettingsChanged: {
    refreshArt()
    if (loaded && !capture) saved.parameters = JSON.stringify(settings)
  }
  onSceneChanged: refreshArt()
  onYawChanged: refreshArt()
  onPitchChanged: refreshArt()
  onLidChanged: refreshArt()
  onPoseChanged: refreshArt()
  Component.onCompleted: {
    if (!capture && saved.parameters) {
      try { settings = Parameters.normalize(JSON.parse(saved.parameters)) } catch (e) {}
    }
    loaded = true
    refreshArt()
  }
  Timer {
    id: redraw
    interval: 35
    onTriggered: {
      window.baselineFrame = Renderer.frame(Asset.geometry, window.scene, window.yaw,
        window.pitch, window.lid, window.pose, window.settings, true)
      window.experimentFrame = Renderer.frame(Asset.geometry, window.scene, window.yaw,
        window.pitch, window.lid, window.pose, window.settings, false)
    }
  }

  component Artwork: Item {
    id: art
    property var frame
    property bool enlarged: false
    property color paper: window.paper
    property color ink: window.ink
    onFrameChanged: canvas.requestPaint()
    onPaperChanged: canvas.requestPaint()
    onInkChanged: canvas.requestPaint()
    onWidthChanged: canvas.requestPaint()
    onHeightChanged: canvas.requestPaint()
    Canvas {
      id: canvas
      width: parent.width * 2; height: parent.height * 2
      scale: 0.5; transformOrigin: Item.TopLeft
      onPaint: {
        var ctx = getContext("2d")
        ctx.reset(); ctx.scale(2, 2)
        if (!art.frame) return
        var f = art.frame, n = f.columns
        // Enlarge exactly the same sampled dots; never rerender a denser mesh.
        var magnification = art.enlarged ? 2 : 1
        var extent = n * f.pitch * magnification
        var ox = (art.width - extent) / 2, oy = (art.height - extent) / 2
        var dot = f.dotSize
        for (var i = 0; i < f.values.length; i++) {
          var band = Math.round(f.values[i] * 64)
          if (!band) continue
          var t = Math.pow(band / 64, f.paletteCurve)
          ctx.fillStyle = Qt.rgba(art.paper.r + (art.ink.r - art.paper.r) * t,
            art.paper.g + (art.ink.g - art.paper.g) * t,
            art.paper.b + (art.ink.b - art.paper.b) * t, 1)
          var x = Math.round(((i % n + 0.5) * f.pitch - dot / 2) * 2) / 2
          var y = Math.round(((Math.floor(i / n) + 0.5) * f.pitch - dot / 2) * 2) / 2
          ctx.fillRect(ox + x * magnification, oy + y * magnification,
            dot * magnification, dot * magnification)
        }
      }
    }
  }

  Rectangle {
    id: content
    anchors.fill: parent
    color: window.color
    ColumnLayout {
      anchors.fill: parent; anchors.margins: 20; spacing: 12
      RowLayout {
        ColumnLayout {
          Label { text: "Light renderer study"; font.pixelSize: 25; font.weight: Font.DemiBold }
          Label { text: "Tune the experiment. The live plugin stays unchanged."; color: "#66615b" }
        }
        Item { Layout.fillWidth: true }
        Button { text: "Copy / load settings"; onClicked: { presetText.text = JSON.stringify(window.settings, null, 2); window.presetError = ""; presets.open() } }
        Button { text: "Reset rendering"; onClicked: window.settings = Parameters.defaults() }
      }
      RowLayout {
        ComboBox {
          model: ["Pods", "Open case", "Closed case", "Right pod out", "Left pod out", "Empty case"]
          currentIndex: ["", "open", "closed", "open-left", "open-right", "open-empty"].indexOf(window.scene)
          onActivated: {
            window.scene = ["", "open", "closed", "open-left", "open-right", "open-empty"][currentIndex]
            window.lid = window.scene === "closed" ? 0 : 1
          }
        }
        Label { text: "Turn" }
        Slider { from: -1.2; to: 1.2; value: window.yaw; onMoved: window.yaw = value; Layout.fillWidth: true }
        Label { text: "Tilt" }
        Slider { from: -0.3; to: 0.8; value: window.pitch; onMoved: window.pitch = value; Layout.fillWidth: true }
        Button { text: "Reset view"; onClicked: { window.yaw = 0; window.pitch = 0.24; window.pose = 1; window.lid = window.scene === "closed" ? 0 : 1 } }
        CheckBox { text: "Warm paper"; checked: window.warmPaper; onToggled: window.warmPaper = checked }
      }
      RowLayout {
        Label { text: window.scene === "" ? "Pod rotation" : "Lid opening"; Layout.preferredWidth: 100 }
        Slider {
          from: 0; to: 1; value: window.scene === "" ? window.pose : window.lid
          onMoved: { if (window.scene === "") window.pose = value; else window.lid = value }
          Layout.fillWidth: true
        }
        Label { text: Math.round((window.scene === "" ? window.pose : window.lid) * 100) + "%"; Layout.preferredWidth: 42 }
        Label { text: "Drag to inspect intermediate poses"; color: "#66615b" }
      }
      RowLayout {
        Layout.fillWidth: true; Layout.fillHeight: true; spacing: 14
        Repeater {
          model: 2
          Rectangle {
            required property int index
            Layout.fillWidth: true; Layout.fillHeight: true
            color: window.paper; radius: 8; border.color: "#dedbd4"
            ColumnLayout {
              anchors.fill: parent; anchors.margins: 12; spacing: 0
              Label { text: index === 0 ? "Live baseline" : "Experiment"; font.pixelSize: 18; color: "#292724" }
              Label { text: "Same dots · 2× enlargement"; color: "#77716b"; font.pixelSize: 12 }
              Artwork {
                frame: index === 0 ? window.baselineFrame : window.experimentFrame
                enlarged: true
                Layout.fillWidth: true; Layout.fillHeight: true
                // Case framing includes empty margins; the painted mesh fits
                // within this panel while the dot enlargement stays exact.
              }
              Label { text: index === 0 ? "Actual size · 3 px spacing" : "Actual size · " + window.settings.dotPitch.toFixed(1) + " px spacing"; color: "#77716b"; font.pixelSize: 12 }
              Artwork {
                frame: index === 0 ? window.baselineFrame : window.experimentFrame
                Layout.fillWidth: true; Layout.preferredHeight: 215
              }
            }
          }
        }
        Rectangle {
          Layout.preferredWidth: 320; Layout.fillHeight: true
          color: "#ece9e2"; radius: 8
          ColumnLayout {
            anchors.fill: parent; anchors.margins: 12; spacing: 6
            Label { text: "Rendering controls"; font.pixelSize: 18 }
            Label { text: "Settings save automatically"; font.pixelSize: 12; color: "#66615b" }
            ScrollView {
              Layout.fillWidth: true; Layout.fillHeight: true
              clip: true; contentWidth: availableWidth
              ColumnLayout {
                width: parent.width; spacing: 8
                Repeater {
                  model: Parameters.controls()
                  ColumnLayout {
                    required property var modelData
                    Layout.fillWidth: true; spacing: 0
                    RowLayout {
                      Label { text: modelData.label; Layout.fillWidth: true; font.pixelSize: 12 }
                      TextField {
                        Layout.preferredWidth: 56
                        text: window.settings[modelData.key].toFixed(2)
                        horizontalAlignment: TextInput.AlignRight
                        selectByMouse: true
                        onEditingFinished: {
                          var number = Number(text)
                          if (isFinite(number)) window.setParameter(modelData.key, number)
                          text = Qt.binding(function() { return window.settings[modelData.key].toFixed(2) })
                        }
                      }
                    }
                    Slider {
                      Layout.fillWidth: true
                      from: modelData.from; to: modelData.to; stepSize: modelData.step || 0.01
                      value: window.settings[modelData.key]
                      onMoved: window.setParameter(modelData.key, value)
                    }
                  }
                }
              }
            }
          }
        }
      }
      Label { text: "Pod controls affect the standalone pair. Recess material brightness affects the case. All dots remain uniform squares."; color: "#66615b"; font.pixelSize: 12 }
    }
  }

  Dialog {
    id: presets
    title: "Copy or load a rendering preset"
    modal: true; anchors.centerIn: parent
    width: 540; height: 560
    ColumnLayout {
      anchors.fill: parent
      Label { text: "Copy these values, or paste a saved set and choose Load."; wrapMode: Text.Wrap; Layout.fillWidth: true }
      ScrollView {
        Layout.fillWidth: true; Layout.fillHeight: true
        TextArea { id: presetText; font.family: "monospace"; selectByMouse: true; wrapMode: TextEdit.NoWrap }
      }
      Label { text: window.presetError; color: "#a63232"; visible: text !== ""; Layout.fillWidth: true; wrapMode: Text.Wrap }
      RowLayout {
        Button { text: "Copy"; onClicked: { presetText.selectAll(); presetText.copy(); presetText.deselect() } }
        Button {
          text: "Load"
          onClicked: {
            try {
              var parsed = JSON.parse(presetText.text)
              if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw Error("Expected a settings object")
              window.settings = Parameters.normalize(parsed)
              presets.close()
            } catch (e) { window.presetError = "Could not load preset: " + e.message }
          }
        }
        Item { Layout.fillWidth: true }
        Button { text: "Close"; onClicked: presets.close() }
      }
    }
  }
  Timer {
    interval: 1400; running: window.capture
    onTriggered: content.grabToImage(function(result) {
      result.saveToFile("/tmp/fleetpods-light-study.png")
      Qt.quit()
    })
  }
}
