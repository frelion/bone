use bone_app::{
    AgentLimits, EndpointConfig, ExternalEffect, InputOutcome, ModelOptions, OutcomeKind,
    ToolLimits, ToolOutcome, WriteResolution,
};

#[test]
fn agent_semantics_used_by_public_dtos_are_reexported() {
    let resolution = WriteResolution {
        external_effect: ExternalEffect::Applied,
        evidence: "checked".into(),
    };
    assert_eq!(resolution.external_effect, ExternalEffect::Applied);
    let _ = InputOutcome::Completed;
    let _ = OutcomeKind::Completed;
    let _ = ToolOutcome::value("visible");
}

#[test]
fn public_configuration_types_are_selectively_reexported() {
    fn exported<T>() {}

    exported::<AgentLimits>();
    exported::<EndpointConfig>();
    exported::<ModelOptions>();
    exported::<ToolLimits>();
}
