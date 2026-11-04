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
                + text_tokens(m["thinking"].as_str().unwrap_or(""))
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
    pub requested_output_tokens: usize,
    pub output_reduced: bool,
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
    plan_at_cap(config,show,input,tools,thinking,base,None,true)
}
fn plan_at_cap(config:&Config,show:&Value,input:usize,tools:usize,thinking:bool,base:usize,effective_cap:Option<usize>,fit_input:bool)->Plan {
    let trained = model_limit(show);
    let cap = (if config.window_tokens == 0 {
        config.auto_max_tokens
    } else {
        config.window_tokens
    })
    .min(trained).min(effective_cap.unwrap_or(usize::MAX));
    let requested_output = if thinking {
        config.output_tokens.max(8192)
    } else {
        config.output_tokens
    };
    let mut output=requested_output.min(cap.saturating_sub(1536)).max(256);
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
    if fit_input{output=output.min(window.saturating_sub(input.saturating_add(tools).saturating_add(512))).max(256);}
    Plan {
        resources: None,
        window_tokens: window,
        output_tokens: output,
        requested_output_tokens:requested_output,
        output_reduced:output<requested_output,
        input_estimate: input,
        tools_estimate: tools,
        history_budget: window.saturating_sub(output + tools + 512),
        model_limit: trained,
        estimated: true,
    }
}
#[derive(Debug)]
struct Capacity {window:usize,minimum:usize,tools:usize,output:usize,model:usize}
impl std::fmt::Display for Capacity{
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{write!(f,"Запрос и обязательные инструкции не помещаются в окно {} токенов: нужно около {} с минимальным резервом ответа (инструменты: около {}, текущий резерв ответа: {}). Текущий запрос сохранён полностью. Выберите большее окно или меньшую часть материала; предел модели — {}. Оценка приблизительная.",self.window,self.minimum,self.tools,self.output,self.model)}
}
impl std::error::Error for Capacity{}
fn capacity(p:&Plan)->anyhow::Error{Capacity{window:p.window_tokens,minimum:p.input_estimate.saturating_add(p.tools_estimate).saturating_add(512+256),tools:p.tools_estimate,output:p.output_tokens,model:p.model_limit}.into()}
fn resource_error(config:&Config,show:&Value,resources:&Value,error:anyhow::Error)->anyhow::Error{
    if config.window_tokens!=0{return error;}
    let Some(issue)=error.downcast_ref::<Capacity>() else{return error;};
    let cap=config.auto_max_tokens.min(model_limit(show));
    if resources["safe_window_tokens"].as_u64().is_some_and(|safe|safe<(issue.minimum as u64))&&issue.minimum<=cap{
        if resources["physical_safe_window_tokens"].as_u64().is_some_and(|safe|safe>=issue.minimum as u64){
            return crate::resources::MemoryPressure(format!("Для сохранённого запроса и инструментов нужно окно около {} токенов; по текущей RAM доступно около {}. Ожидание освобождения памяти; оценка приблизительная.",issue.minimum,resources["safe_window_tokens"])).into();
        }
        return anyhow::anyhow!("Для текущего запроса и инструментов нужно около {} токенов, а оценка RAM устройства допускает около {}. Запрос сохранён. Выберите меньшую модель или обработку материала частями; увеличение предела Авто не добавит RAM. Оценка приблизительная.",issue.minimum,resources["physical_safe_window_tokens"]);
    }
    error
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
    prepare_with_cap(history,tools,config,show,thinking,base,None)
}
fn prepare_with_cap(history:&mut Vec<Value>,tools:&Value,config:&Config,show:&Value,thinking:bool,base:usize,effective_cap:Option<usize>)->Result<(Plan,bool)>{
    config.validate()?;
    let mut p = plan_at_cap(
        config,
        show,
        history_tokens(history),
        tools_tokens(tools),
        thinking,
        base,
        effective_cap,false,
    );
    // Compact recoverable old rounds before reducing the response reserve. The
    // mandatory current request and tools receive space before optional thinking.
    let initial=history_tokens(history);
    if crate::agent::fit_history(history,p.history_budget).is_err(){
        p.input_estimate=history_tokens(history);
        let remaining=p.window_tokens.saturating_sub(p.input_estimate.saturating_add(p.tools_estimate).saturating_add(512));
        if remaining>=256 {p.output_tokens=p.output_tokens.min(remaining);p.output_reduced=p.output_tokens<p.requested_output_tokens;p.history_budget=p.window_tokens.saturating_sub(p.output_tokens+p.tools_estimate+512);}
    }
    p.input_estimate=history_tokens(history);
    let reduced=crate::agent::fit_history(history,p.history_budget).map_err(|_|capacity(&p))?;
    p.input_estimate = history_tokens(history);
    Ok((p, reduced||history_tokens(history)!=initial))
}
pub async fn configure_payload(
    app: &crate::api::Shared,
    model: &str,
    payload: &mut Value,
    base: usize,
) -> Result<Plan> {
    let settings = crate::generation::capture(app).await;
    let config = settings.context.clone();
    config.validate()?;
    let mut fitted=config.clone();
    let show = crate::thinking::show(app, model).await?;
    let owned=crate::engine::owns_engine();
    let mut resources=if owned{
        let memory=tokio::task::spawn_blocking(crate::resources::memory).await?;
        crate::resources::fit_context(&mut fitted,&show,&memory,settings.compute.backend=="cpu"||crate::compute::force_cpu(app,&settings.compute).await)?
    }else{json!({"estimated":false,"reason":"external_engine_resources_unknown"})};
    resources["engine_owned"]=json!(owned);
    resources["generation_settings_revision"]=json!(settings.revision);
    let requested=settings.compute.cpu_threads;
    let cpu=tokio::task::spawn_blocking(move||crate::resources::cpu_threads(requested,owned)).await??;
    if let Some(threads)=cpu["effective_threads"].as_u64(){payload["options"]["num_thread"]=json!(threads);}
    resources["cpu_threads"]=cpu;
    let effective_cap=if config.window_tokens==0{Some(fitted.auto_max_tokens)}else{None};
    let definitions=payload["tools"].clone();let thinking=thinking_enabled(&payload["think"],&show);
    if let Some(messages)=payload.get_mut("messages").and_then(Value::as_array_mut){
        let(mut p,_)=prepare_with_cap(messages,&definitions,&config,&show,thinking,base,effective_cap).map_err(|error|resource_error(&config,&show,&resources,error))?;p.resources=Some(resources);
        payload["options"]["num_ctx"]=json!(p.window_tokens);payload["options"]["num_predict"]=json!(p.output_tokens);return Ok(p);
    }
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
    let mut p = plan_at_cap(
        &config,
        &show,
        input,
        tools,
        thinking_enabled(&payload["think"], &show),
        base,effective_cap,true,
    );
    p.resources = Some(resources);
    if input>p.history_budget{return Err(resource_error(&config,&show,p.resources.as_ref().unwrap(),capacity(&p)));}
    payload["options"]["num_ctx"] = json!(p.window_tokens);
    payload["options"]["num_predict"] = json!(p.output_tokens);
    Ok(p)
}
#[cfg(test)]mod thinking_accounting_tests{
    use super::*;
    #[test]fn reasoning_returned_with_tool_calls_is_part_of_the_next_request_budget(){
        let plain=json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_search_plan","arguments":{}}}]});let mut thinking=plain.clone();thinking["thinking"]=json!("reasoning 🦀 ".repeat(1000));
        assert_eq!(history_tokens(&[thinking.clone()])-history_tokens(&[plain]),text_tokens(thinking["thinking"].as_str().unwrap()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_constrained_required_input_waits_but_real_model_or_manual_limits_do_not(){
        let config=Config::default();let show=json!({"model_info":{"x.context_length":262144}});let resources=json!({"safe_window_tokens":9987,"physical_safe_window_tokens":65536});
        let error=Capacity{window:9987,minimum:12000,tools:2286,output:256,model:262144};
        assert!(resource_error(&config,&show,&resources,error.into()).is::<crate::resources::MemoryPressure>());
        let p=plan(&config,&show,40000,0,false,8192);
        assert!(!resource_error(&config,&show,&resources,capacity(&p)).is::<crate::resources::MemoryPressure>());
        let manual=Config{window_tokens:9987,..config};
        assert!(!resource_error(&manual,&show,&resources,Capacity{window:9987,minimum:12000,tools:2286,output:256,model:262144}.into()).is::<crate::resources::MemoryPressure>());
    }
    #[test]
    fn response_reserve_never_crowds_out_short_request_and_tool_definitions(){
        let show=json!({"model_info":{"qwen.context_length":262144}});
        let mut history=vec![json!({"role":"system","content":"s".repeat(1800)}),json!({"role":"user","content":"симонс"})];
        let tools=json!([{"function":{"name":"search","description":"t".repeat(6700)}}]);
        let original=history.clone();let (p,_)=prepare_with_cap(&mut history,&tools,&Config::default(),&show,true,16384,Some(9987)).unwrap();
        assert_eq!(history,original);assert_eq!(p.window_tokens,9987);assert_eq!(p.requested_output_tokens,8192);assert!(p.output_reduced);
        assert!(p.output_tokens>=256);assert!(p.input_estimate+p.tools_estimate+p.output_tokens+512<=p.window_tokens);
        // Runtime RAM ceilings are not invalid user preferences (Auto minimum8k).
        let (p,_)=prepare_with_cap(&mut history,&json!([]),&Config::default(),&show,true,16384,Some(4096)).unwrap();assert_eq!(p.window_tokens,4096);
        let p=plan_at_cap(&Config::default(),&show,1500,2286,true,16384,Some(9987),true);assert_eq!(p.output_tokens,5689);assert_eq!(p.history_budget,1500);
    }
    #[test]
    fn recoverable_history_is_removed_before_optional_response_reserve_is_reduced(){
        let mut history=vec![json!({"role":"system","content":"rules"}),json!({"role":"user","content":"old".repeat(20000)}),json!({"role":"assistant","content":"old reply"}),json!({"role":"user","content":"current task"})];
        let (p,changed)=prepare_with_cap(&mut history,&json!([]),&Config::default(),&json!({}),true,16384,Some(9987)).unwrap();assert!(changed);assert_eq!(p.output_tokens,8192);assert!(!p.output_reduced);assert_eq!(history.last().unwrap()["content"],"current task");
    }
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
