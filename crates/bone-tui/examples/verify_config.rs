//! Inspect only public, non-secret App configuration for a manual TUI check.
use bone_app::{App, AppOptions, ConfigScope};
#[tokio::main]
async fn main() -> bone_app::Result<()> {
    let app = App::open(AppOptions::platform_default()?).await?;
    for profile in app.profiles().await? {
        println!("profile: {} ({})", profile.id.as_str(), profile.label);
    }
    let config = app.config(ConfigScope::User).await?;
    if let Some(model) = config.worker {
        println!("worker: {} / {}", model.profile.as_str(), model.model);
    }
    app.shutdown().await.map(|_| ())
}
