//! Windowed browser history. Model history and extracted material are never decoded here.
use anyhow::Result;
use serde::{Deserialize,Serialize};
use serde::de::{DeserializeSeed,IgnoredAny,MapAccess,SeqAccess,Visitor};
use serde_json::{json,Value};
use std::{fmt,path::Path,io::BufReader};

#[derive(Serialize,Deserialize)]
pub struct VisibleMessage{
    pub id:String,pub role:String,pub content:String,pub created:String,
    #[serde(default,skip_serializing_if="Option::is_none")]pub model:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub diagnostics:Option<Value>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub duration_ms:Option<u64>,
    #[serde(default)]pub attachments:Vec<crate::api::Attachment>,
    #[serde(default)]pub tools:Vec<Value>,
}
impl From<crate::api::Message> for VisibleMessage{
    fn from(m:crate::api::Message)->Self{Self{id:m.id,role:m.role,content:m.content,created:m.created,model:m.model,diagnostics:m.diagnostics,duration_ms:m.duration_ms,attachments:m.attachments,tools:m.tools}}
}
#[derive(Debug)]pub struct Changed;
impl fmt::Display for Changed{fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{f.write_str("Чат изменился между страницами. Откройте последние сообщения.")}}
impl std::error::Error for Changed{}
struct Window{start:u64,end:u64}
impl<'de> DeserializeSeed<'de> for Window{
    type Value=Vec<VisibleMessage>;
    fn deserialize<D:serde::Deserializer<'de>>(self,d:D)->std::result::Result<Self::Value,D::Error>{d.deserialize_seq(self)}
}
impl<'de> Visitor<'de> for Window{
    type Value=Vec<VisibleMessage>;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result{f.write_str("message array")}
    fn visit_seq<A:SeqAccess<'de>>(self,mut seq:A)->std::result::Result<Self::Value,A::Error>{
        let mut index=0u64;let mut messages=vec![];
        loop{
            let found=if index>=self.start&&index<self.end{match seq.next_element::<VisibleMessage>()?{Some(m)=>{messages.push(m);true},None=>false}}else{seq.next_element::<IgnoredAny>()?.is_some()};
            if !found{break;}index=index.checked_add(1).ok_or_else(||serde::de::Error::custom("Message count overflow"))?;
        }
        Ok(messages)
    }
}
struct Root(Window);
impl<'de> DeserializeSeed<'de> for Root{
    type Value=Vec<VisibleMessage>;
    fn deserialize<D:serde::Deserializer<'de>>(self,d:D)->std::result::Result<Self::Value,D::Error>{d.deserialize_map(self)}
}
impl<'de> Visitor<'de> for Root{
    type Value=Vec<VisibleMessage>;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result{f.write_str("conversation object")}
    fn visit_map<A:MapAccess<'de>>(self,mut map:A)->std::result::Result<Self::Value,A::Error>{
        let mut messages=None;
        while let Some(key)=map.next_key::<String>()?{
            if key=="messages"{if messages.is_some(){return Err(serde::de::Error::duplicate_field("messages"));}messages=Some(map.next_value_seed(Window{start:self.0.start,end:self.0.end})?);}else{map.next_value::<IgnoredAny>()?;}
        }
        messages.ok_or_else(||serde::de::Error::missing_field("messages"))
    }
}
pub fn page(path:&Path,before:Option<u64>,limit:u64,expected:Option<&str>)->Result<Value>{
    anyhow::ensure!((1..=32).contains(&limit),"Размер страницы сообщений от1 до32");
    let metadata=std::fs::metadata(path)?;let modified=metadata.modified()?.duration_since(std::time::UNIX_EPOCH)?;
    let revision=format!("{}-{}-{}",metadata.len(),modified.as_secs(),modified.subsec_nanos());
    if expected.is_some_and(|v|v!=revision){return Err(Changed.into());}
    let header=crate::chat_headers::read(path)?;let end=before.unwrap_or(header.count);
    if end>header.count{return Err(Changed.into());}let start=end.saturating_sub(limit);
    let mut decoder=serde_json::Deserializer::from_reader(BufReader::with_capacity(64*1024,std::fs::File::open(path)?));
    let mut messages=Root(Window{start,end}).deserialize(&mut decoder)?;decoder.end()?;
    let after=std::fs::metadata(path)?;
    if after.len()!=metadata.len()||after.modified()?!=metadata.modified()?{return Err(Changed.into());}
    anyhow::ensure!(messages.len() as u64==end-start,"Число сообщений изменилось при чтении");
    for m in &mut messages{for tool in &mut m.tools{crate::tools::public_record(tool);}}
    Ok(json!({"id":header.id,"title":header.title,"updated":header.updated,"mode":header.mode,"messages":messages,"message_page":{"start":start,"end":end,"total":header.count,"before":if start>0{Some(start)}else{None},"at_latest":end==header.count,"revision":revision,"limit":limit}}))
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn windows_preserve_indices_omit_material_and_reject_stale_or_invalid_pages(){
        let root=std::env::temp_dir().join(format!("chat-pages-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(&root).unwrap();let id=uuid::Uuid::new_v4().to_string();let path=root.join(format!("{id}.json"));
        let mut source=json!({"id":id,"title":"fixture","updated":"now","mode":"chat","messages":(0..73).map(|i|json!({"id":i.to_string(),"role":"user","content":format!("text {i} 🦀"),"created":"now","material":"private extracted text"})).collect::<Vec<_>>(),"history":[{"content":"private model state"}]});std::fs::write(&path,source.to_string()).unwrap();
        let last=page(&path,None,24,None).unwrap();assert_eq!(last["message_page"]["start"],49);assert_eq!(last["messages"][0]["id"],"49");assert!(!last.to_string().contains("private"));
        let revision=last["message_page"]["revision"].as_str().unwrap();let older=page(&path,Some(49),24,Some(revision)).unwrap();assert_eq!(older["message_page"]["start"],25);assert_eq!(older["messages"][23]["id"],"48");
        assert!(page(&path,None,0,None).is_err());assert!(page(&path,Some(74),24,None).unwrap_err().is::<Changed>());
        source["messages"].as_array_mut().unwrap().push(json!({"id":"73","role":"user","content":"new","created":"now"}));std::fs::write(&path,source.to_string()).unwrap();assert!(page(&path,Some(49),24,Some(revision)).unwrap_err().is::<Changed>());assert_eq!(page(&path,Some(3),24,None).unwrap()["messages"].as_array().unwrap().len(),3);
        std::fs::remove_dir_all(root).unwrap();
    }
}
