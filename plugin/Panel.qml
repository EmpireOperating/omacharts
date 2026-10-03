import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui
import "Model.js" as Model
import "ServiceHost.js" as ServiceHost

// The watchlist, in the bar.
//
// Modelled on the Stocks app everyone already knows: one row per symbol, the
// ticker and its name on the left, the price and a coloured change pill on the
// right. Nothing else — a bar panel is a glance, not a workspace.
Panel {
  id: root
  moduleName: "jorgemanrubia.omacharts"
  ipcTarget: "jorgemanrubia.omacharts"
  // Its own handler rather than the base one, so the app can push a refresh
  // the moment the watchlist changes instead of leaving the bar a timer
  // behind. Also lets the panel be opened from a keybinding:
  //   omarchy-shell jorgemanrubia.omacharts toggle
  manageIpc: false

  property int selectedIndex: 0

  // The bar lays widgets out by their implicit size. Without this the root is
  // zero by zero, the button anchored to it is too, and the icon never draws.
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  // Stands in until the host has built the real one, so every binding below
  // can read the same shape rather than guarding for null.
  QtObject {
    id: dummyService
    property var sections: []
    property string lastError: ""
    property bool refreshing: false
    property double updatedAt: 0
    function refresh() {}
    function refreshIfStale() {}
    function openApp() {}
  }

  readonly property var hostedService: {
    var _ = bar && bar.shell ? bar.shell._services : null
    return ServiceHost.hostedService(bar)
  }
  readonly property var service: hostedService !== null ? hostedService : dummyService
  readonly property var rows: Model.rows(service.sections)
  readonly property int quoteCount: Model.quoteCount(rows)
  readonly property var headline: Model.headline(service.sections)

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.6)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  // Up and down come from the app, which derives them from the desktop theme.
  // Hardcoding them here would leave the bar on last year's palette the moment
  // anyone changed their theme.
  readonly property var palette: service.colors
  readonly property color up: palette && palette.up ? palette.up : "#3fb950"
  readonly property color down: palette && palette.down ? palette.down : "#f85149"

  function colorFor(change) {
    var way = Model.direction(change)
    if (way === "up") return up
    if (way === "down") return down
    return dim
  }

  function moveSelection(delta) {
    var picks = Model.selectableIndexes(rows)
    if (!picks.length) return
    var at = picks.indexOf(selectedIndex)
    if (at < 0) at = 0
    else at = Math.max(0, Math.min(picks.length - 1, at + delta))
    selectedIndex = picks[at]
  }

  onOpenedChanged: if (opened) service.refreshIfStale()

  IpcHandler {
    target: "jorgemanrubia.omacharts"

    function open(): void { root.open() }
    function close(): void { root.close() }
    function toggle(): void { root.toggle() }
    function refresh(): void { root.service.refresh() }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: Component {
      Item {
        OmachartsIcon {
          anchors.centerIn: parent
          iconSize: Style.space(13)
          color: root.foreground
        }
      }
    }
    tooltipText: root.service.refreshing
      ? "Updating the watchlist"
      : (root.headline
        ? root.headline.display + " " + Model.formatPercent(root.headline.changePct)
        : "omacharts watchlist")
    onPressed: function (buttonCode) {
      if (buttonCode === Qt.RightButton || buttonCode === Qt.MiddleButton) root.service.refresh()
      else root.toggle()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(330))
    // Asked for what the rows want, capped — the scroller takes it from there.
    contentHeight: panel.fittedContentHeight(
      Math.min(rowsColumn.implicitHeight + Style.space(100), Style.space(620)),
      Style.space(620))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onMoveRequested: function (dx, dy) { if (dy !== 0) root.moveSelection(dy) }
      onActivateRequested: { root.service.openApp(); root.close() }
      onCloseRequested: root.close()
      onTabRequested: function (direction) { root.switchPanel(direction) }
      onTextKey: function (text) { if (text === "r" || text === "R") root.service.refresh() }

      ColumnLayout {
        id: content
        anchors.fill: parent
        spacing: Style.space(6)

        RowLayout {
          Layout.fillWidth: true

          Text {
            text: "Watchlist"
            font.family: root.fontFamily
            font.pixelSize: Style.space(14)
            font.bold: true
            color: root.foreground
          }

          Item { Layout.fillWidth: true }

          Text {
            visible: root.service.refreshing
            text: "updating…"
            font.family: root.fontFamily
            font.pixelSize: Style.space(11)
            color: root.dim
          }
        }

        Text {
          Layout.fillWidth: true
          visible: root.service.lastError !== ""
          text: root.service.lastError
          wrapMode: Text.WordWrap
          font.family: root.fontFamily
          font.pixelSize: Style.space(11)
          color: root.dim
        }

        Text {
          Layout.fillWidth: true
          visible: root.rows.length === 0 && root.service.lastError === ""
          text: "Nothing on the watchlist yet."
          font.family: root.fontFamily
          font.pixelSize: Style.space(12)
          color: root.dim
        }

        // A watchlist outgrows any panel worth putting on a bar, so the rows
        // scroll and the title and the way out stay where you left them.
        ScrollView {
          Layout.fillWidth: true
          Layout.fillHeight: true
          clip: true
          ScrollBar.horizontal.policy: ScrollBar.AlwaysOff

          ColumnLayout {
            id: rowsColumn
            width: Math.max(0, parent.width)
            spacing: Style.space(6)

        Repeater {
          model: root.rows

          delegate: Item {
            Layout.fillWidth: true
            implicitHeight: modelData.header ? Style.space(24) : Style.space(40)

            Text {
              visible: modelData.header
              anchors.left: parent.left
              anchors.bottom: parent.bottom
              anchors.bottomMargin: Style.space(4)
              text: modelData.header ? modelData.name.toUpperCase() : ""
              font.family: root.fontFamily
              font.pixelSize: Style.space(10)
              font.letterSpacing: 0.6
              color: root.dim
            }

            Rectangle {
              anchors.fill: parent
              anchors.leftMargin: -Style.space(6)
              anchors.rightMargin: -Style.space(6)
              radius: Style.space(6)
              visible: !modelData.header
              color: root.foreground
              opacity: index === root.selectedIndex || hover.hovered ? 0.07 : 0
              Behavior on opacity { NumberAnimation { duration: 90 } }
            }

            HoverHandler {
              id: hover
              enabled: !modelData.header
              onHoveredChanged: if (hovered) root.selectedIndex = index
            }

            TapHandler {
              enabled: !modelData.header
              onTapped: { root.service.openApp(); root.close() }
            }

            RowLayout {
              visible: !modelData.header
              anchors.fill: parent
              spacing: Style.space(8)

              ColumnLayout {
                spacing: 0
                Text {
                  text: modelData.display || ""
                  font.family: root.fontFamily
                  font.pixelSize: Style.space(13)
                  font.bold: true
                  color: root.foreground
                }
                Text {
                  Layout.maximumWidth: Style.space(150)
                  text: modelData.name || ""
                  elide: Text.ElideRight
                  font.family: root.fontFamily
                  font.pixelSize: Style.space(10)
                  color: root.dim
                }
              }

              Item { Layout.fillWidth: true }

              Sparkline {
                visible: modelData.spark && modelData.spark.length > 1
                Layout.preferredWidth: Style.space(52)
                Layout.preferredHeight: Style.space(22)
                points: modelData.spark || []
                stroke: root.colorFor(modelData.change)
              }

              Text {
                text: Model.formatPrice(modelData.last)
                font.family: root.fontFamily
                font.pixelSize: Style.space(13)
                color: modelData.hasQuote === true ? root.foreground : root.dim
              }

              // The pill is the thing you actually read, which is why it is
              // the only coloured object on the row.
              Rectangle {
                visible: modelData.hasQuote === true
                implicitWidth: Math.max(Style.space(58), percent.implicitWidth + Style.space(12))
                implicitHeight: Style.space(20)
                radius: Style.space(5)
                color: root.colorFor(modelData.change)

                Text {
                  id: percent
                  anchors.centerIn: parent
                  text: Model.formatPercent(modelData.changePct)
                  font.family: root.fontFamily
                  font.pixelSize: Style.space(11)
                  font.bold: true
                  color: Model.direction(modelData.change) === "flat"
                    ? root.foreground : "#0b0d10"
                }
              }
            }
          }
        }
          }
        }

        Rectangle {
          Layout.fillWidth: true
          implicitHeight: 1
          color: Qt.rgba(root.foreground.r, root.foreground.g, root.foreground.b, 0.12)
        }

        Item {
          Layout.fillWidth: true
          implicitHeight: Style.space(32)

          Rectangle {
            anchors.fill: parent
            anchors.leftMargin: -Style.space(6)
            anchors.rightMargin: -Style.space(6)
            radius: Style.space(6)
            color: root.foreground
            opacity: openHover.hovered ? 0.07 : 0
            Behavior on opacity { NumberAnimation { duration: 90 } }
          }

          HoverHandler { id: openHover }
          TapHandler { onTapped: { root.service.openApp(); root.close() } }

          Text {
            anchors.centerIn: parent
            text: "Open Omacharts"
            font.family: root.fontFamily
            font.pixelSize: Style.space(12)
            color: root.foreground
          }
        }
      }
    }
  }
}
