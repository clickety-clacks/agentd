fn main() {
    let mut arguments = std::env::args_os().skip(1);
    match (arguments.next(), arguments.next()) {
        (None, None) => {
            if let Err(error) = agentd::attention::run() {
                eprintln!("agentd-attention: {error}");
                std::process::exit(1);
            }
        }
        (Some(flag), None) if flag == "--version" => {
            println!("agentd-attention {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("usage: agentd-attention [--version]");
            std::process::exit(2);
        }
    }
}
