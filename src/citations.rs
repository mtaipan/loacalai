//! Validate reference provenance, never infer that a source supports a claim.
use anyhow::Result;
use serde_json::{json,Value};
use std::collections::{BTreeMap,BTreeSet};
#[derive(Debug)]struct Marker{start:usize,end:usize,id:String}
fn prose_and_links(text:&str)->(String,BTreeSet<String>){
    use pulldown_cmark::{Event,Tag,TagEnd,Parser};
    let mut prose=vec![b' ';text.len()];let mut code=false;let mut html_literal=0usize;let mut links=BTreeSet::new();
    for(event,range)in Parser::new(text).into_offset_iter(){match event{
        Event::Start(Tag::CodeBlock(_))=>code=true,Event::End(TagEnd::CodeBlock)=>code=false,
        Event::Start(Tag::Link{dest_url,..}) if !code&&html_literal==0=>{if let Ok(url)=crate::web_tools::checked_url(&dest_url){links.insert(url.to_string());}},
        Event::InlineHtml(raw)=>{let raw=raw.trim_start().to_ascii_lowercase();if raw.starts_with("</code")||raw.starts_with("</pre"){html_literal=html_literal.saturating_sub(1);}else if raw.starts_with("<code")||raw.starts_with("<pre"){html_literal+=1;}},
        Event::Text(_) if !code&&html_literal==0=>prose[range.clone()].copy_from_slice(&text.as_bytes()[range]),
        _=>{}
    }}
    let prose=String::from_utf8(prose).expect("CommonMark source ranges preserve UTF-8 boundaries");for url in crate::task_scope::urls(&prose){links.insert(url.to_string());}(prose,links)
}
fn markers(text:&str)->Vec<Marker>{
    let(prose,_)=prose_and_links(text);let mut result=vec![];let mut offset=0;while let Some(start)=prose[offset..].find("[cite:"){
        let start=offset+start;let after=start+6;let Some(end)=text[after..].find(']')else{break;};let end=after+end;
        let escaped=text.as_bytes()[..start].iter().rev().take_while(|&&byte|byte==b'\\').count()%2==1;
        if !escaped&&end-after<=100&&!prose[after..end].trim().is_empty(){result.push(Marker{start,end:end+1,id:text[after..end].to_string()});}offset=end+1;
    }result
}
fn resolve(text:&str,known:&Value,indexing:Value)->(String,Value){
    let markers=markers(text);let known=known.as_array().unwrap();let map=known.iter().filter_map(|source|source["source_id"].as_str().map(|id|(id,source))).collect::<BTreeMap<_,_>>();
    let mut text_out=String::new();let mut offset=0;let mut numbers=BTreeMap::new();let mut missing=BTreeSet::new();let mut not_received=BTreeSet::new();
    for marker in &markers{
        text_out.push_str(&text[offset..marker.start]);
        if let Some(source)=map.get(marker.id.as_str()){
            let next=numbers.len()+1;let number=*numbers.entry(marker.id.clone()).or_insert(next);let target=source["url"].as_str().unwrap_or("").replace('(',"%28").replace(')',"%29").replace('[',"%5B").replace(']',"%5D");text_out.push_str(&format!("[{number}]({target})"));
            if !matches!(source["state"].as_str(),Some("received"|"partial")){not_received.insert(marker.id.clone());}
        }else{missing.insert(marker.id.clone());text_out.push_str("[источник не найден]");}offset=marker.end;
    }
    text_out.push_str(&text[offset..]);
    let mut unverified_links=vec![];let mut links=0;let mut link_ids=BTreeSet::new();
    for url in prose_and_links(text).1{let Ok(id)=crate::source_index::source_id(&url)else{continue;};link_ids.insert(id.clone());links+=1;if let Some(source)=map.get(id.as_str()){
        if !matches!(source["state"].as_str(),Some("received"|"partial")){not_received.insert(id);}
    }else{unverified_links.push(url);}}
    let sources=known.iter().filter(|source|{let id=source["source_id"].as_str().unwrap_or("");numbers.contains_key(id)||link_ids.contains(id)}).cloned().collect::<Vec<_>>();
    let report=json!({"structured_markers":markers.len(),"links":links,"resolved":numbers.len(),"missing_ids":missing,"unverified_links":unverified_links,"not_received":not_received,"sources":sources,"indexing":indexing,"scope":"saved_web_source_provenance","semantic_support_verified":false,"full_reading_verified":false,"note":"Resolution proves only that a source was actually discovered/received in this task. It does not prove semantic support, freshness, full reading by the model, or truth. Partial receipts retain their coverage. Missing/foreign IDs never become links."});
    (text_out,report)
}
pub async fn validate(app:&crate::api::Shared,run:&str,text:&str,cancel:&tokio_util::sync::CancellationToken)->Result<(String,Value)>{
    let mut ids=markers(text).into_iter().map(|marker|marker.id).collect::<BTreeSet<_>>();
    for url in prose_and_links(text).1{if let Ok(id)=crate::source_index::source_id(&url){ids.insert(id);}}
    if ids.is_empty(){return Ok((text.into(),json!({"structured_markers":0,"links":0,"semantic_support_verified":false,"scope":"saved_web_source_provenance"})));}
    let indexing=crate::source_index::backfill(app,run,cancel).await?;let matches=app.tasks.web_source_matches(run.into(),ids.into_iter().collect()).await?;Ok(resolve(text,&matches,indexing))
}
pub fn summary(report:&Value)->Value{
    json!({"scope":report["scope"],"structured_markers":report["structured_markers"],"links":report["links"],"resolved":report["resolved"],"missing_ids":report["missing_ids"].as_array().map_or(0,Vec::len),"unverified_links":report["unverified_links"].as_array().map_or(0,Vec::len),"not_received":report["not_received"].as_array().map_or(0,Vec::len),"indexing":report["indexing"],"semantic_support_verified":false})
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn markers_resolve_only_known_sources_without_claiming_support(){
        let known=json!([{"source_id":"S-known","url":"https://example.org/a(x)","state":"partial"},{"source_id":"S-found","url":"https://example.org/b","state":"found"}]);
        let(text,report)=resolve("Fact [cite:S-known], again [cite:S-known]. Other [cite:S-foreign]. Snippet [cite:S-found].",&known,json!({"has_more":false}));
        assert_eq!(text,"Fact [1](https://example.org/a%28x%29), again [1](https://example.org/a%28x%29). Other [источник не найден]. Snippet [2](https://example.org/b).");assert_eq!(report["resolved"],2);assert_eq!(report["missing_ids"],json!(["S-foreign"]));assert_eq!(report["not_received"],json!(["S-found"]));assert_eq!(report["semantic_support_verified"],false);
    }
    #[test]fn code_fences_inline_html_code_escapes_and_reference_links_keep_literal_content(){
        let text="Example `[cite:S-code] https://code.invalid/`\n\n```rust\nlet s = \"[cite:S-fenced]\"; // https://fenced.invalid/\n```\n\n    [cite:S-indented]\n\n<code>[cite:S-html] https://html.invalid/</code>\n\n\\[cite:S-escaped]\n\nReal [cite:S-real]. [ref][doc]\n\n[doc]: https://example.org/a_(b)\n";
        let found=markers(text);assert_eq!(found.len(),1);assert_eq!(found[0].id,"S-real");let(_,links)=prose_and_links(text);assert_eq!(links.len(),1);assert!(links.contains("https://example.org/a_(b)"));
        let known=json!([{"source_id":"S-real","url":"https://source.org/","state":"received"}]);let(resolved,_)=resolve(text,&known,Value::Null);assert!(resolved.contains("`[cite:S-code] https://code.invalid/`"));assert!(resolved.contains("[cite:S-fenced]"));assert!(resolved.contains("\\[cite:S-escaped]"));assert!(resolved.contains("Real [1](https://source.org/)"));
    }
}
