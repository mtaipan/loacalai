//! Immutable inference preferences for one admitted operation. Access policy stays live.
use std::{future::Future, sync::Arc};
use crate::api::{Settings, Shared};

#[derive(Clone)]
pub struct Config {
    pub revision: u64,
    pub compute: crate::compute::Config,
    pub context: crate::context::Config,
    pub assistant: crate::agent::AssistantConfig,
    pub translation_model: String,
    pub reranker_enabled: bool,
}
impl From<&Settings> for Config {
    fn from(settings: &Settings) -> Self {
        Self { revision: settings.revision, compute: settings.compute.clone(),
            context: settings.context.clone(), assistant: settings.assistant.clone(),
            translation_model: settings.translation_model.clone(),
            reranker_enabled: settings.web.reranker_enabled }
    }
}
tokio::task_local! { static CURRENT: Arc<Config>; }
pub async fn capture(app: &Shared) -> Arc<Config> {
    if let Ok(config) = CURRENT.try_with(Arc::clone) { return config; }
    let settings = app.settings.lock().await;
    Arc::new(Config::from(&*settings))
}
pub async fn scope<F: Future>(config: Arc<Config>, operation: F) -> F::Output {
    CURRENT.scope(config, operation).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn admitted_preferences_survive_edits_nested_operations_and_concurrency() {
        let root=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("generation-{}",uuid::Uuid::new_v4()));
        let app=Arc::new(crate::api::App::new(root.clone()).unwrap());
        let original=capture(&app).await;
        let old_model=original.translation_model.clone();
        scope(original.clone(), async {
            { let mut next=app.settings.lock().await; next.revision+=1;
                next.translation_model="next-model".into(); next.context.auto_max_tokens=65536;
                next.permission_mode="full".into(); }
            let current=capture(&app).await;
            assert!(Arc::ptr_eq(&current,&original));
            assert_eq!(current.translation_model,old_model);
            assert_eq!(app.settings.lock().await.permission_mode,"full");
            // A separate admission receives current preferences, not another task's snapshot.
            let worker=app.clone();
            let next=tokio::spawn(async move {capture(&worker).await}).await.unwrap();
            assert_eq!(next.translation_model,"next-model");
            scope(next,async {assert_eq!(capture(&app).await.translation_model,"next-model");}).await;
            assert_eq!(capture(&app).await.translation_model,old_model);
        }).await;
        assert_eq!(capture(&app).await.translation_model,"next-model");
        drop(app); std::fs::remove_dir_all(root).unwrap();
    }
}
