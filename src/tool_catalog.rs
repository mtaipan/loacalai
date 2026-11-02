//! Schema selection changes prompt size, never the server permission boundary.
use anyhow::Result;
use serde_json::{json,Value};
const CORE:&[&str]=&["update_search_plan","read_search_plan","discover_tools","read_task","read_file","ingest_file","inspect_project","search_files","read_artifact","search_artifacts","read_run_log","update_plan"];
pub const GROUPS:&[&str]=&["core","project","files","processes","web","browser","materials","analysis","system"];
fn member(group:&str,name:&str)->bool{match group{
    "core"=>CORE.contains(&name),
    "project"=>["inspect_project","git_info","list_files","find_files","search_files","read_file"].contains(&name),
    "files"=>["read_file","find_files","list_files","search_files","write_file","edit_file","copy_file","move_file","delete_file","translate_file"].contains(&name),
    "processes"=>["run_command","start_process","poll_process","write_stdin","stop_process"].contains(&name),
    "web"=>["web_search","fetch_url","browser_open","browser_read"].contains(&name),
    "browser"=>["browser_open","browser_read","browser_action","fetch_url"].contains(&name),
    "materials"=>["read_artifact","search_artifacts","ingest_file","read_document","read_image","transcribe_audio","translate_text","translate_file"].contains(&name),
    "analysis"=>["security_analyze","inspect_project","git_info"].contains(&name),
    "system"=>["get_system_paths","record_microphone","transcribe_audio","read_image"].contains(&name),
    _=>false,
}}
pub fn visible(allowed:&Value,group:&str)->Value{
    let tools=allowed.as_array().unwrap();
    // Small relevance scopes already fit and retain their specialized protocols.
    // Rich scopes show core plus the current group, without limiting action count.
    if tools.len()<=16{return allowed.clone();}
    json!(tools.iter().filter(|tool|{let name=tool["function"]["name"].as_str().unwrap_or("");CORE.contains(&name)||member(group,name)}).cloned().collect::<Vec<_>>())
}
pub fn discover(allowed:&Value,group:&str)->Result<String>{
    anyhow::ensure!(group=="catalog"||GROUPS.contains(&group),"Неизвестная группа инструментов");
    let tools=allowed.as_array().unwrap();
    let groups=GROUPS.iter().filter(|group|tools.iter().any(|tool|member(group,tool["function"]["name"].as_str().unwrap_or("")))).copied().collect::<Vec<_>>();
    let selected=tools.iter().filter(|tool|member(group,tool["function"]["name"].as_str().unwrap_or(""))).map(|tool|json!({"name":tool["function"]["name"],"description":tool["function"]["description"]})).collect::<Vec<_>>();
    anyhow::ensure!(group=="catalog"||!selected.is_empty(),"Эта группа недоступна в текущей области задачи");
    Ok(json!({"selected_group":if group=="catalog"{Value::Null}else{json!(group)},"groups":groups,"tools":selected,"note":"The next model call receives schemas for core plus the selected group. Other groups remain discoverable; this is not a total action/tool quota. Actual scope and approval policies are enforced independently by the server."}).to_string())
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn rich_tool_sets_stay_discoverable_and_scopes_remain_authoritative(){
        let scope=crate::task_scope::TaskScope::new("Fix the whole project");let all=scope.definitions();let core=visible(&all,"core");assert!(core.as_array().unwrap().len()<all.as_array().unwrap().len());assert!(crate::context::tools_tokens(&core)<crate::context::tools_tokens(&all));
        let processes=visible(&all,"processes");assert!(processes.as_array().unwrap().iter().any(|tool|tool["function"]["name"]=="run_command"));assert!(scope.allows("write_file"));
        let mut readonly=scope;readonly.read_only=true;let allowed=readonly.definitions();assert!(discover(&allowed,"processes").is_err());assert!(!allowed.as_array().unwrap().iter().any(|tool|tool["function"]["name"]=="discover_tools"));
    }
}
