use std::fs;

use bone_app::{ChatGptCredentials, CredentialError};
use bone_llm::service::chatgpt_subscription::ChatGptAuthCache;

fn credentials() -> (tempfile::TempDir, ChatGptCredentials) {
    let temporary = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    fs::set_permissions(
        temporary.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let credentials = ChatGptCredentials::at(temporary.path().join("config")).unwrap();
    (temporary, credentials)
}

#[test]
fn creates_a_private_exclusive_cache() {
    let (_temporary, credentials) = credentials();
    let lease = credentials.acquire().unwrap();
    assert_eq!(fs::read_to_string(lease.auth_file()).unwrap(), "{}");
    assert!(matches!(credentials.acquire(), Err(CredentialError::Busy)));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(lease.auth_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn clear_is_a_noop_for_a_never_initialized_provider() {
    let (temporary, credentials) = credentials();
    credentials.clear().unwrap();
    assert!(!temporary.path().join("config").exists());
}

#[test]
fn clear_observes_a_lease_when_rig_has_removed_the_cache() {
    let (_temporary, credentials) = credentials();
    let lease = credentials.acquire().unwrap();
    fs::remove_file(lease.auth_file()).unwrap();
    assert_eq!(credentials.clear(), Err(CredentialError::Busy));
    drop(lease);
    credentials.clear().unwrap();
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_cache() {
    let (temporary, credentials) = credentials();
    let config = temporary.path().join("config");
    let providers = config.join("providers");
    let directory = providers.join("chatgpt-subscription");
    fs::create_dir_all(&directory).unwrap();
    for path in [&config, &providers, &directory] {
        fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    }
    std::os::unix::fs::symlink("/not-a-bone-cache", directory.join("auth.json")).unwrap();

    assert!(matches!(
        credentials.acquire(),
        Err(CredentialError::Unavailable)
    ));
}
