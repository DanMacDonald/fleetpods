import QtQuick

// The same arc the bar draws around the small icon. Panel owns the sweep.
Item {
  id: ring

  property color ringColor: "#e6e6e6"
  property bool spinning: false
  property real sweep: 0.15
  property real doneSweep: 0.15
  property real doneOpacity: 0

  function paintRing(ctx, item, arc, bright) {
    ctx.reset()
    ctx.lineWidth = Math.max(1.5, item.width * 0.045)
    var radius = (Math.min(item.width, item.height) - ctx.lineWidth) / 2 - 1
    ctx.strokeStyle = ring.ringColor
    ctx.lineCap = "round"
    ctx.globalAlpha = 0.2
    ctx.beginPath()
    ctx.arc(item.width / 2, item.height / 2, radius, 0, Math.PI * 2)
    ctx.stroke()
    if (arc <= 0) return
    ctx.globalAlpha = bright
    ctx.beginPath()
    // Canvas angles run clockwise from 3 o'clock, so pi / 2 starts the sweep at the icon's base.
    ctx.arc(item.width / 2, item.height / 2, radius, Math.PI / 2, Math.PI / 2 + arc)
    ctx.stroke()
  }

  Canvas {
    id: spinRing
    anchors.fill: parent
    visible: ring.spinning
    onVisibleChanged: requestPaint()
    Connections {
      target: ring
      function onSweepChanged() { spinRing.requestPaint() }
      function onSpinningChanged() { spinRing.requestPaint() }
    }
    onPaint: ring.paintRing(getContext("2d"), spinRing, ring.sweep, 1)
  }

  Canvas {
    id: doneRing
    anchors.fill: parent
    opacity: ring.doneOpacity
    visible: opacity > 0
    onOpacityChanged: if (visible) requestPaint()
    onVisibleChanged: requestPaint()
    Connections {
      target: ring
      function onDoneSweepChanged() { if (doneRing.visible) doneRing.requestPaint() }
    }
    onPaint: ring.paintRing(getContext("2d"), doneRing, ring.doneSweep, 1)
  }
}
