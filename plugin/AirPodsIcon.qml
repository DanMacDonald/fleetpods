import QtQuick
import QtQuick.Shapes
import Qt5Compat.GraphicalEffects
import "DotGrid.js" as Grid
import qs.Commons as Commons

// Model-specific outlines, with optional pixel art for the popup.
Item {
  id: root

  property real iconSize: 16
  property color color: Commons.Color.foreground
  // One of "buds", "pro", "max", or "unknown". Unknown uses the dotted Pro drawing.
  property string variant: "buds"
  property string caseState: ""
  property bool pairingMode: false
  property bool pixelArt: false

  width: iconSize
  height: iconSize
  implicitWidth: iconSize
  implicitHeight: iconSize
  // Pods enter stem-first through the same fixed boundary they leave through.
  clip: true

  readonly property bool isPro: variant === "pro"
  readonly property bool isUnknown: variant === "unknown"
  readonly property bool dotted: isPro || isUnknown
  readonly property bool isMax: variant === "max"
  readonly property bool isCase: !isMax && (caseExiting || caseState === "open" || caseState === "closed"
    || caseState === "open-left" || caseState === "open-right" || caseState === "open-empty")
  readonly property bool hiResCase: iconSize >= 32
  readonly property bool pixelCase: pixelArt && isCase && (dotted || caseState === "closed")
  readonly property bool usesPixelArt: pixelCase || (pixelArt && dotted && !isCase)
  // Keep one renderer through either pod's departure, including the second
  // pod leaving. open-left/open-right name the pod still seated in the case.
  readonly property bool usesMeshCase: pixelCase
  readonly property bool usesMeshBuds: pixelArt && dotted && !isCase
  readonly property real podsArtworkScale: 1.38
  function restingPodsTop(size) {
    return Grid.podsArtworkTop(size, podsArtworkScale, Grid.pitch)
  }
  // Connector attachment follows the resting artwork, not the entrance
  // translation. Pods descend over the wires without pulling their ends up.
  readonly property real artworkTop: usesPixelArt && pixelArtwork.item
    ? pixelArtwork.item.artworkTop
    : isCase ? caseY + ink[1] * fit : (iconSize - ink[3] * fit) / 2

  // Apple's artboards sit the outline low, so the glyph is fitted to its ink box instead: x, y, width, height in artboard units.
  readonly property var ink: isCase
    ? (caseState === "closed" ? (iconSize >= 32 ? [1.6, 15, 52.8, 36] : [4, 15, 48, 36])
      : (hiResCase ? [1.6, 8.86, 52.8, 42.14] : [4, 7, 48, 44]))
    : isPro ? [0.25, 26.25, 37.5, 25.75]
    : isMax ? [0, 12, 34, 38]
    : [10.5, 26.5, 35, 25.5]

  // The artwork's painted extent in this item's coordinates.
  readonly property rect inkBox: Qt.rect(
    isCase ? caseX + ink[0] * fit : (iconSize - ink[2] * fit) / 2,
    (isCase ? caseY + ink[1] * fit : (iconSize - ink[3] * fit) / 2) + (isCase ? 0 : (1 - budsIn) * -iconSize),
    ink[2] * fit, ink[3] * fit)
  readonly property real fit: iconSize / Math.max(ink[2], ink[3])
  readonly property real caseBaseline: (iconSize + 44 * fit) / 2
  readonly property real caseX: (iconSize - ink[2] * fit) / 2 - ink[0] * fit
  readonly property real caseY: caseBaseline - (ink[1] + ink[3]) * fit
  // 1 = seated in the well, 0 = lifted clear. Not bound to caseState, so the
  // return trip can animate instead of snapping shut.
  property real leftSeat: 1
  property real rightSeat: 1
  property bool leftReturning: false
  property bool rightReturning: false
  readonly property int meshReturnMs: 500

  function meshLift(seat, returning) {
    // Begin with even the stem tip above the clip, without a long hidden lead-in.
    return (1 - seat) * (returning ? 0.72 : 1)
  }

  function returnTurn(lift) {
    // Hold the presentation angle until the head enters view. Finish the turn
    // with the stem about halfway into its well, then lower it straight down.
    return Math.max(0, Math.min(1, (lift - 0.12) / 0.24))
  }
  property string seatedState: ""
  property bool droppingIntoClosed: false
  property bool caseExiting: false
  property real caseOut: 0

  function startCaseExitWhenClear() {
    if (caseExiting && leftSeat === 0 && rightSeat === 0 && !caseExit.running)
      caseExit.start()
  }
  onLeftSeatChanged: startCaseExitWhenClear()
  onRightSeatChanged: startCaseExitWhenClear()
  // 1 = buds glyph in place, 0 = waiting above the icon.
  property real budsIn: 1
  // Head from the case art, plus a stem that stays inside the well until the bud lifts.
  readonly property string leftPodPath: "M10 24l1.5-5.5c.8-3 3-4.5 5.5-3.5c2.3.9 3.6 2.7 4.5 4.5c2.5-.6 4 .6 4 2.5v2Z M15 21.5h5.4c.35 0 .55.4.45.85l-1.1 16.4c-.15.95-.95 1.6-1.9 1.6h-.3c-.95 0-1.75-.65-1.9-1.6l-1.1-16.4c-.1-.45.1-.85.45-.85Z"
  readonly property string rightPodPath: "M46 24l-1.5-5.5c-.8-3-3-4.5-5.5-3.5c-2.3.9-3.6 2.7-4.5 4.5c-2.5-.6-4 .6-4 2.5v2Z M35.6 21.5h5.4c.35 0 .55.4.45.85l-1.1 16.4c-.15.95-.95 1.6-1.9 1.6h-.3c-.95 0-1.75-.65-1.9-1.6l-1.1-16.4c-.1-.45.1-.85.45-.85Z"

  function seatTargets(state) {
    if (state === "open-left") return [1, 0]
    if (state === "open-right") return [0, 1]
    if (state === "open-empty") return [0, 0]
    return [1, 1]
  }

  function moveSeat(anim, value) {
    // A running animation keeps its old endpoint until restarted.
    anim.stop()
    var seat = anim === leftLift ? leftSeat : rightSeat
    // Start the turn while the pod is still above the artwork. Keep it on
    // reversal midway through a return; clear it for a new seated departure.
    if (seat < 0.02 && value === 1) {
      if (anim === leftLift) leftReturning = true
      else rightReturning = true
    } else if (seat > 0.98 && value === 0) {
      if (anim === leftLift) leftReturning = false
      else rightReturning = false
    }
    if (Math.abs((anim === leftLift ? leftSeat : rightSeat) - value) < 0.02) {
      if (anim === leftLift) leftSeat = value
      else rightSeat = value
      return
    }
    // caseStateChanged can run before usesMeshCase's binding refreshes. Set
    // timing from the requested state before starting: a running animation
    // otherwise retains the old standalone duration for this entire return.
    var meshCase = pixelArt && variant !== "max"
      && (variant === "pro" || variant === "unknown" || caseState === "closed")
    anim.duration = meshCase ? (value === 0 ? 840 : meshReturnMs) : 420
    // Returns descend at constant speed from the first frame through seating.
    anim.easing.type = meshCase
      ? (value === 1 ? Easing.Linear : Easing.InOutCubic) : Easing.InOutQuad
    anim.to = value
    anim.start()
  }

  function syncSeats(animate) {
    if (caseExiting && caseState === "" && animate) return
    caseExiting = false
    caseExit.stop()
    caseOut = 0
    if (caseState === "") {
      // Keep the empty case renderer until it has cleared the bottom clip.
      // If the state arrives early, let the departing pods finish first.
      if (animate && seatedState === "open-empty" && pixelArt && dotted) {
        caseExiting = true
        if (!leftLift.running || leftLift.to !== 0) moveSeat(leftLift, 0)
        if (!rightLift.running || rightLift.to !== 0) moveSeat(rightLift, 0)
        startCaseExitWhenClear()
        return
      }
      var fromCase = seatedState === "open" || seatedState === "open-left"
        || seatedState === "open-right" || seatedState === "open-empty"
        || seatedState === "closed"
      seatedState = ""
      droppingIntoClosed = false
      // The case loader is replaced by standalone artwork here. Preserve the
      // physical pose in this item so a returning pod can descend when a new
      // case renderer is loaded, including on startup with both pods out.
      leftLift.stop()
      rightLift.stop()
      leftSeat = 0
      rightSeat = 0
      leftReturning = false
      rightReturning = false
      if (animate && fromCase) budsDrop.start()
      else budsIn = 1
      return
    }
    budsDrop.stop()
    budsIn = 1
    if (caseState === "closed") {
      var lifted = leftSeat < 0.98 || rightSeat < 0.98
      var wasOpen = seatedState === "" || seatedState === "open" || seatedState === "open-left"
        || seatedState === "open-right" || seatedState === "open-empty"
      seatedState = "closed"
      if (animate && lifted && wasOpen) {
        droppingIntoClosed = true
        moveSeat(leftLift, 1)
        moveSeat(rightLift, 1)
        closeAfterDrop.interval = Math.max(leftLift.duration, rightLift.duration) + 20
        closeAfterDrop.restart()
        return
      }
      droppingIntoClosed = false
      leftLift.stop()
      rightLift.stop()
      leftSeat = 1
      rightSeat = 1
      leftReturning = false
      rightReturning = false
      return
    }
    droppingIntoClosed = false
    var next = seatTargets(caseState)
    seatedState = caseState
    if (!animate) {
      leftLift.stop()
      rightLift.stop()
      leftSeat = next[0]
      rightSeat = next[1]
      leftReturning = false
      rightReturning = false
      return
    }
    moveSeat(leftLift, next[0])
    moveSeat(rightLift, next[1])
  }

  onCaseStateChanged: syncSeats(visible)
  onVisibleChanged: if (!visible) { budsDrop.stop(); syncSeats(false) }
  Component.onCompleted: syncSeats(false)

  // The bar centers whatever bounds it can measure. The bud Shape's bounding
  // rect stays empty, and an empty box is pinned by its corner to the slot
  // center, so the glyph hangs below the other icons. This rect is the ink.
  Rectangle {
    color: "transparent"
    x: root.inkBox.x
    y: root.inkBox.y
    width: root.inkBox.width
    height: root.inkBox.height
  }

  NumberAnimation {
    id: leftLift
    target: root
    property: "leftSeat"
    duration: 420
    easing.type: Easing.InOutQuad
  }
  NumberAnimation {
    id: rightLift
    target: root
    property: "rightSeat"
    duration: 420
    easing.type: Easing.InOutQuad
  }

  Timer {
    id: closeAfterDrop
    interval: 440
    onTriggered: root.droppingIntoClosed = false
  }

  NumberAnimation {
    id: caseExit
    target: root
    property: "caseOut"
    from: 0
    to: 1
    duration: 350
    easing.type: Easing.Linear
    onFinished: {
      root.caseExiting = false
      root.caseOut = 0
      root.seatedState = ""
      root.syncSeats(false)
      if (root.visible && root.caseState === "") budsDrop.restart()
    }
  }

  NumberAnimation {
    id: budsDrop
    target: root
    property: "budsIn"
    from: 0
    to: 1
    duration: root.usesMeshBuds ? 494 : 414
    easing.type: Easing.Linear
  }

  Image {
    visible: root.isCase && root.caseState === "closed" && !root.droppingIntoClosed && !root.pixelCase
    source: root.isCase ? Qt.resolvedUrl(root.iconSize >= 32
      ? "assets/airpods-case-closed-large.svg"
      : "assets/airpods-case-closed.svg") : ""
    width: 56 * root.fit
    height: width
    x: root.caseX
    y: root.caseY
    sourceSize: Qt.size(width * 3, height * 3)
    smooth: true
    layer.enabled: visible
    layer.smooth: true
    layer.textureSize: Qt.size(Math.ceil(width * 3), Math.ceil(height * 3))
    layer.effect: ColorOverlay { color: root.color }
  }

  Image {
    id: hiResCaseImage
    visible: root.hiResCase && root.isCase && !root.pixelCase && (root.caseState !== "closed" || root.droppingIntoClosed)
    source: {
      // While a lifted pod drops back in, hold the full open case so the
      // popup does not blank before the closed glyph.
      var file = "airpods-case-open.svg"
      if (!root.droppingIntoClosed) {
        if (root.caseState === "open-left") file = "airpods-case-open-left-large.svg"
        else if (root.caseState === "open-right") file = "airpods-case-open-right-large.svg"
        else if (root.caseState === "open-empty") file = "airpods-case-open-empty-large.svg"
      }
      return Qt.resolvedUrl("assets/" + file)
    }
    width: 56 * root.fit
    height: width
    x: root.caseX
    y: root.caseY
    sourceSize: Qt.size(width * 3, height * 3)
    smooth: true
    layer.enabled: visible
    layer.smooth: true
    layer.textureSize: Qt.size(Math.ceil(width * 3), Math.ceil(height * 3))
    layer.effect: ColorOverlay { color: root.color }
  }

  Image {
    visible: hiResCaseImage.visible && root.caseState !== "open-empty"
    source: {
      var file = "airpods-case-open-seams.svg"
      if (!root.droppingIntoClosed) {
        if (root.caseState === "open-left") file = "airpods-case-open-left-seams.svg"
        else if (root.caseState === "open-right") file = "airpods-case-open-right-seams.svg"
        else if (root.caseState === "open-empty") return ""
      }
      return Qt.resolvedUrl("assets/" + file)
    }
    x: hiResCaseImage.x
    y: hiResCaseImage.y
    width: hiResCaseImage.width
    height: hiResCaseImage.height
    sourceSize: hiResCaseImage.sourceSize
    smooth: true
  }

  Item {
    visible: !root.hiResCase && root.isCase && (root.caseState !== "closed" || root.droppingIntoClosed)
    width: 56 * root.fit
    height: width
    x: root.caseX
    y: root.caseY

    Image {
      anchors.fill: parent
      source: Qt.resolvedUrl("assets/airpods-case-open-empty.svg")
      sourceSize: Qt.size(width * 3, height * 3)
      smooth: true
      layer.enabled: true
      layer.smooth: true
      layer.textureSize: Qt.size(Math.ceil(width * 3), Math.ceil(height * 3))
      layer.effect: ColorOverlay { color: root.color }
    }

    // Anything below the rim is inside the case, so a seated stem stays hidden.
    Item {
      width: parent.width
      height: 27 * root.fit
      clip: true

      Shape {
        visible: root.leftSeat > 0.02
        width: 56
        height: 56
        opacity: root.leftSeat > 0.2 ? 1 : root.leftSeat / 0.2
        preferredRendererType: Shape.CurveRenderer
        transform: [
          Scale { xScale: root.fit; yScale: root.fit },
          Translate { y: (1 - root.leftSeat) * -18 * root.fit }
        ]
        ShapePath {
          fillColor: root.color
          strokeWidth: -1
          fillRule: ShapePath.WindingFill
          PathSvg { path: root.leftPodPath }
        }
      }

      Shape {
        visible: root.rightSeat > 0.02
        width: 56
        height: 56
        opacity: root.rightSeat > 0.2 ? 1 : root.rightSeat / 0.2
        preferredRendererType: Shape.CurveRenderer
        transform: [
          Scale { xScale: root.fit; yScale: root.fit },
          Translate { y: (1 - root.rightSeat) * -18 * root.fit }
        ]
        ShapePath {
          fillColor: root.color
          strokeWidth: -1
          fillRule: ShapePath.WindingFill
          PathSvg { path: root.rightPodPath }
        }
      }
    }
  }

  Loader {
    id: artworkPalette
    active: root.pixelArt && (root.dotted || root.isCase)
    sourceComponent: PopupPalette { foregroundColor: root.color }
  }

  Loader {
    id: pixelArtwork
    active: root.usesPixelArt && artworkPalette.item !== null
    width: root.iconSize
    height: root.iconSize
    y: root.isCase ? root.caseOut * root.iconSize : (1 - root.budsIn) * -root.iconSize
    sourceComponent: root.usesMeshCase ? meshCaseComponent
      : root.usesMeshBuds ? meshBudsComponent : pixelComponent
  }

  Component {
    id: meshCaseComponent
    CaseMesh {
      objectName: "caseMeshArtwork"
      // Seat the returning pod before closing the lid over it.
      caseState: root.caseExiting ? "open-empty" : root.droppingIntoClosed ? "open" : root.caseState
      leftLift: root.meshLift(root.leftSeat, root.leftReturning)
      rightLift: root.meshLift(root.rightSeat, root.rightReturning)
      leftTurn: root.leftReturning ? root.returnTurn(leftLift) : 0
      rightTurn: root.rightReturning ? root.returnTurn(rightLift) : 0
      pairingMode: root.pairingMode
      foregroundColor: artworkPalette.item.foreground
      backgroundColor: artworkPalette.item.backgroundColor
    }
  }

  Component {
    id: meshBudsComponent
    BudsMesh {
      objectName: "budsMeshArtwork"
      arrival: root.budsIn
      // Fill more of the popup slot without enlarging the dots themselves.
      artworkScale: root.podsArtworkScale
      foregroundColor: artworkPalette.item.foreground
      backgroundColor: artworkPalette.item.backgroundColor
    }
  }

  Component {
    id: pixelComponent
    PixelPods {
      objectName: "pixelArtwork"
      pairingMode: root.pairingMode
      caseState: root.pixelCase ? (root.droppingIntoClosed ? "open" : root.caseState) : ""
      foregroundColor: artworkPalette.item.foreground
      backgroundColor: artworkPalette.item.backgroundColor
    }
  }

  Shape {
    visible: !root.isCase && !(root.pixelArt && root.dotted)
    width: root.isPro ? 38 : root.isMax ? 34 : 56
    height: root.isPro ? 56 : root.isMax ? 54 : 56
    // Ink origin to zero, then scale about zero, then centre: applied in list order.
    transform: [
      Translate { x: -root.ink[0]; y: -root.ink[1] },
      Scale { xScale: root.fit; yScale: root.fit },
      Translate {
        x: (root.iconSize - root.ink[2] * root.fit) / 2
        y: (root.iconSize - root.ink[3] * root.fit) / 2 + (1 - root.budsIn) * -root.iconSize
      }
    ]
    // The curve renderer is what keeps the stems smooth once the glyph is under a pixel wide.
    preferredRendererType: Shape.CurveRenderer
    // Sampled down from three times the painted size, or Apple's sub-pixel hairlines break into dashes.
    layer.enabled: true
    layer.smooth: true
    layer.textureSize: Qt.size(width * root.fit * 3, height * root.fit * 3)

    ShapePath {
      fillColor: root.color
      // Negative, not zero: zero still strokes, and strokeColor defaults to white.
      strokeWidth: -1
      // SVG fills non-zero, and the ear-tip holes are wound against their outline.
      fillRule: ShapePath.WindingFill
      PathSvg { path: root.isPro ? root.proPath : root.isMax ? root.maxPath : root.budsPath }
    }
  }

  // Outlines from the chapter nav on apple.com/airpods, one compound path each.
  readonly property string budsPath: "m19.0472 41.6014c1.6961-.5435 2.9684-1.3377 3.919-2.056v10.4647c0 .9404-.6801 1.9622-1.8118 1.9622-.2141 0-.1549.0113-.3796.0174-.775.0211-1.1255.0034-1.5764.0034-.9556 0-1.7321-1.0078-1.7321-1.9777 0 0-.0016-7.9666 0-8.0062.4363-.0114.8535-.1424 1.2629-.331.1063-.0298.2115-.0428.3179-.0768zm18.0461.0339c-.0471-.0142-.0937-.019-.1409-.0341-1.7379-.5568-3.0299-1.3768-3.9874-2.1087v10.5175c0 .9404.6801 1.9622 1.8118 1.9622.2141 0 .1549.0113.3796.0174.775.0211 1.1255.0034 1.5764.0034.9556 0 1.7321-1.0078 1.7321-1.9777 0 0 .0016-7.9666 0-8.0062-.4745-.0125-.9279-.1561-1.3716-.3739zm-12.5212-12.5755c.9158 1.1893 1.5012 2.3614 1.3384 4.244-.1558 1.8013-1.3522 3.5998-2.239 4.3652-.9412.8124-2.4403 2.1826-4.929 2.9799-.7896.2527-1.5126.3544-2.1636.3544-2.2127 0-3.5976-1.174-3.9654-1.5866-1.0698-1.2002-1.8559-3.7706-1.9491-5.3096-.1656-2.7333 3.1243-5.1079 4.5956-6.0765 1.8171-1.1962 3.7536-1.4531 4.7993-1.4531.0432 0 .0848.0004.1248.0012.8009.0165 2.6014.1611 4.3879 2.481zm-10.8064 6.4711c-.6333-1.8826-1.3741-2.6064-1.6801-2.0209-.407.7787.085 4.116 1.5193 5.1174.478.3337.9317-.8051.1608-3.0966zm5.6478-2.6104-.0461-.0631c-.2833-.3875-.8322-.4728-1.2197-.1895l-.8845.5593c-.3875.2833-.4728.8321-.1895 1.2196l.0461.0631c.2833.3875.8322.4728 1.2197.1895l.8845-.5593c.3875-.2833.4728-.8321.1895-1.2196zm25.9221 1.1866c-.0932 1.5389-.8792 4.1094-1.949 5.3096-.3679.4126-1.7527 1.5866-3.9654 1.5866-.6511 0-1.374-.1017-2.1636-.3544-2.4887-.7974-3.9878-2.1675-4.929-2.9799-.8867-.7654-2.0832-2.564-2.239-4.3652-.1628-1.8826.4225-3.0547 1.3384-4.244 1.7866-2.3199 3.587-2.4645 4.388-2.481.04-.0008.0816-.0012.1248-.0012 1.0457 0 2.9822.2568 4.7993 1.4531 1.4714.9686 4.7612 3.3433 4.5956 6.0765zm-6.5988-.8799-.8845-.5593c-.3875-.2833-.9363-.198-1.2197.1895l-.0461.0631c-.2833.3875-.1981.9363.1895 1.2196l.8845.5593c.3875.2833.9363.1981 1.2197-.1895l.0461-.0631c.2833-.3875.198-.9363-.1895-1.2196zm5.1776.2828c-.306-.5855-1.0468.1383-1.6801 2.0209-.7709 2.2914-.3172 3.4302.1608 3.0966 1.4344-1.0014 1.9263-4.3387 1.5193-5.1174z"
  readonly property string proPath: "m12.8732 39.9868c.4781-.2063.9295-.4448 1.3597-.7047l.0123 10.3713c.0001.0895.0002.179-.0002.2684-.0004.0753-.0012.1506-.0031.226-.0043.1642-.0137.3298-.0427.4921-.0294.1647-.0775.3181-.1535.4678-.0746.1472-.1724.2819-.2889.3987s-.251.2148-.398.2898c-.1495.0764-.3027.1249-.4674.1547-.1623.0294-.3278.0392-.492.0439-.0753.0021-.1506.0031-.226.0036-.0895.0007-.179.0007-.2684.0009-.0895.0001-.9254.0011-1.015.0007-.0753-.0004-.1507-.0012-.226-.0031-.1642-.0043-.3298-.0137-.4921-.0427-.1647-.0294-.3181-.0775-.4678-.1535-.1471-.0747-.2819-.1724-.3987-.2889s-.2148-.251-.2898-.398c-.0764-.1495-.1249-.3027-.1547-.4673-.0294-.1623-.0392-.3279-.0438-.4921-.0021-.0753-.0031-.1506-.0036-.226-.0007-.0895-.0008-.1789-.0009-.2684l-.0104-8.7734c.7386-.0081 1.4647-.0917 2.1577-.2603.6558-.1594 1.2994-.3746 1.9133-.6394zm-7.7782-10.7131c-.3191 0-.6392.0356-.9587.0972.7881-.8296 1.6831-1.4359 2.6149-1.8897.1805-.0879.3593-.1699.5361-.2462 1.4226-.6138 2.7245-.8594 3.8769-.8594.5481 0 1.0624.0556 1.5396.1536 1.6049.3297 3.394 1.3396 4.2358 3.3776.6855 1.6595 1.0043 3.8107-.6151 6.1314-.9628 1.3798-2.3504 2.3846-3.8474 3.0305-.5743.2478-1.1646.4427-1.7534.5859-.6725.1635-1.3534.2359-2.0122.2372.6157-.4592 1.1254-1.0375 1.4781-1.736.7756-1.5358.7535-3.3085-.0655-5.2687-.9353-2.2386-2.6285-3.0591-3.313-3.308-.5574-.2026-1.1348-.3054-1.716-.3054zm7.1136 2.8617c.1325.3071.4041.5388.7267.62.3588.0905.6454-.0138.9829-.1594l.3715-.1603c.3375-.1456.61-.2827.7903-.6057.1623-.2905.1801-.647.0475-.9542-.1324-.3071-.4041-.5388-.7267-.62-.3588-.0905-.6454.0138-.9829.1594l-.3715.1603c-.3375.1456-.61.2826-.7903.6057-.1623.2905-.1801.647-.0475.9542zm-11.3961 4.9296c-.3892-.869-.7632-2.3354-.3421-3.757.4819-1.6268 1.3806-2.464 2.3693-2.9652.1106-.0561.2205-.1076.3297-.1547.6757-.2916 1.3219-.4145 1.9258-.4145.5541 0 1.0726.1036 1.5451.2753 1.0707.3893 2.3146 1.3365 3.0226 3.0308.5765 1.3797.9526 3.1234.0804 4.8506-.5186 1.0269-1.3431 1.7209-2.267 2.1195-.3545.153-.7234.2624-1.0955.3306-.2441.0447-.4924.0656-.7408.0656-1.1174 0-2.2365-.4205-2.9775-.972-.702-.5224-1.3219-1.2302-1.8499-2.4091zm3.8507.7843c.273-.0298.5328-.1447.7356-.3587.1257-.1326.2296-.3032.3007-.5158.226-.6753.0308-1.4064-.2175-1.9957-.3049-.7238-.761-1.1617-1.1368-1.3636-.2163-.1162-.4545-.1874-.7036-.1813-.1837.0045-.3734.0507-.5648.1528-.3133.1671-.5831.4758-.6816 1.1251-.086.5674.0975 1.1845.2626 1.5568.2241.5049.4606.8201.7175 1.0622.3315.3124.8455.5667 1.2878.5182zm20.4636 2.1375c-.4781-.2063-.9294-.4448-1.3596-.7047l-.0123 10.3713c-.0001.0895-.0002.179.0002.2684.0004.0753.0012.1506.0031.226.0043.1642.0137.3298.0427.4921.0294.1647.0775.3181.1535.4678.0746.1472.1724.2819.2889.3987s.251.2148.398.2898c.1495.0764.3027.1249.4674.1547.1623.0294.3278.0392.492.0439.0753.0021.1506.0031.226.0036.0895.0007.179.0007.2684.0009.0895.0001.9254.0011 1.015.0007.0753-.0004.1507-.0012.226-.0031.1642-.0043.3298-.0137.4921-.0427.1647-.0294.3181-.0775.4678-.1535.1471-.0747.2819-.1724.3987-.2889s.2148-.251.2898-.398c.0764-.1495.1249-.3027.1547-.4673.0294-.1623.0392-.3279.0438-.4921.0021-.0753.0031-.1506.0036-.226.0007-.0895.0008-.1789.0009-.2684l.0104-8.7734c-.7386-.0081-1.4647-.0917-2.1577-.2603-.6557-.1594-1.2994-.3746-1.9133-.6394zm8.7369-10.6159c-.3195-.0616-.6396-.0972-.9587-.0972-.5812 0-1.1586.1028-1.716.3054-.6846.2489-2.3777 1.0694-3.313 3.308-.819 1.9602-.8411 3.7328-.0655 5.2687.3528.6985.8624 1.2767 1.4781 1.736-.6588-.0013-1.3397-.0737-2.0122-.2372-.5889-.1432-1.1791-.3381-1.7534-.5859-1.497-.6459-2.8846-1.6507-3.8474-3.0305-1.6194-2.3207-1.3007-4.4719-.6151-6.1314.8419-2.038 2.6309-3.0479 4.2358-3.3776.4771-.098.9915-.1536 1.5396-.1536 1.1523 0 2.4543.2456 3.8769.8594.1769.0763.3557.1583.5361.2462.9318.4539 1.8268 1.0601 2.6149 1.8897zm-8.1199 1.8104c-.1804-.3231-.4529-.4601-.7903-.6057l-.3715-.1603c-.3375-.1456-.6241-.2499-.9829-.1594-.3226.0812-.5943.3129-.7267.62-.1325.3071-.1147.6637.0475.9542.1804.3231.4529.4601.7903.6057l.3715.1603c.3375.1456.6241.2498.9829.1594.3226-.0812.5942-.3129.7267-.62s.1147-.6637-.0475-.9542zm9.5938 8.2928c-.741.5515-1.8601.972-2.9775.972-.2484 0-.4966-.0208-.7407-.0656-.3722-.0682-.7411-.1776-1.0956-.3306-.924-.3986-1.7485-1.0926-2.267-2.1195-.8722-1.7272-.496-3.4709.0805-4.8506.7079-1.6943 1.9518-2.6415 3.0225-3.0308.4725-.1718.991-.2753 1.5452-.2753.6039 0 1.2501.1229 1.9257.4145.1091.0471.2191.0986.3297.1547.9887.5012 1.8874 1.3384 2.3693 2.9652.4211 1.4215.0471 2.888-.3421 3.757-.528 1.1788-1.1479 1.8867-1.8499 2.4091zm.2672-4.762c-.0984-.6494-.3683-.958-.6816-1.1251-.1915-.1022-.3812-.1484-.5648-.1528-.2491-.0061-.4873.0651-.7036.1813-.3759.2019-.832.6398-1.1368 1.3636-.2483.5894-.4434 1.3204-.2175 1.9957.0712.2126.175.3832.3007.5158.2028.214.4626.3289.7356.3587.4423.0484.9563-.2058 1.2878-.5182.2569-.2421.4935-.5573.7175-1.0622.1652-.3723.3486-.9893.2626-1.5568z"
  readonly property string maxPath: "m28.1074 19.9239a16.48 16.48 0 0 0 -11.1391-3.8564 16.6761 16.6761 0 0 0 -11.0307 3.8251 13.7027 13.7027 0 0 1 11.03-5.6445l.0324-.001.0323.001a14.0385 14.0385 0 0 1 11.0751 5.6758zm5.8908 9.4515c-.0023-.1218-.0065-.2413-.0108-.3606l-2.124.1116c.003.2755.0011.4837.0119.6141.015.1851.0556.2291.2073.22s.2846-.0125.2846-.0125.003.6717.003 1.4453a.9184.9184 0 0 0 .1981.6441c-.0253-.0139-.046-.026-.073-.0407a1.76 1.76 0 0 0 -1.1136-.0838 4.2534 4.2534 0 0 0 -.8141.3451 3.233 3.233 0 0 1 -.448 2.1706c-.744 1.5126-4.4739 9.5192-6.3231 12.0837a5.6741 5.6741 0 0 1 -1.5548 1.6271 5.0642 5.0642 0 0 0 .1155.7407 1.6237 1.6237 0 0 0 1.2337 1.0973c1.5287.251 2.9509-1.6365 3.4964-2.4424s1.456-2.2649 3.36-5.7777c1.5994-2.9508 3.2484-6.5836 3.174-7.9225a2.8277 2.8277 0 0 0 -.375-1.236 1.37 1.37 0 0 0 -.3859-.3861.591.591 0 0 0 .1213.0163c.1532 0 .6206-.0991.6206-.8139s-.017-1.5324-.017-1.5324.0324-.0026.2305-.0119c.1517-.0071.1847-.087.1844-.3052-.0002-.057-.0002-.1206-.002-.1902zm-29.906-7.3471a14.8884 14.8884 0 0 0 .7553-1.8568c1.4854-3.0082 6.2274-6.7652 12.1194-6.7652l.0331-.001.0326.001c5.8922 0 10.634 3.757 12.12 6.7652a14.8563 14.8563 0 0 0 .755 1.8568 14.344 14.344 0 0 1 1.9274 6.2567l2.1047-.1106a16.2156 16.2156 0 0 0 -2.4042-7.5263 13.0512 13.0512 0 0 0 -.9392-1.1513 16.3879 16.3879 0 0 0 -13.5634-7.3915l-.0329.0008-.0326-.0008a16.3871 16.3871 0 0 0 -13.5636 7.3915 12.9711 12.9711 0 0 0 -.9394 1.1513 16.216 16.216 0 0 0 -2.4044 7.5277l2.1048.1105a14.3438 14.3438 0 0 1 1.9274-6.258zm7.2341 15.5937a54.4387 54.4387 0 0 0 -3.7722-6.4595 2.3354 2.3354 0 0 0 -2.7954-.5693 3.3356 3.3356 0 0 0 -.3614.3036c-.6509.767-.5552 1.667.056 2.91.744 1.5126 4.4739 9.5192 6.3231 12.0838.9176 1.2723 1.6491 2.0038 2.79 1.9046a1.5692 1.5692 0 0 0 .9562-.4324 2.6555 2.6555 0 0 0 .5359-1.6444 5.6731 5.6731 0 0 0 -.8431-2.4424c-.1243-.2483-2.418-4.6869-2.8891-5.654zm-1.185 8.89c-1.8492-2.5644-5.5793-10.571-6.3232-12.0836a3.2329 3.2329 0 0 1 -.4481-2.1706 4.2606 4.2606 0 0 0 -.8142-.3451 1.7974 1.7974 0 0 0 -1.08.072.9972.9972 0 0 0 .1532-.5915c0-.7737.003-1.4453.003-1.4453s.1332.0031.2849.0124.1921-.0346.2073-.22c.0106-.13.0088-.338.0117-.6129l-2.124-.1116c-.0044.119-.0086.238-.0108.3593-.0019.1 0 .1877.0012.2607.0034.1656.05.2284.1815.2345.198.0093.2306.0119.2306.0119s-.0173.8178-.0173 1.5327.4674.8139.6206.8139c.01 0 .0252-.0016.0391-.0027a1.3714 1.3714 0 0 0 -.3652.3725 2.832 2.832 0 0 0 -.3749 1.236c-.0745 1.3389 1.5746 4.9717 3.174 7.9225 1.9039 3.5128 2.8143 4.9717 3.36 5.7776s1.9676 2.6935 3.4963 2.4425a1.6233 1.6233 0 0 0 1.2335-1.0972 5.0376 5.0376 0 0 0 .1156-.7407 5.6728 5.6728 0 0 1 -1.5548-1.6272zm19.3433-12.7055c.6112-1.2427.7069-2.1427.056-2.91a3.3567 3.3567 0 0 0 -.3614-.3036 2.3357 2.3357 0 0 0 -2.7956.5693 54.4733 54.4733 0 0 0 -3.7721 6.4595c-.4712.9671-2.7649 5.4057-2.8888 5.6536a5.6762 5.6762 0 0 0 -.8431 2.4425 2.6543 2.6543 0 0 0 .5361 1.6444 1.5675 1.5675 0 0 0 .9561.4324c1.1408.0992 1.8721-.6322 2.79-1.9046 1.8488-2.5643 5.5789-10.5709 6.3228-12.0835z"
}
