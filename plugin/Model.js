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
        display: entry.display,
        name: entry.name,
        last: entry.last,
        change: entry.change,
        changePct: entry.changePct,
        hasQuote: entry.last !== null && entry.last !== undefined
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

// Up, down, or neither. The panel colours from this rather than comparing
// numbers in three places.
function direction(value) {
  if (value === null || value === undefined) return "flat"
  if (value > 0) return "up"
  if (value < 0) return "down"
  return "flat"
}
