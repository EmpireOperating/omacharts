use std::collections::HashMap;
use std::path::Path;

use omacharts_engine::omarchy;
use omacharts_engine::theme::{builtin_themes, contrast_ratio, delta_e, theme_bars, Direction, Oklch, Theme};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omarchy");
    let mut themes: Vec<(String, Theme)> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let keys: HashMap<String, String> = omarchy::parse(&std::fs::read_to_string(&path).unwrap());
        themes.push((name.clone(), omarchy::derive(&keys, &name)));
    }
    themes.sort_by(|a, b| a.0.cmp(&b.0));
    for t in builtin_themes() {
        themes.push((t.name.clone(), t));
    }
    println!("{:<18} bg      gutter  dBg   lifted | ring    L    C   ctr  dAx   dGr   dBo   dCr   dUp   dDn  | hover", "theme");
    for (name, t) in &themes {
        let ui = &t.ui;
        let bars = theme_bars(t);
        let (up, down) = (bars.outline(Direction::Up), bars.outline(Direction::Down));
        let f = t.frame(&bars);
        let r = Oklch::of(&f.focus).unwrap();
        println!(
            "{:<18} {} {} {:.3} {:<6} | {} {:.2} {:.3} {:.2} {:.3} {:.3} {:.3} {:.3} {:.3} {:.3} | {}",
            name, ui.background, f.gutter, delta_e(&f.gutter, &ui.background), f.gutter != ui.surface,
            f.focus, r.l, r.c, contrast_ratio(&f.focus, &ui.background),
            delta_e(&f.focus, &ui.axis), delta_e(&f.focus, &ui.grid), delta_e(&f.focus, &ui.border), delta_e(&f.focus, &ui.crosshair),
            delta_e(&f.focus, up), delta_e(&f.focus, down), f.gutter_hover,
        );
    }
}
