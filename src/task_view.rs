//! Small, independently pageable views over the durable Task ledger.
use crate::api::Shared;
use anyhow::{Context,Result};
use rusqlite::params;
use serde_json::{json,Value};
fn request_page(mut state:Value,limit:usize,preview:usize)->Value{
    let old_more=state["has_more"].as_bool().unwrap_or(false);let requests=state["requests"].as_array_mut().unwrap();let more=old_more||requests.len()>limit;
    requests.truncate(limit);for request in requests.iter_mut(){request["preview"]=json!(if preview==0{String::new()}else{crate::tools::truncate(request["preview"].as_str().unwrap_or(""),preview)});}
    let next=requests.last().map(|request|request["sequence"].clone()).unwrap_or(json!(0));
    json!({"id":state["id"],"requests":state["requests"],"next_after":next,"has_more":more,"note":"Request previews; read_task(request,offset,max_characters) reads the exact original text."})
}
fn trim_index(mut page:Value,limit:usize)->Value{
    let old_more=page["has_more"]==true||!page["next_before"].is_null();
    let items=page["items"].as_array_mut().unwrap();let more=old_more||items.len()>limit;items.truncate(limit);
    let before=if more{items.last().map(|item|item["sequence"].clone()).unwrap_or(Value::Null)}else{Value::Null};page["has_more"]=json!(more);page["next_before"]=before;page
}
pub async fn read(app:&Shared,run:&str,task:Value,args:&Value)->Result<Value>{
    let id=task["id"].as_str().context("Нет идентификатора задачи")?.to_string();let limit=usize::try_from(args["limit"].as_u64().unwrap_or(8))?;let preview=usize::try_from(args["preview_characters"].as_u64().unwrap_or(120))?;
    anyhow::ensure!((1..=32).contains(&limit)&&preview<=400,"Некорректный размер страницы задачи");
    let section=args["section"].as_str().unwrap_or(if args.get("after").is_some(){"requests"}else if args.get("before_artifact").is_some(){"observations"}else if args.get("before_material").is_some(){"material_jobs"}else if args.get("before_discovery").is_some()||args.get("discovery_snapshot").is_some(){"discovery"}else if args.get("before_document").is_some(){"document_jobs"}else if args.get("before_run").is_some(){"runs"}else{"overview"});
    match section{
        "requirements"|"requirement_checks"|"attempts"=>app.tasks.requirement_page(run.into(),section.into(),args.clone()).await,
        "document_jobs"=>app.tasks.document_job_page(format!("task:{id}"),args["before_document"].as_i64(),limit).await,
        "discovery"=>crate::discovery::catalog(app.data.clone(),id,args["before_discovery"].as_i64(),args["discovery_snapshot"].as_i64(),limit).await,
        "requests"=>Ok(request_page(app.tasks.task(id,args["after"].as_i64().unwrap_or(0)).await?,limit,preview)),
        "observations"=>Ok(trim_index(app.tasks.artifact_list(run.into(),args["before_artifact"].as_i64()).await?,limit)),
        "material_jobs"=>Ok(trim_index(app.tasks.material_jobs(run.into(),args["before_material"].as_i64()).await?,limit)),
        "runs"=>app.tasks.task_run_page(id,args["before_run"].as_i64(),limit).await,
        "processes"=>{
            let all=crate::processes::for_task(app,&id).await?;let total=all.len();let offset=usize::try_from(args["process_offset"].as_u64().unwrap_or(0))?;let next=(offset+limit).min(total);
            Ok(json!({"active_processes":all.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"next_process_offset":next,"has_more":next<total}))
        },
        "plan"=>{
            let plan=&task["plan"];let steps=plan["steps"].as_array().cloned().unwrap_or_default();let offset=usize::try_from(args["plan_offset"].as_u64().unwrap_or(0))?;let next=(offset+limit).min(steps.len());
            let shown=steps.iter().enumerate().skip(offset).take(limit).map(|(index,step)|json!({"index":index,"status":step["status"],"title_preview":crate::tools::truncate(step["title"].as_str().unwrap_or(""),preview)})).collect::<Vec<_>>();
            Ok(json!({"revision":plan["revision"],"assessment":"agent_reported","steps":shown,"step_count":steps.len(),"next_plan_offset":next,"has_more":next<steps.len(),"completion_verified":false}))
        },
        "overview"=>{
            let document_jobs=app.tasks.document_job_page(format!("task:{id}"),None,3).await?;
            let discovery=crate::discovery::catalog(app.data.clone(),id.clone(),None,None,3).await?;
            let requests=request_page(task.clone(),limit,preview);let processes=crate::processes::for_task(app,&id).await?;let jobs=trim_index(app.tasks.material_jobs(run.into(),None).await?,3);
            let plan=&task["plan"];let steps=plan["steps"].as_array();let active=steps.into_iter().flatten().enumerate().filter(|(_,step)|step["status"]=="in_progress").map(|(index,step)|json!({"index":index,"title_preview":crate::tools::truncate(step["title"].as_str().unwrap_or(""),preview)})).collect::<Vec<_>>();
            Ok(json!({"id":id,"project":task["project"],"goal_preview":crate::tools::truncate(task["goal_preview"].as_str().unwrap_or(""),512),"goal_characters":task["goal_characters"],"goal_preview_partial":task["goal_characters"].as_u64().unwrap_or(0)>512,"state":task["state"],"requests":requests["requests"],"next_after":requests["next_after"],"has_more":requests["has_more"],"plan":if plan.is_null(){Value::Null}else{json!({"revision":plan["revision"],"step_count":steps.map_or(0,Vec::len),"active_steps":active,"assessment":"agent_reported"})},"active_processes":processes.iter().take(3).cloned().collect::<Vec<_>>(),"active_process_count":processes.len(),"material_jobs":jobs,"discovery":discovery,"document_jobs":document_jobs,"legacy_context":task["legacy_context"],"completion_verified":false,"requirements":task["requirements"],"sections":["requests","runs","plan","observations","material_jobs","processes","discovery","document_jobs","requirements","requirement_checks","attempts"],"note":"Overview only. Each section is pageable; previews are not full source text. Original requests are read by sequence and offset. Historical claims and plan marks do not verify effects or project completion."}))
        },
        _=>anyhow::bail!("Неизвестный раздел задачи")
    }
}
impl crate::task_store::Store{
    pub async fn task_run_page(&self,task:String,before:Option<i64>,limit:usize)->Result<Value>{
        self.access(move|conn|{
            anyhow::ensure!((1..=32).contains(&limit)&&before.is_none_or(|value|value>0),"Некорректная страница запусков");
            let mut query=conn.prepare("SELECT q.sequence,r.id,r.status,r.created FROM task_requests q JOIN runs r ON r.id=q.run WHERE q.task=?1 AND (?2 IS NULL OR q.sequence<?2) ORDER BY q.sequence DESC LIMIT ?3")?;
            let mut rows=query.query_map(params![task,before,i64::try_from(limit+1)?],|row|Ok(json!({"sequence":row.get::<_,i64>(0)?,"id":row.get::<_,String>(1)?,"status":row.get::<_,String>(2)?,"created":row.get::<_,String>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>limit;rows.truncate(limit);let next=if more{rows.last().map(|row|row["sequence"].clone()).unwrap_or(Value::Null)}else{Value::Null};
            Ok(json!({"runs":rows,"next_before_run":next,"has_more":more,"note":"Run status is not project completion. legacy_imported records contain user requests, not verified effects."}))
        }).await
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn reduced_pages_keep_the_cursor_at_the_last_visible_item(){
        let state=json!({"id":"task","requests":(1..=32).map(|sequence|json!({"sequence":sequence,"preview":"exact 🦀 source"})).collect::<Vec<_>>(),"has_more":false});
        let page=request_page(state,3,0);assert_eq!(page["next_after"],3);assert_eq!(page["has_more"],true);assert_eq!(page["requests"][0]["preview"],"");
        let items=(1..=32).rev().map(|sequence|json!({"sequence":sequence})).collect::<Vec<_>>();let page=trim_index(json!({"items":items,"next_before":null,"has_more":false}),3);
        assert_eq!(page["next_before"],30);assert_eq!(page["items"].as_array().unwrap().len(),3);assert_eq!(page["has_more"],true);
    }
    #[tokio::test]
    async fn run_pages_have_stable_order_and_no_loss_with_equal_timestamps(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();let store=crate::task_store::Store::open(&root).unwrap();
        for index in 0..37{store.begin_in_project(format!("run-{index}"),"chat".into(),json!({"mode":"agent","content":format!("request {index}")}),"project".into()).await.unwrap();}
        let task=store.task_for_run("run-0".into()).await.unwrap().unwrap()["id"].as_str().unwrap().to_string();
        let mut before=None;let mut ids=vec![];loop{let page=store.task_run_page(task.clone(),before,3).await.unwrap();ids.extend(page["runs"].as_array().unwrap().iter().map(|row|row["id"].as_str().unwrap().to_string()));if page["has_more"]!=true{break;}before=page["next_before_run"].as_i64();assert!(before.is_some());}
        assert_eq!(ids,(0..37).rev().map(|index|format!("run-{index}")).collect::<Vec<_>>());assert!(store.task_run_page(task,Some(0),3).await.is_err());drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
