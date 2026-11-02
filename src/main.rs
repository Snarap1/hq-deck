fn main() -> anyhow::Result<()> {
    if std::env::args_os().skip(1).any(|arg| arg == "--print-config") {
        if let Err(error) = hq_deck::config::print_plugin_config() {
            eprintln!("hq-deck: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    hq_deck::run()
}
