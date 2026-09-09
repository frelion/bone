use std::{collections::VecDeque, sync::Arc};

use bone_adapters::{
    read_only_tools,
    tools::{BashOutput, Tool, ToolEnvironment, ToolFailureKind},
};
use bone_core::{
    BackgroundEntry, BootstrapContext, CallContext, CallError, ExternalEffect, PortFuture,
    ToolEffect, ToolOutcome, ToolPort, ToolSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use crate::{
    CallRef, DataStore, InputId, RuntimeConfig, RuntimeId, SessionEvent, SessionId, SessionSeq,
    ToolMode, UnresolvedWriteView, WorkspaceId,
    storage::{Lease, StoreError},
};

pub(crate) type WriteNotifier = Arc<dyn Fn(CallRef) + Send + Sync>;

pub(crate) struct WriteGate {
    pub lock: Mutex<()>,
    pub changed: watch::Sender<u64>,
}

impl WriteGate {
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            lock: Mutex::new(()),
            changed,
        }
    }
}

pub(crate) async fn unresolved_after_write_gate(
    store: &DataStore,
    gate: &WriteGate,
    workspace: WorkspaceId,
    session: Option<SessionId>,
) -> Result<Vec<UnresolvedWriteView>, StoreError> {
    let mut changed = gate.changed.subscribe();
    let writes = store.unresolved_writes(workspace, None)?;
    if !writes.is_empty() {
        return Ok(writes_for_session(writes, session));
    }

    match gate.lock.try_lock() {
        Ok(_guard) => unresolved_for_session(store, workspace, session),
        Err(_) => {
            tokio::select! {
                _ = changed.changed() => unresolved_for_session(store, workspace, session),
                _guard = gate.lock.lock() => unresolved_for_session(store, workspace, session),
            }
        }
    }
}

fn unresolved_for_session(
    store: &DataStore,
    workspace: WorkspaceId,
    session: Option<SessionId>,
) -> Result<Vec<UnresolvedWriteView>, StoreError> {
    store
        .unresolved_writes(workspace, None)
        .map(|writes| writes_for_session(writes, session))
}

fn writes_for_session(
    mut writes: Vec<UnresolvedWriteView>,
    session: Option<SessionId>,
) -> Vec<UnresolvedWriteView> {
    if let Some(session) = session {
        writes.retain(|write| write.session == session);
    }
    writes
}

#[derive(Clone)]
pub(crate) struct ToolContext {
    pub store: DataStore,
    pub workspace: WorkspaceId,
    pub session: SessionId,
    pub runtime: RuntimeId,
    pub write_gate: Arc<WriteGate>,
    pub lease: Arc<Lease>,
    pub notify: WriteNotifier,
}

pub(crate) fn assemble(
    config: &RuntimeConfig,
    context: ToolContext,
) -> Result<Vec<Arc<dyn ToolPort>>, bone_adapters::tools::ToolError> {
    let environment = ToolEnvironment::with_limits(&config.workspace, config.tools.limits.clone())?;
    let mut tools = read_only_tools(&environment);
    tools.push(Arc::new(SessionHistory {
        store: context.store.clone(),
        session: context.session,
        tool_output_bytes: config.limits.tool_output_bytes,
    }));
    if config.tools.mode == ToolMode::WorkspaceWrite {
        tools.push(Arc::new(WriteTool::new(
            environment.apply_patch(),
            context.clone(),
            |_| ExternalEffect::Applied,
        )));
        tools.push(Arc::new(WriteTool::new(
            environment.bash(),
            context,
            bash_effect,
        )));
    }
    Ok(tools)
}

pub(crate) fn background(
    store: &DataStore,
    session: SessionId,
    context_bytes: usize,
    pending: &[InputId],
) -> Result<BootstrapContext, crate::storage::StoreError> {
    let budget = context_bytes / 4;
    let mut cursor = SessionSeq(0);
    let mut bytes = 0;
    let mut entries = VecDeque::<BackgroundEntry>::new();
    let mut omitted = false;
    loop {
        let page = store.history(session, cursor, 64)?;
        for item in page.items {
            if matches!(
                &item.event,
                SessionEvent::InputSubmitted { input, .. } if pending.contains(input)
            ) {
                continue;
            }
            let label = format!("session event {}", item.sequence.0);
            let content = serde_json::to_string(&item.event).map_err(|_| {
                crate::storage::StoreError::Corrupt {
                    message: "cannot encode session history",
                }
            })?;
            let size = label.len() + content.len();
            if size > budget {
                omitted = true;
                continue;
            }
            while bytes + size > budget {
                if let Some(removed) = entries.pop_front() {
                    bytes -= removed.label.len() + removed.content.len();
                    omitted = true;
                }
            }
            bytes += size;
            entries.push_back(BackgroundEntry::new(label, content));
        }
        cursor = page.next_cursor;
        if !page.has_more {
            break;
        }
    }
    Ok(BootstrapContext {
        entries: entries.into(),
        omitted,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    #[serde(default)]
    after: u64,
}

struct SessionHistory {
    store: DataStore,
    session: SessionId,
    tool_output_bytes: usize,
}

#[derive(Serialize)]
struct HistoryToolPage {
    items: Vec<crate::HistoryEntry>,
    next_cursor: SessionSeq,
    has_more: bool,
    omitted: bool,
}

impl ToolPort for SessionHistory {
    fn specification(&self) -> ToolSpec {
        ToolSpec {
            name: "session_history".into(),
            description: "Read durable public events from earlier in this session.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "after": { "type": "integer", "minimum": 0 }
                },
                "additionalProperties": false
            }),
            effect: ToolEffect::ReadOnly,
        }
    }

    fn run(&self, arguments: Value, _: CallContext) -> PortFuture<ToolOutcome> {
        let store = self.store.clone();
        let session = self.session;
        let tool_output_bytes = self.tool_output_bytes;
        Box::pin(async move {
            let args = match serde_json::from_value::<HistoryArgs>(arguments) {
                Ok(args) => args,
                _ => return ToolOutcome::failed("invalid session_history arguments"),
            };
            session_history_outcome(&store, session, SessionSeq(args.after), tool_output_bytes)
        })
    }
}

fn session_history_outcome(
    store: &DataStore,
    session: SessionId,
    after: SessionSeq,
    tool_output_bytes: usize,
) -> ToolOutcome {
    let page = match store.history(session, after, 1) {
        Ok(page) => page,
        Err(_) => return ToolOutcome::failed("cannot read session history"),
    };
    let next_cursor = page.next_cursor;
    let has_more = page.has_more;
    let value = match serde_json::to_value(HistoryToolPage {
        items: page.items,
        next_cursor,
        has_more,
        omitted: false,
    }) {
        Ok(value) => value,
        Err(_) => return ToolOutcome::failed("cannot encode session history"),
    };
    let outcome = ToolOutcome::value(value);
    if serde_json::to_vec(&outcome).is_ok_and(|encoded| encoded.len() <= tool_output_bytes) {
        return outcome;
    }
    ToolOutcome::value(json!({
        "items": [],
        "next_cursor": next_cursor,
        "has_more": has_more,
        "omitted": true
    }))
}

struct WriteTool<T: Tool> {
    tool: Arc<T>,
    store: DataStore,
    workspace: WorkspaceId,
    session: SessionId,
    runtime: RuntimeId,
    gate: Arc<WriteGate>,
    lease: Arc<Lease>,
    notify: WriteNotifier,
    success_effect: fn(&T::Output) -> ExternalEffect,
}

impl<T: Tool> WriteTool<T> {
    fn new(
        tool: T,
        context: ToolContext,
        success_effect: fn(&T::Output) -> ExternalEffect,
    ) -> Self {
        Self {
            tool: Arc::new(tool),
            store: context.store,
            workspace: context.workspace,
            session: context.session,
            runtime: context.runtime,
            gate: context.write_gate,
            lease: context.lease,
            notify: context.notify,
            success_effect,
        }
    }
}

impl<T: Tool + 'static> ToolPort for WriteTool<T> {
    fn specification(&self) -> ToolSpec {
        let definition = self.tool.definition();
        ToolSpec {
            name: definition.name().to_owned(),
            description: definition.description().to_owned(),
            parameters: definition.parameters().clone(),
            effect: ToolEffect::ExternalWrite,
        }
    }

    fn run(&self, arguments: Value, context: CallContext) -> PortFuture<ToolOutcome> {
        let parsed = match serde_json::from_value::<T::Args>(arguments.clone()) {
            Ok(parsed) => parsed,
            Err(_) => {
                return Box::pin(async {
                    ToolOutcome::failed("tool arguments do not match the declared schema")
                });
            }
        };
        let call = CallRef {
            runtime: self.runtime,
            id: context.id().0,
        };
        let tool = Arc::clone(&self.tool);
        let store = self.store.clone();
        let workspace = self.workspace;
        let session = self.session;
        let gate = Arc::clone(&self.gate);
        let lease = Arc::clone(&self.lease);
        let notify = Arc::clone(&self.notify);
        let success_effect = self.success_effect;
        Box::pin(async move {
            let task = tokio::spawn(async move {
                let _lease = lease;
                let _guard = gate.lock.lock().await;
                if context.cancellation_requested() {
                    return ToolOutcome::failed("tool call was cancelled before execution");
                }
                match store.unresolved_writes(workspace, None) {
                    Ok(writes) if !writes.is_empty() => {
                        return ToolOutcome::failed(
                            "workspace has a write that must be resolved before another write",
                        );
                    }
                    Err(_) => return ToolOutcome::failed("cannot check prior write state"),
                    Ok(_) => {}
                }
                match store.begin_write(
                    workspace,
                    session,
                    call,
                    tool.definition().name(),
                    arguments,
                ) {
                    Ok(true) => {}
                    Ok(false) => return ToolOutcome::failed("write call was already recorded"),
                    Err(_) => return ToolOutcome::failed("cannot record write before execution"),
                }
                gate.changed
                    .send_modify(|version| *version = version.wrapping_add(1));

                let outcome = match tool.call(parsed).await {
                    Ok(output) => ToolOutcome {
                        external_effect: success_effect(&output),
                        result: serde_json::to_value(output)
                            .map_err(|_| CallError::failed("cannot encode tool output")),
                    },
                    Err(error) => {
                        let failure = tool.map_error(error);
                        ToolOutcome {
                            result: Err(CallError::failed(
                                failure
                                    .model_output()
                                    .as_text()
                                    .map(str::to_owned)
                                    .or_else(|| {
                                        failure.model_output().as_json().map(Value::to_string)
                                    })
                                    .unwrap_or_else(|| "tool execution failed".into()),
                            )),
                            external_effect: if failure.kind() == ToolFailureKind::InvalidArguments
                            {
                                ExternalEffect::None
                            } else {
                                ExternalEffect::Unknown
                            },
                        }
                    }
                };
                if store
                    .finish_write(workspace, call, outcome.clone())
                    .is_err()
                {
                    return ToolOutcome {
                        result: Err(CallError::failed(
                            "write finished but its result could not be saved",
                        )),
                        external_effect: ExternalEffect::Unknown,
                    };
                }
                notify(call);
                outcome
            });
            match task.await {
                Ok(outcome) => outcome,
                Err(_) => ToolOutcome {
                    result: Err(CallError::failed("write task failed")),
                    external_effect: ExternalEffect::Unknown,
                },
            }
        })
    }
}

fn bash_effect(output: &BashOutput) -> ExternalEffect {
    if output.timed_out {
        ExternalEffect::Unknown
    } else {
        ExternalEffect::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SubmitInput;

    #[test]
    fn session_history_advances_past_an_event_that_exceeds_its_output_budget() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace_root = temporary.path().join("workspace");
        std::fs::create_dir(&workspace_root).unwrap();
        let store = DataStore::open(temporary.path().join("data")).unwrap();
        let workspace = store.workspace(&workspace_root).unwrap();
        let session = store
            .create_session(workspace.id, "history".into())
            .unwrap();
        let large = store
            .accept_input(session.info.id, &SubmitInput::new("x".repeat(2_048)))
            .unwrap()
            .0;
        let small = store
            .accept_input(session.info.id, &SubmitInput::new("later"))
            .unwrap()
            .0;

        let skipped = session_history_outcome(
            &store,
            session.info.id,
            SessionSeq(large.saved_at.0 - 1),
            512,
        )
        .result
        .unwrap();
        assert_eq!(skipped["omitted"], true);
        assert_eq!(skipped["next_cursor"], large.saved_at.0);
        assert_eq!(skipped["has_more"], true);

        let visible = session_history_outcome(
            &store,
            session.info.id,
            SessionSeq(skipped["next_cursor"].as_u64().unwrap()),
            512,
        )
        .result
        .unwrap();
        assert_eq!(visible["omitted"], false);
        assert_eq!(visible["next_cursor"], small.saved_at.0);
        assert_eq!(
            visible["items"][0]["event"]["InputSubmitted"]["text"],
            "later"
        );
    }

    #[tokio::test]
    async fn one_session_close_does_not_wait_for_another_sessions_recorded_write() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace_root = temporary.path().join("workspace");
        std::fs::create_dir(&workspace_root).unwrap();
        let store = DataStore::open(temporary.path().join("data")).unwrap();
        let workspace = store.workspace(&workspace_root).unwrap();
        let writer = store.create_session(workspace.id, "writer".into()).unwrap();
        let idle = store.create_session(workspace.id, "idle".into()).unwrap();
        store
            .begin_write(
                workspace.id,
                writer.info.id,
                CallRef {
                    runtime: RuntimeId::new(),
                    id: 1,
                },
                "bash",
                json!({}),
            )
            .unwrap();
        let gate = WriteGate::new();
        let _guard = gate.lock.lock().await;

        let writes = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            unresolved_after_write_gate(&store, &gate, workspace.id, Some(idle.info.id)),
        )
        .await
        .expect("another session's recorded write must not delay close")
        .unwrap();
        assert!(writes.is_empty());
    }
}
