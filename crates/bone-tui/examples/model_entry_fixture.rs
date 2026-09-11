//! Credential-free fixture for manually exercising /model in an isolated workspace.
//! cargo run -p bone-tui --example model_entry_fixture -- seed DATA_DIR WORKSPACE
//! Use inspect instead of seed after editing through the real TUI.
use bone_app::{App, AppOptions, ConfigScope, EndpointConfig, Profile, ProfileId};
#[tokio::main]
async fn main() -> bone_app::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        args.len() == 4 && matches!(args[1].as_str(), "seed" | "inspect"),
        "expected seed|inspect DATA_DIR WORKSPACE"
    );
    let app = App::open(AppOptions::new(&args[2])).await?;
    let workspace = app.open_workspace(&args[3]).await?;
    if args[1] == "seed" {
        app.save_profile(
            Profile::new(
                ProfileId::new("api-verification").unwrap(),
                "API verification",
                EndpointConfig::OpenAiResponses {
                    base_url: Some("https://example.invalid/v1".into()),
                },
            )
            .unwrap(),
        )
        .await?;
    }
    for profile in app.profiles().await? {
        println!(
            "{}: {} ({:?})",
            profile.id.as_str(),
            profile.label,
            profile.endpoint
        );
    }
    println!(
        "Workspace model: {:?}",
        app.config(ConfigScope::Workspace(workspace.id))
            .await?
            .worker
    );
    app.shutdown().await.map(|_| ())
}
