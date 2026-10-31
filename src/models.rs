use serde_json::{json, Value};

pub fn default_translation() -> String {
    "translategemma:4b".into()
}
pub fn default_assistant() -> String {
    "qwen2.5:3b".into()
}

pub fn catalog() -> Value {
    json!([
        {"id":"hf.co/IndexTeam/Index-Translate-2B-GGUF:Q6_K","name":"Index-Translate 2B · Q6","role":"translate","size":"1,61 ГБ","description":"Компактный переводчик; терминология, Markdown и форматирование. Кандидат для быстрого перевода."},
        {"id":"hf.co/tencent/Hy-MT2-1.8B-GGUF:Q8_0","name":"Hy-MT2 1.8B · Q8","role":"translate","size":"1,91 ГБ","description":"Лёгкая альтернатива для перевода выделения на CPU."},
        {"id":"translategemma:4b","name":"TranslateGemma 4B","role":"translate","size":"3,3 ГБ","description":"Текущая модель перевода. Оставлена для сравнения."},
        {"id":"hf.co/tencent/Hy-MT2-7B-GGUF:Q4_K_M","name":"Hy-MT2 7B · Q4","role":"translate","size":"4,62 ГБ","description":"Более тяжёлый режим; на CPU ответ может занимать заметно больше времени."},
        {"id":"hf.co/IndexTeam/Index-Translate-9B-GGUF:Q4_K_M","name":"Index-Translate 9B · Q4","role":"translate","size":"5,78 ГБ","description":"Перевод с требованиями к стилю, терминологии и структуре."},
        {"id":"translategemma:12b","name":"TranslateGemma 12B","role":"translate","size":"8,1 ГБ","description":"Тяжёлый переводчик Google. Памяти нужно больше размера весов."},
        {"id":"translategemma:27b","name":"TranslateGemma 27B","role":"translate","size":"17 ГБ","description":"Для большого запаса RAM/VRAM. На Ryzen 5600X CPU перевод будет медленным."},
        {"id":"qwen2.5:3b","name":"Qwen2.5 3B","role":"assistant","size":"1,9 ГБ","description":"Текущий компактный чат и агент с инструментами."},
        {"id":"qwen2.5:7b","name":"Qwen2.5 7B","role":"assistant","size":"4,7 ГБ","description":"Более крупная модель для чата и инструментов. Перевод выполняет выбранный переводчик."},
        {"id":"qwen2.5:14b","name":"Qwen2.5 14B","role":"assistant","size":"9 ГБ","description":"Тяжёлый чат и агент; на CPU работает медленнее."},
        {"id":"richardyoung/qwen3-14b-abliterated:latest","name":"Qwen3 14B Abliterated · Q4","role":"assistant","size":"9 ГБ","description":"Версия Richard Young с уменьшенной склонностью к отказам. На CPU требует больше времени."},
        {"id":"huihui_ai/qwen3.5-abliterated:2B","name":"Qwen3.5 2B Abliterated · Q4","role":"assistant","size":"1,9 ГБ","description":"Компактная версия Huihui для локального чата и инструментов."},
        {"id":"huihui_ai/qwen3.5-abliterated:4B","name":"Qwen3.5 4B Abliterated · Q4","role":"assistant","size":"3,3 ГБ","description":"Версия Huihui среднего размера для чата и агента."},
        {"id":"huihui_ai/qwen3.5-abliterated:9b","name":"Qwen3.5 9B Abliterated · Q4","role":"assistant","size":"6,6 ГБ","description":"Более крупная версия Huihui для чата и инструментов; на CPU медленнее 2B и 4B."}
    ])
}
pub fn valid(id: &str, role: &str) -> bool {
    catalog()
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == id && m["role"] == role)
}
pub fn validate_settings(settings: &crate::api::Settings) -> anyhow::Result<()> {
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
