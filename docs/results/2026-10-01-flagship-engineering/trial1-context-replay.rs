#![allow(dead_code)]
use std::collections::BTreeMap;
use anyhow::Result;
use serde_json::{Value,json};
use rig_core::completion::{CompletionRequest,Message};
#[path="/Users/zzhang/.bone/acceptance/2026-10-01-workspace-engineering-trial1/replay-frozen-src/state.rs"] mod state;
#[path="/Users/zzhang/.bone/acceptance/2026-10-01-workspace-engineering-trial1/replay-frozen-src/context.rs"] mod context;
#[path="/Users/zzhang/.bone/acceptance/2026-10-01-workspace-engineering-trial1/replay-frozen-src/tools.rs"] mod tools;
struct Probe {state:state::SessionState}
impl Probe {
fn root(&self,input:&str)->Result<String>{Ok(input.to_owned())}
    fn preamble(&self, id: &str) -> Result<String> {
        let job = &self.state.jobs[id];
        let catalog=self.state.jobs.values().take(32).map(|j|json!({"id":j.id,"title":j.title,"state":j.state,"active_input":j.active_input})).collect::<Vec<_>>();
        Ok(format!(
            "You are BONE, the one agent in this conversation. You are currently working inside job {id} ({title}). Jobs are your internal continuing work contexts; never ask the user to create, select or manage them. Every thought and action belongs to this job.\nYour current task is only the input marked status=ACTIVE with ID {input} in this request. Match that marker to its original content. QUEUED inputs are retained for later and must not replace the current task; HISTORICAL and SHARED inputs supply relevant background and corrections. Follow the newest applicable instruction by revision when older instructions conflict. Current input markers override any routing state described in an earlier summary. Use tools to inspect actual files and verify results. Reply with a final answer only after this input is handled. Preserve original constraints. When the ACTIVE request incorporates earlier work, inspect QUEUED requests and explicitly use input_resolve after verifying their work is completed or a newer instruction supersedes them. Give a concrete reason; leave independent queued work unresolved. A final answer settles only the ACTIVE input. Do not repeat completed or superseded effects.\nContinue related work here. Create other jobs only when independent work or a separate continuing context benefits the task. job_send returns an exact input_id; use job_wait instead of polling. You may send a followup to an existing idle job. Use job_handoff before acting to transfer conversation responsibility. Waiting and handoff must be sole tool calls in their batch. When the user asks to stop, call pause_work. To pause/resume other work after a changed instruction, use job_control. If instructions are unclear, ask_user.\nFile tools are confined to workspace {workspace}; shell has local user privileges. Do not claim an operation succeeded unless tool evidence verifies it. Tools disabled by the session permission policy must not be worked around.\nPublic user instructions are included in the history with their revisions. Apply newer relevant corrections to your work; other jobs' assignments remain theirs. When an earlier requirement or unfinished commitment is unclear in a summary, use job_inspect(users_only=true) to find original session instructions, then job_inspect(event_id=...) for their complete readable text. Follow next_before_id and next_offset when truncated. Do not conclude that a specification is missing just because its summary is vague.\nJOB CATALOG:\n{catalog}\nExecution budget shared by this input and its delegated jobs: {budget}",
            title = job.title,
            input = job.active_input.as_deref().unwrap_or(""),
            workspace = self.state.workspace.display(),
            catalog = serde_json::to_string(&catalog)?,
            budget = job
                .active_input
                .as_ref()
                .and_then(|i| self.root(i).ok())
                .and_then(|r| self.state.budgets.get(&r))
                .map(|b| format!(
                    "{} / {} calls; {} / {} created jobs",
                    b.calls_used, b.max_calls, b.jobs_used, b.max_jobs
                ))
                .unwrap_or_default()
        ))
    }
}
fn main()->Result<()> {
 let art=std::path::Path::new("/Users/zzhang/.bone/acceptance/2026-10-01-workspace-engineering-trial1");
 let all:Vec<state::Event>=serde_json::from_slice(&std::fs::read(art.join("phase1-6-history.json"))?)?;
 let mut state:state::SessionState=serde_json::from_slice(&std::fs::read(art.join("phase1-6-state.json"))?)?;
 let jobid=state.jobs.keys().next().unwrap().clone();
 let input=all.iter().find(|e|e.kind=="input").unwrap().id.clone();
 {let j=state.jobs.get_mut(&jobid).unwrap();j.history.clear();j.summary=None;j.active_input=Some(input.clone());j.inbox.clear();j.state=state::JobState::Ready;}
 state.budgets.get_mut(&input).unwrap().calls_used=0;
 let mut probe=Probe{state};let mut events=BTreeMap::new();let mut rows=vec![];
 for (index,event) in all.iter().enumerate(){
 if event.kind=="model_started" {
  let job=&probe.state.jobs[&jobid];
  let mut template=CompletionRequest::from(Vec::<Message>::new()).preamble(probe.preamble(&jobid)?).tools(tools::definitions(false,false));template.max_tokens=Some(8192);
  let overhead=context::serialized_chars(&template)?;let history_budget=48000-overhead-64;
  let raw=context::build_history(job,&events)?; if index==1 {eprintln!("NATIVE initial history={} input={}",serde_json::to_string(&raw)?.chars().count(),serde_json::to_string(&events[&input].data["message"])?.chars().count());}let template_messages=template.chat_history.clone();let system_chars=template_messages.iter().map(|m|serde_json::to_string(m).unwrap().chars().count()).sum::<usize>();let mut req=template;req.chat_history.extend(raw.clone());let raw_chars=context::serialized_chars(&req)?;
  let actual_purpose=event.data["purpose"].as_str().unwrap_or("");
  let audit_before=serde_json::to_string(&events)?;let mut projected=false;
  if actual_purpose=="work" && raw_chars>48000 {if let Some(h)=context::bounded_work_history(job,&events,history_budget)? {req.chat_history=h;projected=true;} }
  let mut repaired=req.clone();if projected {repaired.chat_history=template_messages;repaired.chat_history.extend(req.chat_history.clone());}let audit_unmodified=audit_before==serde_json::to_string(&events)?;let mut result_rows=vec![];
  for m in &req.chat_history {if let Message::User{content}=m {for part in content {if let rig_core::message::UserContent::ToolResult(tr)=part {
  let mut text_chars=0;let mut preview_chars=0;let mut truncated=false;
  for c in &tr.content{let v=match c {rig_core::message::ToolResultContent::Json{value}=>Some(value.clone()),rig_core::message::ToolResultContent::Text(t)=>serde_json::from_str::<Value>(&t.text).ok(),_=>None};if let Some(v)=v {text_chars+=v["text"].as_str().map(str::len).unwrap_or(0);preview_chars+=v["preview"].as_str().map(str::len).unwrap_or(0);truncated|=v["truncated"].as_bool().unwrap_or(false);}}
  result_rows.push(json!({"name":tr.name,"body_bytes":text_chars,"preview_bytes":preview_chars,"truncated":truncated}));
  }}}}
  rows.push(json!({"event_index":index,"purpose":actual_purpose,"overhead_chars":overhead,"history_budget":history_budget,"raw_request_chars":raw_chars,"work_request_chars":context::serialized_chars(&req)?,"projected":projected,"system_message_chars":system_chars,"old_bone_preamble_present":req.system_instructions().is_some_and(|s|s.starts_with("You are BONE")),"new_bone_preamble_present":repaired.system_instructions().is_some_and(|s|s.starts_with("You are BONE")),"repaired_request_chars":context::serialized_chars(&repaired)?,"audit_unmodified":audit_unmodified,"summary_chars":job.summary.as_ref().map(|id| serde_json::to_string(&raw[0]).unwrap().chars().count()),"tools":result_rows}));
  probe.state.budgets.get_mut(&input).unwrap().calls_used+=1;
 }
 if matches!(event.kind.as_str(),"input"|"model_message"|"tool_result") {probe.state.jobs.get_mut(&jobid).unwrap().history.push(event.id.clone());}
 if event.kind=="summary" {let ids:Vec<String>=serde_json::from_value(event.data["covered_ids"].clone())?;let j=probe.state.jobs.get_mut(&jobid).unwrap();j.history.retain(|id|!ids.contains(id));j.summary=Some(event.id.clone());}
 events.insert(event.id.clone(),event.clone());
 }
 println!("{}",serde_json::to_string_pretty(&rows)?);Ok(())
}
