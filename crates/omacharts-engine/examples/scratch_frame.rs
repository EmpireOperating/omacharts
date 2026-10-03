use std::collections::HashMap;
use std::path::Path;

use omacharts_engine::omarchy;
use omacharts_engine::theme::{builtin_themes, contrast_ratio, delta_e, mix, theme_bars, Direction, Oklch, Theme};

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
    println!("{:<18} {:>5} {:>6} {:>6} | accent L C   | ring@.55: dBg  ctr  dAx  dGr  dBo  dCr  dUp  dDn | ring@.7: dBg ctr  dUp dDn | text ring@.3 dBg ctr dAx dUp dDn", "theme", "bgL", "dSurf", "dSVar");
    for (name, t) in &themes {
        let ui = &t.ui;
        let bars = theme_bars(t);
        let (up, down) = (bars.outline(Direction::Up), bars.outline(Direction::Down));
        let bg = Oklch::of(&ui.background).unwrap();
        let acc = Oklch::of(&ui.accent).unwrap();
        let ring = |alpha: f64| mix(&ui.accent, &ui.background, 1.0 - alpha);
        let r55 = ring(0.55);
        let r70 = ring(0.7);
        let tr = mix(&ui.text, &ui.background, 0.7);
        println!(
            "{:<18} {:>5.2} {:>6.3} {:>6.3} | {:.2} {:.3} | {:>4.3} {:>4.2} {:>4.3} {:>4.3} {:>4.3} {:>4.3} {:>4.3} {:>4.3} | {:>4.3} {:>4.2} {:>4.3} {:>4.3} | {:>4.3} {:>4.2} {:>4.3} {:>4.3} {:>4.3}",
            name,
            bg.l,
            delta_e(&ui.surface, &ui.background),
            delta_e(&ui.surface_variant, &ui.background),
            acc.l, acc.c,
            delta_e(&r55, &ui.background), contrast_ratio(&r55, &ui.background),
            delta_e(&r55, &ui.axis), delta_e(&r55, &ui.grid), delta_e(&r55, &ui.border), delta_e(&r55, &ui.crosshair),
            delta_e(&r55, up), delta_e(&r55, down),
            delta_e(&r70, &ui.background), contrast_ratio(&r70, &ui.background), delta_e(&r70, up), delta_e(&r70, down),
            delta_e(&tr, &ui.background), contrast_ratio(&tr, &ui.background), delta_e(&tr, &ui.axis), delta_e(&tr, up), delta_e(&tr, down),
        );
    }
}
