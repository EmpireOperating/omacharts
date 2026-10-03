fn main() {
    let home = omacharts::store::home();
    match omacharts::bar_plugin::install(&home) {
        Ok(()) => println!("installed; in bar: {}", omacharts::bar_plugin::installed(&home)),
        Err(e) => println!("failed: {e}"),
    }
}
