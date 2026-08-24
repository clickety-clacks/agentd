fn main() {
    if let Err(error) = agentd::cli::run(std::env::args_os().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
