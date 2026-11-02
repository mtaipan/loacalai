//! Acquire the data-home lease before opening databases or recovering jobs.
//! An old PID or an open port is never proof of ownership of either resource.
use anyhow::{Context,Result};
use serde_json::{json,Value};
use std::{fs::{File,OpenOptions},path::{Path,PathBuf},time::Duration};
pub struct Owner { _lock:File, data:PathBuf }
pub fn acquire(root:&Path)->Result<Option<Owner>>{
    let data=root.join("data");std::fs::create_dir_all(&data)?;
    // Keep this inode permanently; replacing/deleting the lock file would allow
    // another process to lock a different file while the first still owns it.
    let file=OpenOptions::new().read(true).write(true).create(true).truncate(false).open(data.join("instance.lock"))?;
    match file.try_lock(){
        Ok(())=>Ok(Some(Owner{_lock:file,data})),
        Err(std::fs::TryLockError::WouldBlock)=>Ok(None),
        Err(error)=>Err(error.into()),
    }
}
impl Owner{
    pub fn publish(&self,root:&Path,port:u16)->Result<()>{
        crate::changes::replace(&self.data.join("instance.json"),&serde_json::to_vec(&json!({"root":root,"port":port,"pid":std::process::id()}))?)
    }
}
pub fn port(root:&Path)->Result<u16>{
    let record:Value=serde_json::from_slice(&std::fs::read(root.join("data/instance.json"))?)?;
    let actual=dunce::canonicalize(record["root"].as_str().context("Не указан владелец данных приложения")?)?;
    anyhow::ensure!(actual==root,"Запись экземпляра относится к другой папке");
    let port=u16::try_from(record["port"].as_u64().context("Не указан порт экземпляра")?)?;
    anyhow::ensure!(port>0,"Не указан порт экземпляра");Ok(port)
}
pub async fn existing(root:&Path,port:u16)->Result<(String,Value)>{
    let url=format!("http://127.0.0.1:{port}");
    let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build()?;
    let value:Value=client.get(format!("{url}/api/bootstrap")).send().await?.error_for_status()?.json().await?;
    let actual=dunce::canonicalize(value["root"].as_str().context("Порт занят другим сервисом")?)?;
    anyhow::ensure!(value["app"]=="Local Gemma Studio"&&actual==root,"Порт {port} занят приложением с другой папкой данных");
    Ok((url,value))
}
pub async fn activate(url:&str,bootstrap:&Value)->Result<bool>{
    let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build()?;
    let value:Value=client.post(format!("{url}/api/activate")).header("X-Studio-Token",bootstrap["token"].as_str().context("Не указан token экземпляра")?).json(&json!({})).send().await?.error_for_status()?.json().await?;
    Ok(value["native"]==true)
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn lease_survives_metadata_replacement_and_is_released_by_owner(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();let root=dunce::canonicalize(root).unwrap();
        let owner=acquire(&root).unwrap().unwrap();owner.publish(&root,3210).unwrap();assert_eq!(port(&root).unwrap(),3210);assert!(acquire(&root).unwrap().is_none());
        owner.publish(&root,3211).unwrap();assert!(acquire(&root).unwrap().is_none());drop(owner);assert!(acquire(&root).unwrap().is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
}
