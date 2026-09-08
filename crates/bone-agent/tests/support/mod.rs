#![allow(dead_code)]

use bone_agent::*;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::{broadcast, mpsc, oneshot};

pub fn input(id: u64, text: &str) -> Input {
    Input::new(InputId(id), text)
}

pub fn specification(name: &str, effect: ToolEffect) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: name.into(),
        parameters: json!({"type":"object"}),
        effect,
    }
}

pub fn create(goal: &str, inputs: Vec<InputId>) -> JobChange {
    JobChange::Create(JobSpec {
        goal: goal.into(),
        inputs,
        parent: None,
        references: vec![],
    })
}

pub fn update(
    job: JobId,
    goal: Option<&str>,
    action: JobAction,
    inputs: Vec<InputId>,
    required: bool,
) -> JobChange {
    JobChange::Update {
        job,
        goal: goal.map(str::to_owned),
        action,
        inputs,
        required,
    }
}

pub fn decision(changes: Vec<JobChange>) -> CallOutcome {
    CallOutcome::kernel(KernelDecision {
        changes,
        ..Default::default()
    })
}

pub fn answer(text: &str) -> WorkProposal {
    WorkProposal {
        reply: Some(text.into()),
        next: Next::Finish,
        ..Default::default()
    }
}

pub fn operation(name: &str) -> WorkProposal {
    WorkProposal {
        operation: Some(ToolCall::new(name, json!({}))),
        ..Default::default()
    }
}

pub fn applied() -> CallOutcome {
    CallOutcome {
        external_effect: ExternalEffect::Applied,
        ..CallOutcome::artifact(json!({"receipt":"saved"}))
    }
}

pub fn cancelled() -> CallOutcome {
    CallOutcome {
        result: Err(CallError {
            kind: CallErrorKind::Cancelled,
            message: "local wait ended".into(),
        }),
        external_effect: ExternalEffect::None,
    }
}

pub fn model_calls(effects: &[Effect]) -> Vec<(CallId, ModelInput)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Start {
                id,
                call: Call::Model(input),
                ..
            } => Some((*id, input.clone())),
            _ => None,
        })
        .collect()
}

pub fn routing(effects: &[Effect]) -> (CallId, ModelInput) {
    model_calls(effects)
        .into_iter()
        .find(|(_, input)| matches!(input.task, ModelTask::Kernel { .. }))
        .expect("Kernel routing call")
}

pub fn worker(effects: &[Effect], job: JobId) -> (CallId, ModelInput) {
    model_calls(effects)
        .into_iter()
        .find(|(_, input)| matches!(input.task, ModelTask::Work { job: owner, .. } if owner == job))
        .expect("worker for the expected job")
}

pub fn tool(effects: &[Effect], name: &str) -> CallId {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Start {
                id,
                call: Call::Tool(call),
                ..
            } if call.name == name => Some(*id),
            _ => None,
        })
        .expect("tool invocation")
}

pub fn wake(effects: &[Effect]) -> WakeId {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::WakeAfter { id, .. } => Some(*id),
            _ => None,
        })
        .expect("timer")
}

pub fn notices(effects: &[Effect]) -> Vec<&Notice> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Publish(notice) => Some(notice),
            _ => None,
        })
        .collect()
}

pub fn replies(effects: &[Effect]) -> Vec<&str> {
    notices(effects)
        .into_iter()
        .filter_map(|notice| match notice {
            Notice::Reply { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

pub fn no_start(effects: &[Effect]) {
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Start { .. })),
        "an unexpected call was dispatched"
    );
}

pub fn no_route(effects: &[Effect]) {
    assert!(
        !model_calls(effects)
            .iter()
            .any(|(_, input)| matches!(input.task, ModelTask::Kernel { .. }))
    );
}

pub fn no_tool(effects: &[Effect]) {
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::Start {
            call: Call::Tool(_),
            ..
        }
    )));
}

pub fn finished(id: CallId, outcome: CallOutcome) -> Event {
    Event::CallFinished { id, outcome }
}

pub struct Scenario {
    pub kernel: Kernel,
}

impl Scenario {
    pub fn new() -> Self {
        Self::with_config(KernelConfig::default())
    }
    pub fn with_config(config: KernelConfig) -> Self {
        Self {
            kernel: Kernel::new(
                config,
                vec![
                    specification("lookup", ToolEffect::ReadOnly),
                    specification("write", ToolEffect::ExternalWrite),
                ],
            )
            .unwrap(),
        }
    }
    pub fn say(&mut self, id: u64, text: &str) -> Vec<Effect> {
        let input = input(id, text);
        assert_eq!(self.kernel.admit(&input).unwrap(), None);
        self.kernel.step(Event::Input(input))
    }
    pub fn finish(&mut self, id: CallId, outcome: CallOutcome) -> Vec<Effect> {
        self.kernel.step(finished(id, outcome))
    }
    pub fn work(&mut self, id: CallId, proposal: WorkProposal) -> Vec<Effect> {
        self.finish(id, CallOutcome::work(proposal))
    }
    pub fn create(&mut self, id: u64, text: &str) -> (JobId, CallId) {
        let (route, _) = routing(&self.say(id, text));
        let effects = self.finish(route, decision(vec![create(text, vec![InputId(id)])]));
        let job = self.kernel.snapshot().jobs.last().unwrap().id;
        let (call, _) = worker(&effects, job);
        (job, call)
    }
    pub fn job(&self, id: JobId) -> JobSnapshot {
        self.kernel
            .snapshot()
            .jobs
            .into_iter()
            .find(|job| job.id == id)
            .unwrap()
    }
    pub fn call(&self, id: CallId) -> CallSnapshot {
        self.kernel
            .snapshot()
            .calls
            .into_iter()
            .find(|call| call.id == id)
            .unwrap()
    }
    pub fn state(&self, id: u64) -> InputState {
        self.kernel
            .snapshot()
            .inputs
            .into_iter()
            .find(|entry| entry.input.id == InputId(id))
            .unwrap()
            .state
    }
}

pub struct ModelRequest {
    pub input: ModelInput,
    pub context: CallContext,
    pub reply: oneshot::Sender<CallOutcome>,
}

impl ModelRequest {
    pub fn job(&self) -> JobId {
        match self.input.task {
            ModelTask::Work { job, .. } => job,
            _ => panic!("expected a worker"),
        }
    }
    pub fn inputs(&self) -> Vec<InputId> {
        match &self.input.task {
            ModelTask::Kernel { inputs, .. } => inputs.iter().map(|input| input.id).collect(),
            _ => panic!("expected routing"),
        }
    }
    pub fn route(self, goal: &str) {
        let inputs = self.inputs();
        self.reply
            .send(decision(vec![create(goal, inputs)]))
            .unwrap();
    }
    pub fn work(self, proposal: WorkProposal) {
        self.reply.send(CallOutcome::work(proposal)).unwrap();
    }
}

pub struct ControlledModel(pub mpsc::UnboundedSender<ModelRequest>);
impl ModelPort for ControlledModel {
    fn infer(&self, input: ModelInput, context: CallContext) -> BoxFuture<'static, CallOutcome> {
        let (reply, response) = oneshot::channel();
        self.0
            .send(ModelRequest {
                input,
                context,
                reply,
            })
            .unwrap();
        // Intentionally ignores cancellation. The runtime must drop the local wait.
        Box::pin(async move {
            response
                .await
                .unwrap_or_else(|_| CallOutcome::failed("test reply dropped"))
        })
    }
}

pub struct ToolRequest {
    pub arguments: Value,
    pub context: CallContext,
    pub reply: oneshot::Sender<CallOutcome>,
}
pub struct ControlledTool {
    pub requests: mpsc::UnboundedSender<ToolRequest>,
    pub effect: ToolEffect,
}
impl ToolPort for ControlledTool {
    fn specification(&self) -> ToolSpec {
        specification("operation", self.effect)
    }
    fn run(&self, arguments: Value, context: CallContext) -> BoxFuture<'static, CallOutcome> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(ToolRequest {
                arguments,
                context,
                reply,
            })
            .unwrap();
        Box::pin(async move {
            response
                .await
                .unwrap_or_else(|_| CallOutcome::unknown("test reply dropped"))
        })
    }
}

pub fn session(
    tools: Vec<Arc<dyn ToolPort>>,
    config: KernelConfig,
) -> (AgentHandle, mpsc::UnboundedReceiver<ModelRequest>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        Runtime::spawn(
            Arc::new(ControlledModel(sender)),
            tools,
            config,
            RuntimeConfig::default(),
        )
        .unwrap(),
        receiver,
    )
}

pub async fn receive<T>(receiver: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("expected independently progressing call")
        .expect("call channel open")
}

pub async fn notice(
    receiver: &mut broadcast::Receiver<Notice>,
    predicate: impl Fn(&Notice) -> bool,
) -> Notice {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let notice = receiver
                .recv()
                .await
                .expect("notice channel open and not lagging");
            if predicate(&notice) {
                return notice;
            }
        }
    })
    .await
    .expect("expected notice")
}

pub async fn observed(
    observation: &mut Observation,
    predicate: impl Fn(&StepEvent) -> bool,
) -> Arc<StepEvent> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let step = observation.events.recv().await.unwrap();
            if predicate(&step) {
                return step;
            }
        }
    })
    .await
    .expect("expected observed transition")
}
