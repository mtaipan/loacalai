use serde_json::Value;

pub fn default_translation() -> String {
    "hf.co/IndexTeam/Index-Translate-2B-GGUF:Q6_K".into()
}
pub fn default_assistant() -> String {
    "huihui_ai/qwen3.5-abliterated:4B".into()
}

pub fn catalog() -> Value {
    serde_json::from_str(include_str!("model-catalog.json")).expect("valid model catalog")
}
pub fn valid(id: &str, role: &str) -> bool {
    catalog()
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == id && m["role"] == role)
}
pub fn validate_settings(settings: &crate::api::Settings) -> anyhow::Result<()> {
    anyhow::ensure!(valid(&settings.team_model, "assistant") && settings.team_model.to_lowercase().contains("qwen"),
        "Для команды выберите Qwen как управляющую модель");
    anyhow::ensure!(
        valid(&settings.translation_model, "translate"),
        "Неизвестная модель перевода"
    );
    anyhow::ensure!(
        settings.quick_model.is_empty() || valid(&settings.quick_model, "translate"),
        "Неизвестная модель перевода выделения"
    );
    anyhow::ensure!(
        valid(&settings.agent_model, "assistant") && valid(&settings.chat_model, "assistant"),
        "Выберите модель с поддержкой инструментов для агента и чата"
    );
    Ok(())
}
pub fn installed(name: &str, models: &Value) -> bool {
    models.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|m| m["name"] == name || m["model"] == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translator_cannot_be_used_as_agent() {
        assert!(valid("translategemma:12b", "translate"));
        assert!(!valid("translategemma:12b", "assistant"));
        assert!(valid("qwen2.5:14b", "assistant"));
        assert!(!valid("remote-cloud:latest", "translate"));
    }
}
