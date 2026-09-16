use std::process::ExitCode;

mod headless;
#[path = "terminal/main_error.rs"]
mod terminal_error;

#[tokio::main]
async fn main() -> ExitCode {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|value| value == "run") {
        return headless::run(&args[1..]).await;
    }
    if args.first().is_some_and(|value| value == "credentials") {
        return headless::credentials(&args[1..]).await;
    }
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        print!("{}", headless::ROOT_HELP);
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 && (args[0] == "--version" || args[0] == "-V") {
        println!("bone {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match bone_tui::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            terminal_error::write(&format_args!("BONE could not start: {error}"));
            ExitCode::FAILURE
        }
    }
}
