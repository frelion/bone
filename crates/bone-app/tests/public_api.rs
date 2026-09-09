use bone_app::{
    AgentLimits, EndpointConfig, ExternalEffect, InputOutcome, ModelOptions, OutcomeKind,
    ToolLimits, ToolOutcome, WriteResolution,
};

#[test]
fn public_api_reexports_frontend_configuration_and_agent_semantics() {
    fn exported<T>() {}

    let resolution = WriteResolution {
        external_effect: ExternalEffect::Applied,
        evidence: "checked".into(),
    };
    assert_eq!(resolution.external_effect, ExternalEffect::Applied);
    let _ = InputOutcome::Completed;
    let _ = OutcomeKind::Completed;
    let _ = ToolOutcome::value("visible");
    exported::<AgentLimits>();
    exported::<EndpointConfig>();
    exported::<ModelOptions>();
    exported::<ToolLimits>();
}
