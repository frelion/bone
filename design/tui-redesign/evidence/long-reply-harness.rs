use bone_tui::{state::{UiState,SessionUi},view};
use bone_app::*;
use ratatui::{Terminal,backend::TestBackend};
fn main(){
let info=SessionInfo{id:SessionId::new(),workspace:WorkspaceId::new(),title:"Investigate draft recovery".into(),archived:false};
let mut s=UiState::default();s.selected=Some(info.id);s.sessions=vec![info.clone()];
let mut ui=SessionUi::new(info.clone(),1);
for n in 0..4 {ui.history.push_back(HistoryEntry{sequence:SessionSeq(n+1),occurred_at:0,event:SessionEvent::Reply{job:JobRef{runtime:RuntimeId::new(),id:1},inputs:vec![],text:format!("## Finding {}\n\nThe draft needs to remain attached to its session.\n\n```rust\nlet draft = session.draft();\nsave(draft).await?;\n```\n\n- Preserve pending text\n- Restore the reading position\n",n+1)}});}
s.session_ui.insert(info.id,ui);
let mut t=Terminal::new(TestBackend::new(160,40)).unwrap();t.draw(|f|{view::render(f,&s);}).unwrap();
for y in 0..40{for x in 0..160{print!("{}",t.backend().buffer()[(x,y)].symbol());}println!();}
}
