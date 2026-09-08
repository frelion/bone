use bone_app::{ExternalEffect, InputOutcome, OutcomeKind, ToolOutcome, WriteResolution};

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
