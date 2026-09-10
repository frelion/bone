use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match bone_tui::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("BONE could not start: {error}");
            ExitCode::FAILURE
        }
    }
}
