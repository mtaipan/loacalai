//! Coordinate installer publication with startup before creating the data home.
//! The mutex lives outside the renamed tree; the instance lease protects lifetime.
use anyhow::{Context,Result};
use serde::Deserialize;
use sha2::{Digest,Sha256};
use std::{io::Read,path::{Path,PathBuf}};

pub fn mutex_name(root:&Path)->String{
    // Matches Setup.cs: physical path, ASCII case fold, UTF-8 SHA256. Non-ASCII
    // case conversion intentionally does not depend on Unicode library versions.
    let normalized=root.to_string_lossy().replace('/',"\\").trim_end_matches('\\').chars().map(|c|if c.is_ascii_lowercase(){c.to_ascii_uppercase()}else{c}).collect::<String>();
    format!("Global\\LocalGemmaStudio.Install.{:x}",Sha256::digest(normalized.as_bytes()))
}
fn journal_path(root:&Path)->Result<PathBuf>{Ok(root.parent().context("Не указана папка установки")?.join(format!(".{}.update.json",root.file_name().context("Не указано имя папки установки")?.to_string_lossy())))}
#[derive(Deserialize)]
struct Pending{
    #[serde(rename="Version")] version:u32,
    #[serde(rename="Root")] root:PathBuf,
    #[serde(rename="Stage")] stage:PathBuf,
    #[serde(rename="Backup")] backup:PathBuf,
    #[serde(rename="Phase")] phase:String,
    #[serde(rename="HadRoot")] had_root:bool,
}
fn linked(path:&Path)->bool{
    let Ok(metadata)=std::fs::symlink_metadata(path)else{return false;};
    #[cfg(windows)]{use std::os::windows::fs::MetadataExt;metadata.file_type().is_symlink()||metadata.file_attributes()&0x400!=0}
    #[cfg(not(windows))]{metadata.file_type().is_symlink()}
}
fn pending(root:&Path)->Result<()>{
    let journal=journal_path(root)?;
    if !journal.exists(){return Ok(());}
    anyhow::ensure!(!linked(&journal),"Журнал обновления является ссылкой; запуск остановлен до открытия данных");
    // Skip Carry while deserializing: millions of preserved paths need not be
    // allocated merely to decide whether startup may acquire the instance lease.
    let state:Pending=serde_json::from_reader(std::fs::File::open(&journal)?).context("Журнал обновления повреждён; откройте установщик для восстановления")?;
    anyhow::ensure!(state.version==1&&mutex_name(&state.root)==mutex_name(root),"Журнал обновления относится к другой папке или версии; запуск остановлен");
    for(path,suffix)in[(&state.stage,"stage"),(&state.backup,"backup")]{
        let prefix=format!(".{}.{}-",root.file_name().unwrap().to_string_lossy(),suffix);
        let name=path.file_name().and_then(|s|s.to_str()).unwrap_or("");
        anyhow::ensure!(path.parent()==root.parent()&&name.strip_prefix(&prefix).is_some_and(|id|id.len()==32&&uuid::Uuid::parse_str(id).is_ok())&&!linked(path),"Небезопасный путь в журнале обновления; запуск остановлен");
    }
    anyhow::ensure!(["staging","prepared","old_moved","carried","published","committed"].contains(&state.phase.as_str()),"Неизвестная фаза обновления; запуск остановлен");
    if matches!(state.phase.as_str(),"staging"|"prepared")&&state.had_root&&root.is_dir()&&!state.backup.exists(){return Ok(());}
    let marker=root.join("install-publication.txt");
    if root.is_dir()&&!state.stage.exists()&&!linked(&marker){
        if let Ok(file)=std::fs::File::open(&marker){
            let expected=state.stage.to_string_lossy();let mut bytes=Vec::new();file.take(expected.len() as u64+1).read_to_end(&mut bytes)?;
            if bytes==expected.as_bytes(){return Ok(());}
        }
    }
    anyhow::bail!("Обновление приложения прервано. Откройте установщик для восстановления: {}. Данные не открывались",journal.display())
}
#[cfg(windows)]
mod platform{
    use super::*;
    use std::ffi::c_void;
    #[link(name="kernel32")]
    extern "system"{
        fn CreateMutexW(attributes:*const c_void,owner:i32,name:*const u16)->*mut c_void;
        fn WaitForSingleObject(handle:*mut c_void,milliseconds:u32)->u32;
        fn ReleaseMutex(handle:*mut c_void)->i32;
        fn CloseHandle(handle:*mut c_void)->i32;
    }
    pub struct Guard(*mut c_void);
    impl Drop for Guard{fn drop(&mut self){unsafe{ReleaseMutex(self.0);CloseHandle(self.0);}}}
    pub fn acquire(root:&Path)->Result<Guard>{
        let name=mutex_name(root).encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let handle=unsafe{CreateMutexW(std::ptr::null(),0,name.as_ptr())};
        anyhow::ensure!(!handle.is_null(),"Не удалось открыть блокировку обновления: {}",std::io::Error::last_os_error());
        match unsafe{WaitForSingleObject(handle,0)}{
            0|0x80=>Ok(Guard(handle)),
            status=>{unsafe{CloseHandle(handle);}if status==0x102{anyhow::bail!("Приложение обновляется. Откройте его после завершения установки; данные не открывались");}anyhow::bail!("Не удалось проверить блокировку обновления: {}",std::io::Error::last_os_error())}
        }
    }
}
#[cfg(windows)]pub struct Guard{_native:platform::Guard}
#[cfg(not(windows))]pub struct Guard;
pub fn acquire(root:&Path)->Result<Guard>{
    #[cfg(windows)]{let native=platform::acquire(root)?;pending(root)?;Ok(Guard{_native:native})}
    #[cfg(not(windows))]{let _=root;Ok(Guard)}
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn publication_mutex_identity_matches_the_installer_utf8_contract(){
        assert_eq!(mutex_name(Path::new(r"C:\Guard Fixture\app")),r"Global\LocalGemmaStudio.Install.bd7cbeadf65ef416cfe3741a70d4423db971e356ef938f723b9aa75b7b71782d");
        assert_eq!(mutex_name(Path::new(r"c:/guard fixture/app/")),mutex_name(Path::new(r"C:\GUARD FIXTURE\APP")));
    }
    #[test]fn interrupted_publish_never_creates_a_data_home_and_complete_publish_is_admitted(){
        let area=std::env::temp_dir().join(format!("install-guard-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(&area).unwrap();let root=area.join("app");let stage=area.join(format!(".app.stage-{}",uuid::Uuid::new_v4().simple()));let backup=area.join(format!(".app.backup-{}",uuid::Uuid::new_v4().simple()));std::fs::create_dir_all(&stage).unwrap();std::fs::create_dir_all(&backup).unwrap();
        let journal=journal_path(&root).unwrap();let state=serde_json::json!({"Version":1,"Root":root,"Stage":stage,"Backup":backup,"Phase":"old_moved","HadRoot":true,"Carry":["data"]});std::fs::write(&journal,state.to_string()).unwrap();
        assert!(pending(&root).is_err());assert!(!root.exists());
        std::fs::create_dir_all(&root).unwrap();assert!(pending(&root).is_err());assert!(!root.join("data").exists());
        std::fs::remove_dir(&stage).unwrap();std::fs::write(root.join("install-publication.txt"),stage.to_string_lossy().as_bytes()).unwrap();pending(&root).unwrap();
        assert!(!root.join("data").exists());std::fs::remove_dir_all(area).unwrap();
    }
    #[test]fn staging_does_not_block_startup_of_the_untouched_old_release(){
        let area=std::env::temp_dir().join(format!("install-guard-{}",uuid::Uuid::new_v4()));let root=area.join("app");std::fs::create_dir_all(&root).unwrap();let stage=area.join(format!(".app.stage-{}",uuid::Uuid::new_v4().simple()));let backup=area.join(format!(".app.backup-{}",uuid::Uuid::new_v4().simple()));
        let state=serde_json::json!({"Version":1,"Root":root,"Stage":stage,"Backup":backup,"Phase":"staging","HadRoot":true});std::fs::write(journal_path(&root).unwrap(),state.to_string()).unwrap();pending(&root).unwrap();
        std::fs::create_dir_all(backup).unwrap();assert!(pending(&root).is_err());assert!(!root.join("data").exists());std::fs::remove_dir_all(area).unwrap();
    }
}
