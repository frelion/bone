use bone_app::{
    AcceptanceCursor, AcceptanceDecision, AcceptanceId, AcceptancePage, AcceptanceReceipt,
    AcceptanceRecord, AcceptanceRequestId, AcceptanceSubmission, AgentLimits, AttentionItem,
    EndpointConfig, EvidenceAvailability, EvidenceCursor, EvidencePage, EvidenceRef,
    EvidenceSourceKind, EvidenceSourcePage, ExternalEffect, HistoryCursor, InputOutcome,
    ModelOptions, OutcomeKind, RecentHistoryPage, ResultArtifact, ResultPage, ResultRef,
    ResultSummary, SessionReleaseReceipt, SessionReleaseStatus, SessionRetentionReason,
    SessionSummary, ToolLimits, ToolOutcome, WorkspaceBaseline, WorkspaceChangeCursor,
    WorkspaceChangePage, WorkspaceChangedFile, WorkspaceFileMedia, WorkspaceFileSource,
    WorkspaceFileView, WorkspaceOverview, WriteResolution,
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
    exported::<AttentionItem>();
    exported::<SessionSummary>();
    exported::<WorkspaceOverview>();
    exported::<WorkspaceBaseline>();
    exported::<WorkspaceChangedFile>();
    exported::<WorkspaceChangeCursor>();
    exported::<WorkspaceChangePage>();
    exported::<WorkspaceFileSource>();
    exported::<WorkspaceFileMedia>();
    exported::<WorkspaceFileView>();
    exported::<HistoryCursor>();
    exported::<RecentHistoryPage>();
    exported::<AcceptanceDecision>();
    exported::<AcceptanceId>();
    exported::<AcceptanceRequestId>();
    exported::<AcceptanceSubmission>();
    exported::<AcceptanceReceipt>();
    exported::<AcceptanceRecord>();
    exported::<AcceptanceCursor>();
    exported::<AcceptancePage>();
    exported::<ResultRef>();
    exported::<ResultSummary>();
    exported::<ResultPage>();
    exported::<ResultArtifact>();
    exported::<EvidenceRef>();
    exported::<EvidenceSourceKind>();
    exported::<EvidenceAvailability>();
    exported::<EvidenceCursor>();
    exported::<EvidencePage>();
    exported::<EvidenceSourcePage>();
    exported::<SessionReleaseReceipt>();
    exported::<SessionReleaseStatus>();
    exported::<SessionRetentionReason>();
    let _ = bone_app::App::workspace_overview;
    let _ = bone_app::App::resolved_workspace_config;
    let _ = bone_app::App::create_session_idempotent;
    let _ = bone_app::App::last_active_session;
    let _ = bone_app::App::set_last_active_session;
    let _ = bone_app::App::workspace_changes;
    let _ = bone_app::App::results;
    let _ = bone_app::App::result_artifact;
    let _ = bone_app::App::result_evidence;
    let _ = bone_app::App::evidence_source;
    let _ = bone_app::App::acceptances;
    let _ = bone_app::App::release_session;
    let _ = bone_app::Session::recent_history;
    let _ = bone_app::Session::submit_acceptance;
    let _ = bone_app::Session::title_from_first_input;
    exported::<bone_app::CreateSessionRequest>();
}
