use bone_agent::{Effort, SystemConfig, TaskConfig};
use bone_config::{ConfigManager, ConfigSection};
use serde_json::{Value, json};

fn settings() -> Value {
    json!({
        "coordinator": {"model": "system-coordinator", "effort": "low", "timeout_seconds": 15},
        "default_solver": {"model": "default-solver", "effort": "high"}
    })
}

#[test]
fn task_overrides_leave_the_system_coordinator_and_persisted_defaults_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let store = ConfigManager::builder()
        .register::<SystemConfig>()
        .unwrap()
        .build(&path)
        .unwrap();
    store
        .set_value(
            SystemConfig::KEY,
            settings(),
            store.snapshot().unwrap().revision(),
        )
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let system = SystemConfig::from_snapshot(&store.snapshot().unwrap()).unwrap();
    let task = TaskConfig {
        model: Some("task-solver".into()),
        effort: Some(Effort::Max),
        timeout_seconds: Some(300),
    };
    let solver = system.solver_for(&task).unwrap();
    assert_eq!(solver.model, "task-solver");
    assert_eq!(solver.effort, Some(Effort::Max));
    assert_eq!(solver.timeout().as_secs(), 300);
    assert_eq!(system.coordinator.model, "system-coordinator");
    assert_eq!(system.coordinator.effort, Some(Effort::Low));
    assert_eq!(system.coordinator.timeout().as_secs(), 15);
    assert_eq!(
        system.solver_for(&TaskConfig::default()).unwrap(),
        system.default_solver
    );
    assert_eq!(system.default_solver.timeout().as_secs(), 120);
    assert_eq!(system.soft_deadline_seconds, 30);
    assert_eq!(system.shutdown_grace_seconds, 5);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn invalid_system_settings_are_rejected_by_the_configuration_service() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigManager::builder()
        .register::<SystemConfig>()
        .unwrap()
        .build(directory.path().join("config.json"))
        .unwrap();
    let mut missing_coordinator = settings();
    missing_coordinator
        .as_object_mut()
        .unwrap()
        .remove("coordinator");
    let mut empty_model = settings();
    empty_model["coordinator"]["model"] = json!("  ");
    let mut zero_timeout = settings();
    zero_timeout["default_solver"]["timeout_seconds"] = json!(0);
    let mut unknown_effort = settings();
    unknown_effort["coordinator"]["effort"] = json!("unsupported-effort");
    let mut misspelled_setting = settings();
    misspelled_setting["coordinator"]["timeot_seconds"] = json!(10);
    let mut zero_soft_deadline = settings();
    zero_soft_deadline["soft_deadline_seconds"] = json!(0);
    let mut zero_shutdown_grace = settings();
    zero_shutdown_grace["shutdown_grace_seconds"] = json!(0);
    for value in [
        missing_coordinator,
        empty_model,
        zero_timeout,
        unknown_effort,
        misspelled_setting,
        zero_soft_deadline,
        zero_shutdown_grace,
    ] {
        assert!(
            store.validate_value(SystemConfig::KEY, &value).is_err(),
            "accepted {value}"
        );
    }
}

#[test]
fn a_task_cannot_supply_coordinator_settings() {
    let result = serde_json::from_value::<TaskConfig>(json!({
        "model": "task-solver",
        "coordinator": {"model": "task-selected-coordinator"}
    }));
    assert!(result.is_err());
    let system: SystemConfig = serde_json::from_value(settings()).unwrap();
    assert!(
        system
            .solver_for(&TaskConfig {
                timeout_seconds: Some(0),
                ..TaskConfig::default()
            })
            .is_err()
    );
}
