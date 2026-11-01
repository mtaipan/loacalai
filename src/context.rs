//! Shared accounting for the actual inference payload. Estimates are not an
//! exact tokenizer; keep a reserve and expose the chosen limits to the caller.
use anyhow::Result;
use serde_json::{json, Value};
// Transport/memory bounds, independent of the selected model's token window.
pub const MAX_REQUEST_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_TURN_TEXT_BYTES: usize = 7 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// Zero chooses a window automatically, up to auto_max_tokens.
    pub window_tokens: usize,
    pub auto_max_tokens: usize,
    pub output_tokens: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            window_tokens: 0,
            auto_max_tokens: 32768,
            output_tokens: 4096,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.window_tokens == 0 || (4096..=262144).contains(&self.window_tokens),
            "Контекст: Авто или 4096–262144 токенов"
        );
        anyhow::ensure!(
            (8192..=262144).contains(&self.auto_max_tokens),
            "Предел Авто: 8192–262144 токенов"
        );
        anyhow::ensure!(
            (512..=32768).contains(&self.output_tokens),
            "Ответ: 512–32768 токенов"
        );
        let cap = if self.window_tokens == 0 {
            self.auto_max_tokens
        } else {
            self.window_tokens
        };
        anyhow::ensure!(
            self.output_tokens + 1024 < cap,
            "Оставьте в контексте место для запроса, помимо резерва ответа"
        );
        Ok(())
    }
}
pub fn text_tokens(text: &str) -> usize {
    // No JSON escaping of content: quotes/newlines are input text, not a
    // second serialization layer. Non-ASCII remains conservatively weighted.
    let (ascii, other) = text.chars().fold((0usize, 0usize), |(a, b), c| {
        if c.is_ascii() {
            (a + 1, b)
        } else {
            (a, b + 1)
        }
    });
    ascii.div_ceil(3) + other
}
pub fn history_tokens(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|m| {
            text_tokens(m["content"].as_str().unwrap_or(""))
                + m.get("tool_calls")
                    .map(|c| text_tokens(&c.to_string()))
                    .unwrap_or(0)
                + 24
        })
        .sum()
}
pub fn model_limit(show: &Value) -> usize {
    show["model_info"]
        .as_object()
        .into_iter()
        .flat_map(|o| o.iter())
        .filter(|(k, _)| k.ends_with(".context_length"))
        .filter_map(|(_, v)| v.as_u64())
        .max()
        .unwrap_or(16384)
        .clamp(2048, 1048576) as usize
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Plan {
    pub resources: Option<Value>,
    pub window_tokens: usize,
    pub output_tokens: usize,
    pub input_estimate: usize,
    pub tools_estimate: usize,
    pub history_budget: usize,
    pub model_limit: usize,
    pub estimated: bool,
}
pub fn plan(
    config: &Config,
    show: &Value,
    input: usize,
    tools: usize,
    thinking: bool,
    base: usize,
) -> Plan {
    let trained = model_limit(show);
    let cap = (if config.window_tokens == 0 {
        config.auto_max_tokens
    } else {
        config.window_tokens
    })
    .min(trained);
    let output = (if thinking {
        config.output_tokens.max(8192)
    } else {
        config.output_tokens
    })
    .min(cap.saturating_sub(1536))
    .max(256);
    let need = input
        .saturating_add(tools)
        .saturating_add(output)
        .saturating_add(512);
    let mut window = if config.window_tokens == 0 {
        base.min(cap).max(need.min(cap))
    } else {
        cap
    };
    if config.window_tokens == 0 {
        window = [4096, 8192, 16384, 32768, 65536, 131072, 262144, 1048576]
            .into_iter()
            .find(|n| *n >= window)
            .unwrap_or(cap)
            .min(cap);
    }
    Plan {
        resources: None,
        window_tokens: window,
        output_tokens: output,
        input_estimate: input,
        tools_estimate: tools,
        history_budget: window.saturating_sub(output + tools + 512),
        model_limit: trained,
        estimated: true,
    }
}
pub fn tools_tokens(tools: &Value) -> usize {
    if tools.is_null() || tools.as_array().is_some_and(|a| a.is_empty()) {
        0
    } else {
        text_tokens(&tools.to_string())
    }
}
pub fn thinking_enabled(value: &Value, show: &Value) -> bool {
    if value.is_null() {
        crate::thinking::choices(show)
            .iter()
            .any(|v| *v != json!(false))
    } else {
        *value != json!(false)
    }
}
pub fn prepare(
    history: &mut Vec<Value>,
    tools: &Value,
    config: &Config,
    show: &Value,
    thinking: bool,
    base: usize,
) -> Result<(Plan, bool)> {
    config.validate()?;
    let mut p = plan(
        config,
        show,
        history_tokens(history),
        tools_tokens(tools),
        thinking,
        base,
    );
    let reduced=crate::agent::fit_history(history,p.history_budget).map_err(|_|anyhow::anyhow!(
        "Запрос и обязательные инструкции не помещаются в выбранное окно {} токенов (инструменты: около {}, резерв ответа: {}). Полный текущий запрос сохранён. Увеличьте контекст или предел Авто в настройках; предел модели — {}. Оценка токенов приблизительная.",p.window_tokens,p.tools_estimate,p.output_tokens,p.model_limit))?;
    p.input_estimate = history_tokens(history);
    Ok((p, reduced))
}
pub async fn configure_payload(
    app: &crate::api::Shared,
    model: &str,
    payload: &mut Value,
    base: usize,
) -> Result<Plan> {
    let settings = app.settings.lock().await.clone();
    let mut config = settings.context.clone();
    config.validate()?;
    let show = crate::thinking::show(app, model).await?;
    let memory = tokio::task::spawn_blocking(crate::resources::memory).await?;
    let resources = crate::resources::fit_context(
        &mut config,
        &show,
        &memory,
        settings.compute.backend == "cpu"
            || crate::compute::force_cpu(app, &settings.compute).await,
    )?;
    let tools = tools_tokens(&payload["tools"]);
    let input = if let Some(s) = payload["prompt"].as_str() {
        text_tokens(s)
    } else {
        history_tokens(
            payload["messages"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )
    };
    let mut p = plan(
        &config,
        &show,
        input,
        tools,
        thinking_enabled(&payload["think"], &show),
        base,
    );
    p.resources = Some(resources);
    anyhow::ensure!(input<=p.history_budget,"Для этого запроса нужно больше выбранного контекста {}. Запрос сохранён; увеличьте окно или предел Авто в настройках. Предел модели: {}.",p.window_tokens,p.model_limit);
    payload["options"]["num_ctx"] = json!(p.window_tokens);
    payload["options"]["num_predict"] = json!(p.output_tokens);
    Ok(p)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounting_includes_tools_output_and_model_limit() {
        let show = json!({"model_info":{"qwen3moe.context_length":262144}});
        let config = Config::default();
        let p = plan(&config, &show, 10000, 6000, false, 16384);
        assert_eq!(p.window_tokens, 32768);
        assert!(10000 <= p.history_budget);
        let tiny = plan(
            &config,
            &json!({"model_info":{"x.context_length":8192}}),
            10000,
            6000,
            true,
            16384,
        );
        assert_eq!(tiny.window_tokens, 8192);
        assert!(tiny.output_tokens < 8192);
        assert!(Config {
            window_tokens: 4096,
            ..config
        }
        .validate()
        .is_err());
    }
    #[test]
    fn task_survives_old_history_compaction() {
        let task = "Create the full banking wallet project, do not omit requirements.";
        let mut h = vec![
            json!({"role":"system","content":"rules"}),
            json!({"role":"user","content":"old".repeat(40000)}),
            json!({"role":"assistant","content":"old response"}),
            json!({"role":"user","content":task}),
        ];
        let (_, changed) = prepare(
            &mut h,
            &json!([]),
            &Config::default(),
            &json!({}),
            false,
            8192,
        )
        .unwrap();
        assert!(changed);
        assert_eq!(h.last().unwrap()["content"], task);
    }
    #[test]
    fn multilingual_tasks_tools_and_thinking_share_one_budget() {
        let show =
            json!({"model_info":{"qwen.context_length":65536},"thinking":{"values":[false,true]}});
        let task =
            "Требуется полноценный проект: Java, транзакции, блокировки, интеграционные тесты.\n"
                .repeat(90);
        let tools = json!([{"function":{"description":"tool schema ".repeat(2000)}}]);
        let mut h = vec![
            json!({"role":"system","content":"instructions".repeat(100)}),
            json!({"role":"user","content":task}),
        ];
        let (p, _) = prepare(&mut h, &tools, &Config::default(), &show, true, 16384).unwrap();
        assert!(p.window_tokens > 16384);
        assert!(p.input_estimate + p.tools_estimate + p.output_tokens + 512 <= p.window_tokens);
        assert_eq!(h[1]["content"], task);
        assert!(thinking_enabled(&Value::Null, &show));
        assert!(!thinking_enabled(&Value::Null, &json!({})));
        assert_eq!(tools_tokens(&Value::Null), 0);
    }
    #[test]
    fn real_capacity_error_never_shortens_current_task() {
        let task = "原文 длинный текст 🦀 ".repeat(2000);
        let mut h = vec![
            json!({"role":"system","content":"rules"}),
            json!({"role":"user","content":task}),
        ];
        let original = h.clone();
        let config = Config {
            window_tokens: 8192,
            output_tokens: 1024,
            ..Default::default()
        };
        assert!(prepare(&mut h, &json!([]), &config, &json!({}), false, 8192).is_err());
        assert_eq!(h, original);
    }
    #[test]
    fn compaction_moves_on_to_the_next_tool_and_keeps_analysis() {
        let mut h = vec![
            json!({"role":"system","content":"rules"}),
            json!({"role":"user","content":"review results"}),
        ];
        for _ in 0..3 {
            h.push(json!({"role":"tool","content":json!({"status":"succeeded","analysis":"specific finding ".repeat(2000),"coverage":"supplied_text_only","live_testing":false}).to_string()}));
        }
        assert!(crate::agent::fit_history(&mut h, 1000).unwrap());
        for m in &h[2..] {
            let v: Value = serde_json::from_str(m["content"].as_str().unwrap()).unwrap();
            assert_eq!(v["context_truncated"], true);
            assert!(v["analysis"]
                .as_str()
                .unwrap()
                .starts_with("specific finding"));
            assert_eq!(v["live_testing"], false);
        }
    }
}
