use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::{ChatGptAuthCache, DeviceCodePrompt, Error, LeasedModel};

#[test]
fn service_errors_and_device_codes_are_redacted() {
    let error = Error::AuthorizationFailed;
    let rendered = format!("{error:?}: {error}");
    assert!(!rendered.contains("sentinel-secret-token"));
    assert!(!rendered.contains("Authorization: Bearer"));

    let prompt = DeviceCodePrompt {
        verification_uri: "https://auth.openai.com/codex/device".to_owned(),
        user_code: "SENTINEL-CODE".to_owned(),
    };
    let rendered = format!("{prompt:?}");
    assert!(rendered.contains("auth.openai.com"));
    assert!(!rendered.contains("SENTINEL-CODE"));
}

struct DropProbe {
    dropped: Arc<AtomicBool>,
    path: PathBuf,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

impl ChatGptAuthCache for DropProbe {
    fn auth_file(&self) -> &Path {
        &self.path
    }
}

#[test]
fn leased_models_keep_the_auth_cache_alive() {
    let dropped = Arc::new(AtomicBool::new(false));
    let cache: Arc<dyn ChatGptAuthCache> = Arc::new(DropProbe {
        dropped: Arc::clone(&dropped),
        path: PathBuf::from("/private/cache/auth.json"),
    });
    let model = LeasedModel {
        inner: (),
        _auth: Arc::clone(&cache),
    };

    drop(cache);
    assert!(!dropped.load(Ordering::Acquire));
    drop(model);
    assert!(dropped.load(Ordering::Acquire));
}
