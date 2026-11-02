//! Stream saved sources into a bounded tree of review notes, not a full prompt.
use crate::api::Shared;
use anyhow::{Context,Result};
use serde_json::{json,Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
struct Note{preview:String,id:String,ordinal:u64}
struct Reviewer<'a>{app:&'a Shared,model:&'a str,question:&'a str,think:&'a Value,tx:&'a mpsc::Sender<Value>,cancel:&'a CancellationToken,run:String,stats:Vec<Value>,calls:u64,note_chars:usize}
impl Reviewer<'_>{
    async fn call(&mut self,material:&str,mut provenance:Value)->Result<Note>{
        let ordinal=provenance["ordinal"].as_u64().unwrap_or(0);
        let text=crate::engine::review_call(self.app,self.model,self.question,material,self.think,self.tx,self.cancel,&mut self.stats).await?;
        self.calls+=1;
        provenance["origin"]=json!("review_note");provenance["model"]=json!(self.model);provenance["inference"]=self.stats.last().cloned().unwrap_or(Value::Null);
        let source=self.app.tasks.put_artifact(self.run.clone(),"derived_result".into(),text.clone(),provenance).await?;
        crate::timeline::window(&mut self.stats);
        Ok(Note{preview:crate::tools::truncate(&text,self.note_chars),id:source["id"].as_str().unwrap().into(),ordinal})
    }
    async fn merge(&mut self,mut notes:Vec<Note>)->Result<Note>{
        notes.sort_by_key(|note|note.ordinal);let ordinal=notes.first().map_or(0,|note|note.ordinal);
        let references=notes.iter().map(|note|note.id.clone()).collect::<Vec<_>>();
        let text=notes.iter().map(|note|format!("Saved note {}:\n{}",note.id,note.preview)).collect::<Vec<_>>().join("\n\n");
        self.call(&format!("Consolidate these partial review notes. Preserve concrete findings, supporting excerpts and uncertainty. These are summaries, not the full sources. Do not invent verification or cross-fragment context. Keep concise.\n{text}"),json!({"children":references,"summary_based":true,"ordinal":ordinal})).await
    }
}
pub async fn review(app:&Shared,model:&str,question:&str,sources:&[Value],think:&Value,tx:&mpsc::Sender<Value>,cancel:&CancellationToken)->Result<(String,usize,Value)>{
    let config=app.settings.lock().await.context.clone();let show=crate::thinking::show_cancellable(app,model,cancel).await?;
    let mut baseline=crate::engine::review_history(question,"");
    let(budget,_)=crate::context::prepare(&mut baseline,&json!([]),&config,&show,crate::context::thinking_enabled(think,&show),16384)?;
    let available=budget.history_budget.saturating_sub(crate::context::history_tokens(&baseline)+700);
    anyhow::ensure!(available>=256,"В выбранном окне недостаточно места для анализа части материала. Увеличьте окно или уменьшите резерв ответа; исходники сохранены");
    let mut reviewer=Reviewer{app,model,question,think,tx,cancel,run:crate::permissions::TASK.try_with(Clone::clone).context("Нет активного запуска")?,stats:vec![],calls:0,note_chars:(available/6).clamp(32,1600)};
    let mut levels:Vec<Vec<Note>>=vec![];let mut parts=0;let mut coverage=vec![];
    for source in sources{
        let id=source["id"].as_str().context("Нет ID исходника")?;let mut offset=0usize;
        loop{
            anyhow::ensure!(!cancel.is_cancelled(),"Анализ остановлен");
            let page=app.tasks.artifact_page(id.into(),offset).await?;let text=page["text"].as_str().unwrap_or("");
            let mut position=offset;
            for fragment in crate::agent::review_pages(text,available.min(5000)){
                parts+=1;let end=position+fragment.chars().count();
                crate::engine::emit(tx,json!({"type":"notice","message":format!("Анализ материала: часть {parts}, символы {position}–{end}")})).await?;
                let mut note=reviewer.call(&format!("Source fragment {parts}. Source ID {id}; characters {position}..{end}. Adjacent context may be missing. Report concrete findings briefly, with exact supporting excerpts.\n{fragment}"),json!({"source":id,"start":position,"end":end,"source_metadata":source["metadata"],"ordinal":parts})).await?;
                let mut level=0;
                loop{
                    if levels.len()<=level{levels.push(vec![]);}
                    levels[level].push(note);if levels[level].len()<3{break;}
                    let group=std::mem::take(&mut levels[level]);note=reviewer.merge(group).await?;level+=1;
                }
                position=end;
            }
            offset=page["next_offset"].as_u64().context("Нет смещения страницы")? as usize;
            if page["has_more"]!=true{break;}
        }
        coverage.push(json!({"id":id,"characters_read":offset,"characters":source["characters"],"complete":offset as u64==source["characters"].as_u64().unwrap_or(u64::MAX),"source_metadata":source["metadata"]}));
    }
    let mut remaining=levels.into_iter().flatten().collect::<Vec<_>>();
    remaining.sort_by_key(|note|note.ordinal);
    while remaining.len()>3{
        let mut next=vec![];for group in remaining.chunks(3){if group.len()==1{next.push(group[0].clone());}else{next.push(reviewer.merge(group.to_vec()).await?);}}remaining=next;
    }
    let children=remaining.iter().map(|note|note.id.clone()).collect::<Vec<_>>();let summaries=remaining.iter().map(|note|format!("Saved note {}:\n{}",note.id,note.preview)).collect::<Vec<_>>().join("\n\n");
    let final_note=reviewer.call(&format!("Synthesize notes from {parts} source fragments into one review. Cite source IDs and exact excerpts that survived into the notes. This is summary-based; cross-fragment relations and missing original-document context remain unverified. Do not claim tests or full project verification.\n{summaries}"),json!({"children":children,"summary_based":true,"source_coverage":coverage})).await?;
    let mut result=String::new();let mut offset=0;loop{let page=app.tasks.artifact_page(final_note.id.clone(),offset).await?;result.push_str(page["text"].as_str().unwrap_or(""));offset=page["next_offset"].as_u64().unwrap() as usize;if page["has_more"]!=true{break;}}
    Ok((result,parts,json!({"source_coverage":coverage,"inferences":reviewer.stats,"inference_count":reviewer.calls,"inferences_partial":reviewer.calls>crate::timeline::TOOL_WINDOW as u64,"final_note":final_note.id,"summary_based":true})))
}
