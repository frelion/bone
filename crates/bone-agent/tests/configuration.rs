use bone_agent::{
    Effort, ModelSettings, ModelSettingsError, ResolvedAgentRuntimeConfigError, SystemConfig,
    SystemConfigError,
};
use bone_tools::ToolLimits;
use serde_json::json;

fn settings() -> SystemConfig {
    serde_json::from_value(json!({
        "coordinator": {"model": "system-coordinator", "effort": "low", "timeout_seconds": 15},
        "default_solver": {"model": "default-solver", "effort": "high"}
    }))
    .unwrap()
}

#[test]
fn a_complete_solver_override_leaves_system_defaults_unchanged() {
    let system = settings();
    let runtime = system
        .resolve(
            ToolLimits::default(),
            Some(ModelSettings {
                model: "session-solver".into(),
                effort: Some(Effort::Max),
                timeout_seconds: 300,
            }),
        )
        .unwrap();

    assert_eq!(runtime.solver().model, "session-solver");
    assert_eq!(runtime.solver().effort, Some(Effort::Max));
    assert_eq!(runtime.deadlines().work_timeout().as_secs(), 300);
    assert_eq!(runtime.coordinator().model, "system-coordinator");
    assert_eq!(runtime.coordinator().effort, Some(Effort::Low));
    assert_eq!(runtime.deadlines().review_timeout().as_secs(), 15);
    assert_eq!(system.default_solver.model, "default-solver");
    assert_eq!(system.default_solver.timeout().as_secs(), 120);
    assert_eq!(system.soft_deadline_seconds, 30);
    assert_eq!(system.shutdown_grace_seconds, 5);
}

#[test]
fn serde_shape_and_explicit_validation_reject_invalid_system_values() {
    for invalid in [
        json!({
            "default_solver": {"model": "solver"}
        }),
        json!({
            "coordinator": {"model": "  "},
            "default_solver": {"model": "solver"}
        }),
        json!({
            "coordinator": {"model": "reviewer"},
            "default_solver": {"model": "solver", "timeout_seconds": 0}
        }),
        json!({
            "coordinator": {"model": "reviewer", "effort": "unsupported-effort"},
            "default_solver": {"model": "solver"}
        }),
        json!({
            "coordinator": {"model": "reviewer", "timeot_seconds": 10},
            "default_solver": {"model": "solver"}
        }),
    ] {
        let decoded = serde_json::from_value::<SystemConfig>(invalid);
        if let Ok(system) = decoded {
            assert!(system.validate().is_err());
        }
    }

    let mut zero_soft_deadline = settings();
    zero_soft_deadline.soft_deadline_seconds = 0;
    assert_eq!(
        zero_soft_deadline.validate(),
        Err(SystemConfigError::NonPositive {
            field: "soft_deadline_seconds"
        })
    );

    let invalid_override = ModelSettings {
        model: " ".into(),
        effort: None,
        timeout_seconds: 1,
    };
    assert_eq!(
        invalid_override.validate(),
        Err(ModelSettingsError::InvalidModel)
    );
    assert!(matches!(
        settings().resolve(ToolLimits::default(), Some(invalid_override)),
        Err(ResolvedAgentRuntimeConfigError::Solver(
            ModelSettingsError::InvalidModel
        ))
    ));
}

#[test]
fn fingerprints_are_identical_for_equal_resolved_runtime_values() {
    let system = settings();
    let first = system.resolve(ToolLimits::default(), None).unwrap();
    let second = system.resolve(ToolLimits::default(), None).unwrap();
    assert_eq!(first.fingerprint(), second.fingerprint());
    assert_eq!(first.fingerprint().to_string().len(), 64);
}
