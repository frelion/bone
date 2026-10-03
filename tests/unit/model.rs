use super::*;

async fn lock_codex_source(
    source: &Path,
    lock_directory: &Path,
) -> Result<(CredentialLock, PathBuf)> {
    lock_codex_source_with_legacy(source, lock_directory, lock_directory).await
}

#[tokio::test]
async fn codex_source_uses_one_stable_lock_across_different_legacy_homes() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("auth.json");
    std::fs::write(&source, "{}").unwrap();
    let stable = root.path().join("os-home-locks");
    let home_a = root.path().join("home-a-locks");
    let home_b = root.path().join("home-b-locks");
    let (owner, _) = lock_codex_source_with_legacy(&source, &stable, &home_a)
        .await
        .unwrap();
    assert_eq!(owner.0.len(), 2);
    let waiting_source = source.clone();
    let waiter = tokio::spawn(async move {
        lock_codex_source_with_legacy(&waiting_source, &stable, &home_b).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(
        !waiter.is_finished(),
        "different HOME bypassed source ownership"
    );
    drop(owner);
    let (next, _) = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(next.0.len(), 2);
    drop(next);
    assert_eq!(std::fs::read_to_string(source).unwrap(), "{}");
}

#[cfg(unix)]
#[tokio::test]
async fn codex_source_deduplicates_stable_and_legacy_directory_aliases() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("auth.json");
    std::fs::write(&source, "{}").unwrap();
    let stable = root.path().join("locks");
    std::fs::create_dir(&stable).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&stable, &alias).unwrap();
    let (guard, _) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        lock_codex_source_with_legacy(&source, &stable, &alias),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(guard.0.len(), 1, "same physical lock was acquired twice");
    drop(guard);
}

#[tokio::test]
async fn legacy_codex_owner_blocks_new_lock_and_cancellation_releases_stable() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("auth.json");
    std::fs::write(&source, "{}").unwrap();
    let identity = format!(
        "{:x}",
        Sha256::digest(
            source
                .canonicalize()
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
        )
    );
    let stable = root.path().join("stable");
    let legacy = root.path().join("legacy");
    let legacy_path = legacy.join(format!("{identity}.lock"));
    let stable_path = stable.join(format!("{identity}.lock"));
    let old_owner = acquire_credential_lock(&legacy_path).await.unwrap();
    let waiter =
        tokio::spawn(async move { lock_codex_source_with_legacy(&source, &stable, &legacy).await });
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(
        !waiter.is_finished(),
        "new process bypassed legacy ownership"
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(60),
            acquire_credential_lock(&stable_path)
        )
        .await
        .is_err()
    );
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    let stable_owner = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        acquire_credential_lock(&stable_path),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(40),
            acquire_credential_lock(&legacy_path)
        )
        .await
        .is_err()
    );
    drop(stable_owner);
    drop(old_owner);
    let legacy_owner = acquire_credential_lock(&legacy_path).await.unwrap();
    drop(legacy_owner);
}

#[tokio::test]
async fn credential_queue_wait_does_not_consume_the_connection_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let profile = cached_subscription(directory.path());
    let owner = prepare(&profile, directory.path(), "test", "owner", "call-owner")
        .await
        .unwrap();
    let data = directory.path().to_owned();
    let queued_profile = profile.clone();
    let waiter = tokio::spawn(async move {
        let prepared = prepare(&queued_profile, &data, "test", "waiting", "call-waiting").await?;
        tokio::time::timeout(std::time::Duration::from_millis(100), prepared.connect()).await?
    });
    // Longer than the inference allowance, but no provider/authentication
    // request has begun. The queued invocation must still be eligible.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert!(!waiter.is_finished());
    drop(owner);
    let connection = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(connection.model.name(), "chatgpt");
    drop(connection);
}

#[tokio::test]
async fn cancelling_preparation_leaves_no_queued_credential_lease() {
    let directory = tempfile::tempdir().unwrap();
    let profile = cached_subscription(directory.path());
    let owner = prepare(&profile, directory.path(), "test", "owner", "call-owner")
        .await
        .unwrap();
    let data = directory.path().to_owned();
    let waiting_profile = profile.clone();
    let waiter = tokio::spawn(async move {
        prepare(&waiting_profile, &data, "test", "waiting", "call-waiting").await
    });
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    drop(owner);
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        prepare(&profile, directory.path(), "test", "next", "call-next"),
    )
    .await
    .unwrap()
    .unwrap();
    drop(next);
}

#[tokio::test]
async fn inference_timeout_after_preparation_releases_the_credential_lease() {
    use futures_util::StreamExt;
    use rig_core::providers::registry::ProviderRef;
    let directory = tempfile::tempdir().unwrap();
    let mut profile = cached_subscription(directory.path());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, connected) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        accepted.send(()).unwrap();
        // Hold the local response open past the inference deadline.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });
    let ModelReference::Registry(reference) = &profile.model else {
        panic!("registry fixture");
    };
    let ProviderConfig::OpenAi(config) = reference.config("") else {
        panic!("native ChatGPT fixture");
    };
    profile.model = ModelReference::Registry(
        ProviderRef::configured(
            ProviderConfig::OpenAi(config.with_base_url(format!("http://{address}/v1"))),
            "fixture",
        )
        .unwrap(),
    );
    let prepared = prepare(&profile, directory.path(), "test", "job", "call")
        .await
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_millis(120), async {
        let connection = prepared.connect().await?;
        let mut stream = connection
            .model
            .stream(rig_core::completion::CompletionRequest::new("fixture"))?;
        while let Some(item) = stream.next().await {
            item?;
        }
        stream.finish().await.map_err(anyhow::Error::from)
    })
    .await;
    assert!(result.is_err());
    assert!(
        connected.await.is_ok(),
        "native request reached the local fixture"
    );
    let (lease, _) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        lock_profile(directory.path(), "test"),
    )
    .await
    .unwrap()
    .unwrap();
    drop(lease);
    server.abort();
}

#[tokio::test]
async fn cancelling_a_profile_lock_waiter_leaves_no_hidden_lease() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = lock_profile(directory.path(), "fixture").await.unwrap();
    let data = directory.path().to_owned();
    let waiter = tokio::spawn(async move { lock_profile(&data, "fixture").await });
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!waiter.is_finished());
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    drop(owner);
    let (next, _) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        lock_profile(directory.path(), "fixture"),
    )
    .await
    .unwrap()
    .unwrap();
    drop(next);
}

#[tokio::test]
async fn reused_codex_source_serializes_across_distinct_profile_caches() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("codex-auth.json");
    std::fs::write(&source, "{}").unwrap();
    let locks = directory.path().join("shared-locks");
    let cache_a = directory.path().join("data-a");
    let cache_b = directory.path().join("data-b");
    // Independent BONE cache owners may exist, but the reused source's
    // physical lease is shared regardless of those data/profile identities.
    let (profile_a, _) = lock_profile(&cache_a, "alpha").await.unwrap();
    let (profile_b, _) = lock_profile(&cache_b, "beta").await.unwrap();
    let (source_owner, resolved) = lock_codex_source(&source, &locks).await.unwrap();
    let source_again = resolved.clone();
    let locks_again = locks.clone();
    let waiter = tokio::spawn(async move { lock_codex_source(&source_again, &locks_again).await });
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!waiter.is_finished());
    drop(profile_a);
    drop(profile_b);
    assert!(!waiter.is_finished());
    drop(source_owner);
    let (next, _) = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(next);
    assert!(std::fs::read_to_string(source).unwrap() == "{}");
}

#[cfg(unix)]
#[tokio::test]
async fn codex_source_symlink_alias_shares_lock_but_distinct_sources_do_not() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.json");
    let alias = directory.path().join("alias.json");
    let other = directory.path().join("other.json");
    std::fs::write(&source, "{}").unwrap();
    std::fs::write(&other, "{}").unwrap();
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let locks = directory.path().join("locks");
    let (owner, _) = lock_codex_source(&source, &locks).await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(60),
            lock_codex_source(&alias, &locks)
        )
        .await
        .is_err()
    );
    let (independent, _) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        lock_codex_source(&other, &locks),
    )
    .await
    .unwrap()
    .unwrap();
    drop(independent);
    drop(owner);
    let (next, _) = lock_codex_source(&alias, &locks).await.unwrap();
    drop(next);
}

#[test]
fn reuse_codex_login_reads_current_access_token_without_modifying_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth.json");
    let first = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"synthetic-first","account_id":"fixture-account","refresh_token":"never-used","id_token":"never-used"}}"#;
    std::fs::write(&path, first).unwrap();
    let AuthSource::AccessToken {
        access_token,
        account_id,
    } = load_codex_login(&path).unwrap()
    else {
        panic!("native AccessToken source required")
    };
    assert!(access_token == "synthetic-first");
    assert!(account_id.as_deref() == Some("fixture-account"));
    assert!(std::fs::read_to_string(&path).unwrap() == first);
    std::fs::write(
        &path,
        r#"{"tokens":{"access_token":"synthetic-current","account_id":"fixture-account"}}"#,
    )
    .unwrap();
    let AuthSource::AccessToken { access_token, .. } = load_codex_login(&path).unwrap() else {
        panic!("native AccessToken source required")
    };
    assert!(access_token == "synthetic-current");
}

#[test]
fn invalid_codex_login_errors_never_echo_credential_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth.json");
    std::fs::write(&path, "synthetic-sensitive-broken-data").unwrap();
    let message = format!("{:#}", load_codex_login(&path).unwrap_err());
    assert!(message.contains("Codex login"));
    assert!(!message.contains("synthetic-sensitive-broken-data"));
}

fn cached_subscription(data_dir: &Path) -> Profile {
    let path = auth_file(data_dir, "test").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // Entirely synthetic fixture authored by this test. No host cache is read.
    std::fs::write(
        path,
        r#"{"access_token":"bone-test-token","expires_at":4102444800}"#,
    )
    .unwrap();
    Profile::from_model("chatgpt:gpt-5.4").unwrap()
}

#[tokio::test]
async fn cached_subscription_guard_survives_native_stream_creation() {
    let directory = tempfile::tempdir().unwrap();
    let profile = cached_subscription(directory.path());
    let connection = connect(&profile, directory.path(), "test", "job-1", "call-1")
        .await
        .unwrap();
    assert_eq!(connection.model.name(), "chatgpt");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("profiles/test/auth.lock"))
        .unwrap();
    assert!(lock.try_lock_exclusive().is_err());
    // Native streaming is lazy; its owner retains the lock while runtime
    // holds the stream. No live provider or real credentials are involved.
    let stream = connection
        .model
        .stream(rig_core::completion::CompletionRequest::new("fixture"))
        .unwrap();
    assert!(lock.try_lock_exclusive().is_err());
    drop(stream);
    drop(connection);
    lock.try_lock_exclusive().unwrap();
    FileExt::unlock(&lock).unwrap();
}

#[tokio::test]
async fn invalid_oauth_cache_reports_login_without_cache_contents() {
    let directory = tempfile::tempdir().unwrap();
    let profile = cached_subscription(directory.path());
    std::fs::write(
        auth_file(directory.path(), "test").unwrap(),
        "synthetic-sensitive-invalid-cache",
    )
    .unwrap();
    let error = match connect(&profile, directory.path(), "test", "job", "call").await {
        Ok(_) => panic!("invalid cache accepted"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(message.contains("bone login --profile test"));
    assert!(!message.contains("synthetic-sensitive-invalid-cache"));
}

#[tokio::test]
async fn missing_oauth_cache_never_starts_device_flow() {
    let directory = tempfile::tempdir().unwrap();
    let profile = Profile::from_model("chatgpt:gpt-5.4").unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        connect(&profile, directory.path(), "test", "job", "call"),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert!(!auth_file(directory.path(), "test").unwrap().exists());
}

#[tokio::test]
async fn model_connections_require_persistable_invocation_ids() {
    let directory = tempfile::tempdir().unwrap();
    let profile = Profile::from_model("ollama:qwen3").unwrap();
    assert!(
        connect(&profile, directory.path(), "test", "", "call")
            .await
            .is_err()
    );
    assert!(
        connect(&profile, directory.path(), "test", "job", " ")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn private_profile_keys_are_isolated_bound_and_prepared_without_requests() {
    let data = tempfile::tempdir().unwrap();
    let profile = Profile::from_model("openai:future-model")
        .unwrap()
        .with_endpoint("http://127.0.0.1:9123/v1")
        .unwrap();
    assert!(!has_api_key(data.path(), "alpha").unwrap());
    save_api_key(data.path(), "alpha", &profile, "synthetic-alpha-key").unwrap();
    for invalid in [
        "   ",
        "synthetic\tkey",
        "synthetic\0key",
        "synthetic\nkey",
        "synthetic\u{7f}key",
    ] {
        assert!(save_api_key(data.path(), "alpha", &profile, invalid).is_err());
    }
    assert!(save_api_key(data.path(), "alpha", &profile, &"x".repeat(1024 * 1024)).is_err());
    save_api_key(data.path(), "beta", &profile, "synthetic-beta-key").unwrap();
    assert!(has_api_key(data.path(), "alpha").unwrap());
    let prepared = prepare(&profile, data.path(), "alpha", "job", "call")
        .await
        .unwrap();
    assert!(prepared.api_key.as_deref() == Some("synthetic-alpha-key"));
    let beta = prepare(&profile, data.path(), "beta", "job", "call")
        .await
        .unwrap();
    assert!(beta.api_key.as_deref() == Some("synthetic-beta-key"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(api_key_file(data.path(), "alpha").unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let different_host = profile.with_endpoint("http://127.0.0.1:9234/v1").unwrap();
    assert!(
        prepare(&different_host, data.path(), "alpha", "job", "call")
            .await
            .is_err()
    );
    let different_provider = profile.with_model("anthropic:another-model").unwrap();
    assert!(
        prepare(&different_provider, data.path(), "alpha", "job", "call")
            .await
            .is_err()
    );
    let different_model = profile.with_model("openai:another-model").unwrap();
    assert!(
        prepare(&different_model, data.path(), "alpha", "job", "call")
            .await
            .is_ok()
    );
    // An explicitly selected but absent variable must fail; it must never
    // silently fall back to the saved key or a different provider's environment.
    let mut explicit = profile.clone();
    explicit.credential_env = Some(format!("BONE_ABSENT_KEY_{}", uuid::Uuid::new_v4().simple()));
    assert!(
        prepare(&explicit, data.path(), "alpha", "job", "call")
            .await
            .is_err()
    );
    // PATH is a guaranteed nonsecret process setting; use it only to verify
    // explicit-variable precedence without mutating the process environment.
    explicit.credential_env = Some("PATH".into());
    let overridden = prepare(&explicit, data.path(), "alpha", "job", "call")
        .await
        .unwrap();
    assert!(overridden.api_key == Some(std::env::var("PATH").unwrap()));
    let config_text = toml::to_string(&profile).unwrap();
    assert!(!config_text.contains("synthetic-alpha-key"));
    remove_api_key(data.path(), "alpha").unwrap();
    assert!(!has_api_key(data.path(), "alpha").unwrap());
    assert!(has_api_key(data.path(), "beta").unwrap());
}

#[test]
fn local_credential_presence_never_parses_or_exposes_token_text() {
    let data = tempfile::tempdir().unwrap();
    let profile = cached_subscription(data.path());
    let path = auth_file(data.path(), "test").unwrap();
    std::fs::write(&path, "synthetic-invalid-token-text").unwrap();
    assert!(has_login(&profile, data.path(), "test").unwrap());
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "synthetic-invalid-token-text"
    );
    let key_path = api_key_file(data.path(), "test").unwrap();
    std::fs::write(&key_path, "not-even-valid-key-json").unwrap();
    assert!(has_api_key(data.path(), "test").unwrap());
}

#[cfg(unix)]
#[test]
fn profile_key_paths_cannot_follow_symlinks_outside_the_data_directory() {
    let data = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let profile = Profile::from_model("openai:fixture").unwrap();
    std::fs::create_dir(data.path().join("profiles")).unwrap();
    std::os::unix::fs::symlink(outside.path(), data.path().join("profiles/alpha")).unwrap();
    assert!(save_api_key(data.path(), "alpha", &profile, "synthetic-secret").is_err());
    assert!(has_api_key(data.path(), "alpha").is_err());
    assert!(!outside.path().join("api-key").exists());
    std::fs::create_dir(data.path().join("profiles/beta")).unwrap();
    let outside_file = outside.path().join("keep");
    std::fs::write(&outside_file, "keep-original").unwrap();
    std::os::unix::fs::symlink(&outside_file, data.path().join("profiles/beta/api-key")).unwrap();
    assert!(save_api_key(data.path(), "beta", &profile, "synthetic-secret").is_err());
    assert!(remove_api_key(data.path(), "beta").is_err());
    assert_eq!(
        std::fs::read_to_string(outside_file).unwrap(),
        "keep-original"
    );
}

#[tokio::test]
async fn cancelling_native_login_preserves_old_cache_and_releases_profile_lease() {
    use rig_core::{
        http_client::{HeaderMap, HttpMiddleware, Method, Uri},
        wasm_compat::WasmBoxedFuture,
    };
    struct BlockAuth(std::sync::Arc<tokio::sync::Notify>);
    impl HttpMiddleware for BlockAuth {
        fn before_request_headers<'a>(
            &'a self,
            _: &'a Method,
            _: &'a Uri,
            _: &'a mut HeaderMap,
        ) -> WasmBoxedFuture<'a, rig_core::http_client::Result<()>> {
            Box::pin(async move {
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }
    let data = tempfile::tempdir().unwrap();
    let profile = cached_subscription(data.path());
    let auth_path = auth_file(data.path(), "test").unwrap();
    let original = std::fs::read(&auth_path).unwrap();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let http =
        DynHttpClient::new(rig_reqwest::shared()).with_middleware(BlockAuth(started.clone()));
    let directory = data.path().to_owned();
    let login = tokio::spawn(async move {
        login_with(
            &profile,
            &directory,
            "test",
            http,
            DeviceCodeHandler::new(|_| {}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    login.abort();
    assert!(login.await.unwrap_err().is_cancelled());
    assert_eq!(std::fs::read(&auth_path).unwrap(), original);
    assert!(
        std::fs::read_dir(auth_path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("auth-login-"))
    );
    let (lease, _) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        lock_profile(data.path(), "test"),
    )
    .await
    .unwrap()
    .unwrap();
    drop(lease);
}
