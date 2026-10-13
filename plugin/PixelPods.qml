import QtQuick
import QtQuick.Window
import Qt5Compat.GraphicalEffects

Item {
  id: root

  required property color foregroundColor
  required property color backgroundColor
  property string caseState: ""
  property bool pairingMode: false
  property bool referenceColors: false
  readonly property real artworkTop: artwork.y
  readonly property bool lightBackground: backgroundColor.hslLightness > 0.5
  readonly property bool closedCase: caseState === "closed"
  // Choose source paths and rendering mode together. During a case transition,
  // old repeater delegates may still exist until the next binding evaluation.
  readonly property var assetSet: {
    var light = backgroundColor.hslLightness > 0.5
    var directory = (light ? {
      "closed": "case-matrix-dark-closed",
      "open": "case-matrix-light-both",
      "open-left": "case-matrix-light-left",
      "open-right": "case-matrix-light-right",
      "open-empty": "case-matrix-light-empty"
    } : {
      "closed": "case-matrix-dark-closed",
      "open": "case-matrix-dark-both",
      "open-left": "case-matrix-dark-left",
      "open-right": "case-matrix-dark",
      "open-empty": "case-matrix-dark-empty"
    })[caseState] || ""
    return { directory: directory, standalone: directory === "",
      path: "assets/" + (directory || (light ? "earbuds-dots-light" : "earbuds-dots-dark")) + "/",
      lightClosed: light && caseState === "closed" }
  }
  readonly property string caseDirectory: assetSet.directory
  readonly property bool standaloneDots: assetSet.standalone
  readonly property bool openCase: !standaloneDots && !closedCase
  readonly property real caseFrontTop: lightBackground ? 208 / 520 : 230 / 536
  // Closed cases reuse the dark masks, with their opaque surface beneath the dots.
  readonly property bool lightLayerOrder: lightBackground && !closedCase
  readonly property color shadowColor: lightLayerOrder ? foregroundColor : backgroundColor
  readonly property color baseColor: lightLayerOrder ? backgroundColor : foregroundColor
  readonly property color rimColor: shade(lightBackground ? (closedCase ? 0.85 : 0.55) : 0.65)
  readonly property color moonlightColor: shade(lightBackground ? (closedCase ? 0.90 : 0.28) : 0.88)

  function shade(amount) {
    return Qt.tint(backgroundColor, Qt.rgba(foregroundColor.r,
      foregroundColor.g, foregroundColor.b, amount))
  }

  Item {
    id: artwork
    anchors.horizontalCenter: parent.horizontalCenter
    // Keep the base at the closed case's bottom edge as the lid opens upward.
    y: root.caseDirectory ? (parent.height + width * 416 / 564) / 2 - height
      : (parent.height - height) / 2
    width: parent.width
    height: width * (root.closedCase ? 416 / 564
      : root.caseDirectory ? (root.lightBackground ? 520 / 642 : 536 / 633)
      : root.lightBackground ? 610 / 975 : 596 / 995)

    Image {
      id: dots
      anchors.fill: parent
      readonly property real displayPixels: width * Screen.devicePixelRatio
      readonly property string textureName: displayPixels <= 128 ? "dots-128.png"
        : displayPixels <= 256 ? "dots-256.png" : "dots.png"
      source: root.assetSet.standalone ? root.assetSet.path + textureName : ""
      smooth: true
      mipmap: true
      visible: false
    }

    ColorOverlay {
      anchors.fill: parent
      source: dots
      color: root.foregroundColor
      visible: root.standaloneDots
    }

    Repeater {
      model: root.standaloneDots ? [] : root.lightLayerOrder
        ? ["base", "shadow", "rim", "moonlight"]
        : ["shadow", "base", "rim", "moonlight"]

      Item {
        id: layer
        required property string modelData
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        height: root.openCase ? artwork.height * root.caseFrontTop : artwork.height
        clip: root.openCase
        readonly property color targetColor: modelData === "shadow" ? root.shadowColor
          : modelData === "base" ? root.baseColor
          : modelData === "moonlight" ? root.moonlightColor : root.rimColor
        readonly property bool recolor: !root.referenceColors

        Image {
          id: pixels
          width: artwork.width
          height: artwork.height
          // Dark dots need the opposite bevel shading to keep the lower face lit.
          source: root.assetSet.standalone ? "" : root.assetSet.path + (root.assetSet.lightClosed
            && layer.modelData === "base" ? "base-light" : layer.modelData) + ".png"
          // Filter the fine matrix marks when shrinking to popup size.
          smooth: true
          mipmap: true
          visible: !layer.recolor
        }

        // Tint the alpha masks so residual source hues cannot introduce new colors.
        ColorOverlay {
          anchors.fill: pixels
          source: pixels
          visible: layer.recolor
          color: layer.targetColor
        }
      }
    }

    // Reuse the blank closed-case body beneath every open-lid variant. The
    // original open layers stop above it, so no baked-in LED survives underneath.
    Item {
      id: blankCaseFront
      visible: root.openCase
      y: artwork.height * root.caseFrontTop
      width: artwork.width
      height: artwork.height - y

      Repeater {
        model: root.openCase ? ["shadow", "base", "rim", "moonlight"] : []
        Item {
          required property string modelData
          anchors.fill: parent
          readonly property color targetColor: modelData === "shadow" ? root.backgroundColor
            : modelData === "base" ? root.foregroundColor
            : root.shade(modelData === "rim" ? (root.lightBackground ? 0.85 : 0.65)
              : (root.lightBackground ? 0.90 : 0.88))
          Image {
            id: frontPixels
            anchors.fill: parent
            source: "assets/case-matrix-dark-closed/" +
              (root.lightBackground && modelData === "base" ? "base-light" : modelData) + ".png"
            // Below the closed lid and hinge; keep only the unlit lower body.
            sourceClipRect: Qt.rect(0, 176, 564, 240)
            smooth: true
            mipmap: true
            visible: root.referenceColors
          }
          ColorOverlay {
            anchors.fill: parent
            source: frontPixels
            color: parent.targetColor
            visible: !root.referenceColors
          }
        }
      }
    }

    Item {
      id: pairingLight
      objectName: "pairingLight"
      visible: root.pairingMode && !root.standaloneDots && !root.closedCase
      // The halo reaches twice the former dot radius, fading to transparency.
      width: 2 * Math.max(2, artwork.width * 0.022)
      height: width
      anchors.horizontalCenter: parent.horizontalCenter
      y: artwork.height * 0.61 - height / 2
      RadialGradient {
        anchors.fill: parent
        gradient: Gradient {
          GradientStop { position: 0.0; color: "#ffffff" }
          GradientStop { position: 0.25; color: "#f2ffffff" }
          GradientStop { position: 0.5; color: "#80ffffff" }
          GradientStop { position: 0.75; color: "#26ffffff" }
          GradientStop { position: 1.0; color: "#00ffffff" }
        }
      }
      SequentialAnimation on opacity {
        running: pairingLight.visible && root.visible
        loops: Animation.Infinite
        NumberAnimation { from: 0; to: 1; duration: 900; easing.type: Easing.InOutSine }
        NumberAnimation { from: 1; to: 0; duration: 900; easing.type: Easing.InOutSine }
      }
    }
  }
}
