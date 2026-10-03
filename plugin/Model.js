// Shaping the CLI's output for the panel.
//
// Kept out of the QML so it can be reasoned about on its own: the panel is a
// list of rows, and working out what a row says is not a layout problem.

.pragma library

function parse(text) {
  if (!text || !text.trim()) return { sections: [], error: "" }
  try {
    var parsed = JSON.parse(text)
    return {
      sections: Array.isArray(parsed.sections) ? parsed.sections : [],
      // The app derives these from the desktop theme and sends them with the
      // data, so the bar and the window cannot disagree about what up looks
      // like.
      colors: parsed.colors || null,
      updatedAt: parsed.updatedAt || 0,
      error: parsed.error || ""
    }
  } catch (e) {
    return { sections: [], error: "unreadable output" }
  }
}

// Flatten sections into rows the list can render directly: headers and
// entries in one stream, so the list does not have to nest.
function rows(sections) {
  var out = []
  for (var s = 0; s < sections.length; s++) {
    var section = sections[s]
    var entries = Array.isArray(section.entries) ? section.entries : []
    if (!entries.length) continue
    if (!section.root && section.name) {
      out.push({ header: true, name: section.name })
    }
    for (var e = 0; e < entries.length; e++) {
      var entry = entries[e]
      out.push({
        header: false,
        symbol: entry.symbol,
        suffix: entry.suffix || "",
        display: entry.display,
        name: entry.name,
        last: entry.last,
        change: entry.change,
        changePct: entry.changePct,
        hasQuote: entry.last !== null && entry.last !== undefined,
        spark: Array.isArray(entry.spark) ? entry.spark : []
      })
    }
  }
  return out
}

// Which rows the keyboard may land on. Headers are scenery.
function selectableIndexes(rows) {
  var out = []
  for (var i = 0; i < rows.length; i++) if (!rows[i].header) out.push(i)
  return out
}

function quoteCount(rows) {
  var n = 0
  for (var i = 0; i < rows.length; i++) if (!rows[i].header && rows[i].hasQuote) n++
  return n
}

// The one quote the bar itself shows: the first entry that actually has one.
function headline(sections) {
  var all = rows(sections)
  for (var i = 0; i < all.length; i++) {
    if (!all[i].header && all[i].hasQuote) return all[i]
  }
  return null
}

// Prices of different sizes want different precision. A currency pair moving
// 0.0008 shown to two decimals reads as "nothing happened".
function decimals(price) {
  var size = Math.abs(price)
  if (size >= 20) return 2
  if (size >= 1) return 4
  return 6
}

function formatPrice(value) {
  if (value === null || value === undefined) return "–"
  return Number(value).toFixed(decimals(value))
}

function formatPercent(value) {
  if (value === null || value === undefined) return ""
  var sign = value > 0 ? "+" : ""
  return sign + Number(value).toFixed(2) + "%"
}

function formatChange(value, price) {
  if (value === null || value === undefined) return ""
  var sign = value > 0 ? "+" : ""
  return sign + Number(value).toFixed(decimals(price))
}

// Black or white, whichever can be read on `hex`.
//
// The pill takes its colour from the theme, and a theme is free to make its
// up and down dark. Text painted a fixed near-black on a dark pill is a
// percentage nobody can read — which is exactly what happened on a theme whose
// green and red are purple and magenta.
//
// Mirrors the app's own readable_on: same weights, same threshold, so a pill
// in the bar and a label in the window never disagree about which way to go.
function readableOn(hex) {
  var rgb = parseHex(hex)
  if (!rgb) return "#ffffff"
  var luminance = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
  return luminance > 0.55 ? "#000000" : "#ffffff"
}

function parseHex(hex) {
  if (typeof hex !== "string") return null
  var text = hex.trim().replace("#", "")
  if (text.length === 3) {
    text = text[0] + text[0] + text[1] + text[1] + text[2] + text[2]
  }
  // QML stringifies a colour as #AARRGGBB, so the colour is the last six
  // digits, not the first.
  if (text.length === 8) text = text.substring(2)
  if (text.length < 6) return null
  var value = parseInt(text.substring(0, 6), 16)
  if (isNaN(value)) return null
  return [
    ((value >> 16) & 255) / 255,
    ((value >> 8) & 255) / 255,
    (value & 255) / 255
  ]
}

// Up, down, or neither. The panel colours from this rather than comparing
// numbers in three places.
function direction(value) {
  if (value === null || value === undefined) return "flat"
  if (value > 0) return "up"
  if (value < 0) return "down"
  return "flat"
}
