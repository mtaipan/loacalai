use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct AssistantConfig {
    pub agent_instructions: String,
    pub chat_instructions: String,
    pub agent_temperature: f64,
    pub chat_temperature: f64,
    pub repeat_penalty: f64,
}
impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            agent_instructions: String::new(),
            chat_instructions: String::new(),
            agent_temperature: 0.1,
            chat_temperature: 0.7,
            repeat_penalty: 1.05,
        }
    }
}
impl AssistantConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.agent_instructions.chars().count() <= 8000
                && self.chat_instructions.chars().count() <= 8000,
            "Дополнительные инструкции: до 8000 символов для каждого режима"
        );
        anyhow::ensure!(
            [self.agent_temperature, self.chat_temperature]
                .iter()
                .all(|n| n.is_finite() && (0.0..=2.0).contains(n)),
            "Температура: от 0 до 2"
        );
        anyhow::ensure!(
            self.repeat_penalty.is_finite() && (1.0..=1.5).contains(&self.repeat_penalty),
            "Штраф повторов: от 1 до 1,5"
        );
        Ok(())
    }
}

/// This is a bounded answer-quality check, never an authorization decision.
pub fn needs_grounding(text: &str, tools: &[serde_json::Value]) -> bool {
    // A faithful quote of the file may itself contain one of these phrases.
    if tools
        .iter()
        .filter(|t| t["name"] == "read_file" && t["status"] == "succeeded")
        .any(|t| {
            serde_json::from_str::<serde_json::Value>(t["result"].as_str().unwrap_or(""))
                .ok()
                .and_then(|v| v["text"].as_str().map(str::to_owned))
                .is_some_and(|raw| !raw.trim().is_empty() && text.contains(raw.trim()))
        })
    {
        return false;
    }
    let supported = tools
        .iter()
        .rev()
        .find(|t| {
            matches!(
                t["name"].as_str(),
                Some(
                    "read_file"
                        | "write_file"
                        | "edit_file"
                        | "copy_file"
                        | "move_file"
                        | "delete_file"
                )
            )
        })
        .is_some_and(|t| t["status"] == "succeeded");
    if !supported {
        return false;
    }
    let text = text.to_lowercase();
    [
        "не могу прочитать",
        "не могу получить доступ",
        "нет доступа к файл",
        "предоставьте текст файла",
        "предоставьте содержимое файла",
        "создайте файл вручную",
        "создать файл вручную",
        "cannot access the file",
        "can't access the file",
        "cannot read the file",
        "paste the file contents",
        "create the file manually",
        "не соответствует нашим стандартам",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

/// A bounded check for categorical browser claims. Browser actions and page
/// text do not independently verify authentication or an exploited weakness.
/// Describe observed evidence instead; never promote a click to a login proof.
pub fn browser_claim_issue(
    text: &str,
    tools: &[serde_json::Value],
    browser_context: bool,
) -> Option<String> {
    let browser_tools: Vec<_> = tools
        .iter()
        .filter(|t| {
            matches!(
                t["name"].as_str(),
                Some("browser_open" | "browser_read" | "browser_action" | "fetch_url")
            )
        })
        .collect();
    if !browser_context && browser_tools.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let claims = [
        "авторизация выполнена",
        "авторизация успешна",
        "успешно авториз",
        "успешная авторизация выполнена",
        "взлом осуществл",
        "взлом сайта осуществл",
        "сайт взломан",
        "аккаунт взломан",
        "взломал сайт",
        "взломал аккаунт",
        "успешно вош",
        "успешный вход выполнен",
        "доступ к аккаунту получен",
        "обход авторизации выполнен",
        "successfully logged in",
        "successfully authenticated",
        "authentication succeeded",
        "login successful",
        "successfully hacked",
        "account was hacked",
        "site was hacked",
        "bypassed authentication",
    ];
    let unsupported_claim = lower.split(['\n', '.', '!', '?']).any(|sentence| {
        let sentence = sentence.trim();
        !sentence.starts_with('>')
            && ![
                "не подтвержд",
                "не доказ",
                "нет доказательств",
                "нельзя утверждать",
                "not verified",
                "not confirmed",
                "no evidence",
                "cannot claim",
            ]
            .iter()
            .any(|negation| sentence.contains(negation))
            && claims.iter().any(|claim| sentence.contains(claim))
    });
    if !unsupported_claim {
        return None;
    }
    let opened = browser_tools
        .iter()
        .filter(|t| t["status"] == "succeeded" && t["name"] == "browser_open")
        .count();
    let read = browser_tools
        .iter()
        .filter(|t| {
            t["status"] == "succeeded"
                && matches!(t["name"].as_str(), Some("browser_read" | "fetch_url"))
        })
        .count();
    let actions = browser_tools
        .iter()
        .filter(|t| t["status"] == "succeeded" && t["name"] == "browser_action")
        .count();
    Some(format!("Подтверждения входа в аккаунт или взлома нет. За этот запрос выполнено: открытий страницы — {opened}, чтений — {read}, действий в браузере — {actions}. Успешный вызов инструмента не доказывает авторизацию или уязвимость. Нужны конкретные наблюдения и проверка результата; непроведённые проверки нельзя считать успешными."))
}

/// Facts come from tool records, independently of the model's final prose.
pub fn execution_report(tools: &[serde_json::Value]) -> serde_json::Value {
    use serde_json::json;
    let actual: Vec<_> = tools.iter().filter(|t| t["status"].is_string()).collect();
    let succeeded = actual.iter().filter(|t| t["status"] == "succeeded").count();
    let failed = actual.iter().filter(|t| t["status"] == "failed").count();
    let stopped = actual.len() - succeeded - failed;
    let mut files = Vec::new();
    let mut commands = Vec::new();
    let mut reads = Vec::new();
    for t in &actual {
        let result = serde_json::from_str::<serde_json::Value>(t["result"].as_str().unwrap_or(""))
            .unwrap_or(serde_json::Value::Null);
        if t["status"] == "succeeded" && result["verified"] == true && result["changed"] == true {
            if let Some(paths) = result["paths"].as_array() {
                for path in paths {
                    if !files.contains(path) {
                        files.push(path.clone());
                    }
                }
            }
        }
        if t["name"] == "read_file" && t["status"] == "succeeded" {
            reads.push(json!({"path":result["path"],"start_line":result["start_line"],"end_line":result["end_line"],"has_more":result["has_more"]}));
        }
        if t["name"] == "run_command" {
            commands.push(json!({"id":t["id"],"command":t["arguments"]["command"],"status":t["status"],"exit_code":t["exit_code"]}));
        }
    }
    json!({"actions":actual.len(),"succeeded":succeeded,"failed":failed,"stopped":stopped,"verified_changed_files":files,"reads":reads,"commands":commands,"note":"Статусы инструментов; успешный выход команды сам по себе не доказывает прохождение тестов или выполнение всей задачи."})
}

pub fn tool_context(name: &str, status: &str, text: &str, exit_code: Option<i32>) -> String {
    if crate::web_tools::is_web_tool(name) && status == "succeeded" {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(text) {
            v.as_object_mut().map(|o| o.remove("body"));
            if let Some(raw) = v["text"].as_str() {
                if raw.chars().count() > 8000 {
                    v["text"] = serde_json::json!(crate::tools::truncate(raw, 8000));
                    v["context_truncated"] = serde_json::json!(true);
                    v["complete"] = serde_json::json!(false);
                }
            }
            for key in ["elements", "links"] {
                if let Some(a) = v[key].as_array_mut() {
                    if a.len() > 40 {
                        a.truncate(40);
                        v["context_truncated"] = serde_json::json!(true);
                    }
                }
            }
            while v.to_string().len() > 15000 {
                let key = if v["elements"].as_array().is_some_and(|a| !a.is_empty()) {
                    "elements"
                } else {
                    "links"
                };
                if let Some(a) = v[key].as_array_mut().filter(|a| !a.is_empty()) {
                    a.pop();
                    v["context_truncated"] = serde_json::json!(true);
                } else {
                    break;
                }
            }
            v["tool_status"] = serde_json::json!(status);
            return v.to_string();
        }
    }
    if name == "read_document" && status == "succeeded" {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) {
            let original = value["text"].as_str().unwrap_or("");
            let truncated = original.chars().count() > 12000;
            value["text"] = serde_json::json!(crate::tools::truncate(original, 12000));
            value["status"] = serde_json::json!(status);
            if truncated {
                value["context_truncated"] = serde_json::json!(true);
                value["complete"] = serde_json::json!(false);
            }
            return value.to_string();
        }
    }
    if name == "read_file" && status == "succeeded" {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(content) = value["text"].as_str() {
                return format!("STATUS: succeeded\nPATH: {}\nLINES: {}..{}\nHAS_MORE: {}\nNEXT_START_LINE: {}\nThe following is raw file text. Do not copy these metadata labels or markers into edits.\nFILE_TEXT_BEGIN\n{}\nFILE_TEXT_END",value["path"].as_str().unwrap_or("").replace('\\',"/"),value["start_line"],value["end_line"],value["has_more"],value["next_start_line"],content);
            }
        }
    }
    serde_json::json!({"status":status,"result":crate::tools::truncate(text,16000),"exit_code":exit_code}).to_string()
}

pub struct Budget {
    pub used: usize,
    limit: usize,
    attempts: HashMap<String, usize>,
}
impl Budget {
    pub fn new(limit: usize) -> Self {
        Self {
            used: 0,
            limit,
            attempts: HashMap::new(),
        }
    }
    pub fn reserve(&mut self, name: &str, args: &serde_json::Value) -> Result<(), &'static str> {
        if self.limit != 0 && self.used >= self.limit {
            return Err("Достигнут лимит действий. Продолжите новой репликой.");
        }
        let key = format!("{name}:{}", args);
        *self.attempts.entry(key).or_default() += 1;
        self.used += 1;
        Ok(())
    }
    pub fn files_changed(&mut self) {
        self.attempts.clear();
    }
}

pub fn attachment_review_request(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "оцени",
        "оценить",
        "ревью",
        "review",
        "проанализируй",
        "анализ кода",
        "проверь код",
        "check this code",
    ]
    .iter()
    .any(|s| t.contains(s))
        && !t.split(|c: char| !c.is_alphanumeric()).any(|word| {
            ["fix", "modify", "rewrite", "save", "execute", "run"].contains(&word)
                || [
                    "исправ",
                    "измени",
                    "перепиши",
                    "сохрани",
                    "запусти",
                    "выполни",
                ]
                .iter()
                .any(|s| word.starts_with(s))
        })
}

/// Keep a pasted source body separate from the user's review instruction.
/// Identifiers such as save/run inside the source are not requests to act.
pub fn review_instruction(text: &str) -> Option<String> {
    let lines: Vec<_> = text.lines().filter(|s| !s.trim().is_empty()).collect();
    let candidates = if lines.len() > 3 {
        vec![*lines.last()?, *lines.first()?]
    } else {
        vec![text]
    };
    // Mixed instructions may be split across both edges of a pasted source.
    // Source identifiers such as fn fix() are not imperative requests.
    if candidates.iter().any(|line| {
        let line = line.trim().to_lowercase();
        !line.contains(['{', '}', ';'])
            && line.split(|c: char| !c.is_alphanumeric()).any(|word| {
                [
                    "исправь",
                    "измени",
                    "перепиши",
                    "сохрани",
                    "запусти",
                    "выполни",
                    "fix",
                    "modify",
                    "rewrite",
                    "save",
                    "execute",
                ]
                .contains(&word)
            })
    }) {
        return None;
    }
    candidates
        .into_iter()
        .map(str::trim)
        .find(|line| line.chars().count() <= 500 && attachment_review_request(line))
        .map(str::to_string)
}

/// UTF-8 safe pages, preferring a newline near the end of each page.
pub fn review_pages(text: &str, chars: usize) -> Vec<String> {
    let mut rest = text;
    let mut parts = vec![];
    while !rest.is_empty() {
        let end = rest
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let cut = if end < rest.len() {
            rest[..end]
                .rfind('\n')
                .filter(|i| *i > end / 2)
                .map(|i| i + 1)
                .unwrap_or(end)
        } else {
            end
        };
        parts.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    parts
}

/// Keep complete task rounds. Ollama must not silently discard the current task.
pub fn fit_history(history: &mut Vec<serde_json::Value>, limit: usize) -> anyhow::Result<bool> {
    fn cost(history: &[serde_json::Value]) -> usize {
        crate::context::history_tokens(history)
    }
    let mut shortened = false;
    while cost(history) > limit {
        let users: Vec<_> = history
            .iter()
            .enumerate()
            .filter(|(_, m)| m["role"] == "user")
            .map(|(i, _)| i)
            .collect();
        if users.len() > 1 {
            history.drain(1..users[1]);
            shortened = true;
            continue;
        }
        // Reduce old tool payloads, while retaining success/failure and paths.
        if let Some(item) = history.iter_mut().find(|m| {
            m["role"] == "tool"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.chars().count() > 1200)
                && !serde_json::from_str::<serde_json::Value>(m["content"].as_str().unwrap_or(""))
                    .is_ok_and(|v| v["context_truncated"] == true)
        }) {
            let value = item["content"].as_str().unwrap();
            let mut payload = serde_json::from_str::<serde_json::Value>(value)
                .unwrap_or_else(|_| {
                    if value.starts_with("STATUS: succeeded\n") {
                        let (metadata,text)=value.split_once("\nFILE_TEXT_BEGIN\n").unwrap_or(("",value));
                        serde_json::json!({"status":"succeeded","metadata":crate::tools::truncate(metadata,600),"result":text.strip_suffix("\nFILE_TEXT_END").unwrap_or(text)})
                    } else {serde_json::json!({"result":value})}
                });
            for key in ["result", "text", "body", "analysis", "stdout", "stderr"] {
                if let Some(raw) = payload[key].as_str() {
                    payload[key] = serde_json::json!(crate::tools::truncate(raw, 500));
                }
            }
            for key in ["elements", "links", "results"] {
                if let Some(a) = payload[key].as_array_mut() {
                    a.truncate(2);
                    for v in a.iter_mut() {
                        for k in ["snippet", "name", "title"] {
                            if let Some(raw) = v[k].as_str() {
                                v[k] = serde_json::json!(crate::tools::truncate(raw, 100));
                            }
                        }
                    }
                }
            }
            payload["context_truncated"] = serde_json::json!(true);
            payload["complete"] = serde_json::json!(false);
            let mut reduced = payload.to_string();
            if reduced == value || reduced.len() > 2400 {
                let mut compact = serde_json::Map::new();
                for key in [
                    "status",
                    "tool_status",
                    "path",
                    "metadata",
                    "start_line",
                    "end_line",
                    "has_more",
                    "next_start_line",
                    "url",
                    "title",
                    "text",
                    "result",
                    "analysis",
                    "stdout",
                    "stderr",
                    "coverage",
                    "live_testing",
                    "next_offset",
                ] {
                    if !payload[key].is_null() {
                        let v = if let Some(raw) = payload[key].as_str() {
                            serde_json::json!(crate::tools::truncate(raw, 200))
                        } else {
                            payload[key].clone()
                        };
                        compact.insert(key.into(), v);
                    }
                }
                compact.insert("context_truncated".into(), serde_json::json!(true));
                compact.insert("complete".into(), serde_json::json!(false));
                reduced = serde_json::Value::Object(compact).to_string();
            }
            anyhow::ensure!(
                reduced != value,
                "Current task exceeds model context; use a smaller page range."
            );
            item["content"] = serde_json::json!(reduced);
            shortened = true;
            continue;
        }
        anyhow::bail!("Обязательный текущий контекст не помещается в выбранное окно ({limit} токенов по оценке); текущий запрос не сокращён.");
    }
    Ok(shortened)
}

#[derive(Debug)]
pub struct ApprovalStopped(pub &'static str);
impl fmt::Display for ApprovalStopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            "expired" => "Время подтверждения истекло. Действие не выполнялось.",
            "cancelled" => "Подтверждение отменено. Действие не выполнялось.",
            _ => "Пользователь отклонил действие. Оно не выполнялось.",
        })
    }
}
impl std::error::Error for ApprovalStopped {}

#[derive(Debug)]
pub struct CommandFailed {
    pub code: i32,
    pub output: String,
}
impl fmt::Display for CommandFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Команда завершилась с кодом {}.\n{}",
            self.code, self.output
        )
    }
}
impl std::error::Error for CommandFailed {}

#[derive(Debug)]
pub struct ProcessInterrupted(pub &'static str);
impl fmt::Display for ProcessInterrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0 == "cancelled" {
            "Остановлено пользователем"
        } else {
            "Превышено время выполнения команды"
        })
    }
}
impl std::error::Error for ProcessInterrupted {}
#[derive(Debug)]
pub struct ExecutionStopped {
    pub report: String,
}
impl fmt::Display for ExecutionStopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.report)
    }
}
impl std::error::Error for ExecutionStopped {}

pub fn plan(args: &serde_json::Value) -> anyhow::Result<String> {
    let steps = args["steps"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Укажите steps"))?;
    anyhow::ensure!(
        !steps.is_empty(),
        "План не должен быть пустым"
    );
    let mut active = 0;
    for step in steps {
        let title = step["title"].as_str().unwrap_or("");
        anyhow::ensure!(
            !title.trim().is_empty() && title.chars().count() <= 200,
            "Название шага: 1–200 символов"
        );
        let status = step["status"].as_str().unwrap_or("");
        anyhow::ensure!(
            ["pending", "in_progress", "completed"].contains(&status),
            "Неверный статус шага"
        );
        if status == "in_progress" {
            active += 1;
        }
    }
    anyhow::ensure!(
        active <= 1,
        "Одновременно выполняется только один шаг плана"
    );
    Ok(serde_json::json!({"plan":steps,"note":"План и его отметки составлены моделью. Выполнение команд и проверки подтверждаются отдельными карточками инструментов."}).to_string())
}

// A deliberately narrow no-tools path for unmistakably conversational turns.
// Other requests still use the agent and per-action confirmation.
pub fn reply_only(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let has_new_task = [
        "создай",
        "сохрани",
        "запусти",
        "выполни",
        "прочитай",
        "найди",
        "удали",
        "измени",
        "исправь",
        "проверь",
        "открой",
        "create",
        "write",
        "save",
        "read",
        "find",
        "delete",
        "edit",
        "fix",
        "run",
        "execute",
        "open",
    ]
    .iter()
    .any(|word| {
        t.split(|c: char| !c.is_alphanumeric())
            .any(|token| token == *word)
    });
    if has_new_task {
        return false;
    }
    if (t.contains("в чат") || t.contains("in chat"))
        && ["код", "программ", "code", "program"]
            .iter()
            .any(|w| t.contains(w))
    {
        return true;
    }
    if t.len() > 1000 || t.contains('\n') {
        return false;
    }
    if [
        "привет",
        "здравствуй",
        "здравствуйте",
        "hello",
        "hi",
        "nice",
        "найс",
        "спасибо",
        "thanks",
        "thank you",
        "ок",
        "окей",
        "okay",
        "ok",
        "круто",
        "awesome",
        "great",
        "cool",
        "понял",
        "хорошо",
    ]
    .contains(&t.trim_end_matches(['!', '.', '?']))
    {
        return true;
    }
    [
        "ты кто",
        "кто ты",
        "ты знаешь",
        "ты будешь",
        "именно ты будешь",
        "что означает",
        "что значит",
        "что такое",
        "who are you",
        "do you know",
        "will you",
        "why are you responding",
        "why do you reply",
        "why do you respond",
        "почему ты отвечаешь",
        "почему отвечаешь",
    ]
    .iter()
    .any(|prefix| t.starts_with(prefix))
}

pub fn reply_language(text: &str) -> Option<String> {
    let t = text.to_lowercase();
    let explicit = ["ответь ", "отвечай ", "please answer ", "please reply "]
        .iter()
        .any(|s| t.contains(s))
        || ["answer in ", "reply in ", "respond in ", "speak "]
            .iter()
            .any(|s| t.trim().starts_with(s));
    if explicit {
        for (terms, code) in [
            (["english", "англий"], "en"),
            (["russian", "русск"], "ru"),
            (["french", "француз"], "fr"),
            (["german", "немец"], "de"),
            (["spanish", "испан"], "es"),
        ] {
            if terms.iter().any(|w| t.contains(w)) {
                return Some(code.into());
            }
        }
        return None;
    }
    Some(crate::system_translate::source_language(text, "auto"))
}
pub fn wrong_reply_language(text: &str, expected: &str) -> bool {
    if !["en", "ru"].contains(&expected) {
        return false;
    }
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 4 && crate::system_translate::source_language(text, "auto") != expected
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attached_reviews_are_read_only_and_pages_preserve_all_text() {
        assert!(attachment_review_request("оцени код"));
        assert!(attachment_review_request("Review attached files.rs"));
        assert!(attachment_review_request(
            "REVIEW_ATTACHMENT_FIXTURE оцени код"
        ));
        assert!(!attachment_review_request("оцени код и исправь ошибки"));
        assert!(!attachment_review_request("review this and run tests"));
        assert_eq!(
            review_instruction("fn run() {}\nfn save() {}\nfn fix() {}\nоцени код").as_deref(),
            Some("оцени код")
        );
        assert_eq!(
            review_instruction("оцени код\n```rust\nfn run() {}\n```").as_deref(),
            Some("оцени код")
        );
        assert!(review_instruction(
            "fn run() {}\nfn save() {}\nfn fix() {}\nоцени код и исправь ошибки"
        )
        .is_none());
        assert!(
            review_instruction("оцени код\nfn main() {}\n// body\nисправь найденные ошибки")
                .is_none()
        );
        assert!(
            review_instruction("исправь найденные ошибки\nfn main() {}\n// body\nоцени код")
                .is_none()
        );
        let text = "fn main() {}\n// привет 🦀\n".repeat(2000);
        let pages = review_pages(&text, 5000);
        assert_eq!(pages.concat(), text);
        assert!(pages.iter().all(|p| p.chars().count() <= 5000));
        assert_eq!(
            review_pages(&"я".repeat(15000), 5000).concat(),
            "я".repeat(15000)
        );
    }

    #[test]
    fn browser_success_cannot_be_invented_from_open_or_click() {
        use serde_json::json;
        let opened =
            json!({"name":"browser_open","status":"succeeded","result":"{\"text\":\"Sign in\"}"});
        let fake_success = "Итог: Сайт успешно открыт и авторизация выполнена.";
        let issue = browser_claim_issue(fake_success, &[opened.clone()], true).unwrap();
        assert!(issue.contains("открытий страницы — 1"));
        assert!(issue.contains("действий в браузере — 0"));
        let click = json!({"name":"browser_action","status":"succeeded","arguments":{"action":"click"},"result":"{}"});
        assert!(browser_claim_issue(fake_success, &[opened, click], true).is_some());
        assert!(browser_claim_issue("Взлом сайта осуществлён.", &[], true).is_some());
        assert!(browser_claim_issue("Successfully logged in.", &[], true).is_some());
        for text in [
            "Вход не проверен. Страница открыта.",
            "Нет доказательств, что авторизация выполнена.",
            "The page shows a profile; authentication is not independently verified.",
            "План: проверить вход.",
        ] {
            assert!(browser_claim_issue(text, &[], true).is_none(), "{text}");
        }
        assert!(
            browser_claim_issue("В рассказе персонаж успешно вошёл в комнату.", &[], false)
                .is_none()
        );
    }

    #[test]
    fn evidence_correction_is_bounded_to_real_file_results() {
        let read = serde_json::json!({"name":"read_file","status":"succeeded","result":serde_json::json!({"text":"actual data"}).to_string()});
        assert!(needs_grounding("Я не могу прочитать файл", &[read.clone()]));
        assert!(!needs_grounding("Я не могу прочитать файл", &[]));
        let failed = serde_json::json!({"name":"read_file","status":"failed","result":"missing"});
        assert!(!needs_grounding("Я не могу прочитать файл", &[failed]));
        let quote = serde_json::json!({"name":"read_file","status":"succeeded","result":serde_json::json!({"text":"Я не могу прочитать файл"}).to_string()});
        assert!(!needs_grounding(
            "В файле написано: Я не могу прочитать файл",
            &[quote]
        ));
        assert!(!needs_grounding("Прочитано: actual data", &[read]));
    }
    #[test]
    fn acknowledgments_and_language_questions_do_not_receive_tools() {
        for text in [
            "nice",
            "найс!",
            "спасибо",
            "Why are you responding in Russian?",
            "почему ты отвечаешь на русском?",
        ] {
            assert!(reply_only(text), "{text}");
        }
        for text in [
            "nice, create file a.txt",
            "найс, прочитай makar.txt",
            "thanks, run the tests",
            "Объясни ошибку и исправь её",
            "Explain the bug and fix it",
        ] {
            assert!(!reply_only(text), "{text}");
        }
    }
    #[test]
    fn reply_language_follows_current_message_and_explicit_request() {
        assert_eq!(reply_language("nice").as_deref(), Some("en"));
        assert_eq!(
            reply_language("Why are you responding in Russian?").as_deref(),
            Some("en")
        );
        assert_eq!(
            reply_language("что это такое? Ответь на английском").as_deref(),
            Some("en")
        );
        assert!(wrong_reply_language(
            "Хорошо, спасибо за приветствие!",
            "en"
        ));
        assert!(!wrong_reply_language("Glad that helped!", "en"));
        assert!(!wrong_reply_language("Merci!", "fr"));
    }

    #[test]
    fn execution_counts_are_derived_from_verified_tool_records() {
        let tools = serde_json::json!([
            {"name":"write_file","status":"succeeded","result":"{\"verified\":true,\"changed\":true,\"paths\":[\"A:/one.txt\"]}"},
            {"name":"edit_file","status":"succeeded","result":"{\"verified\":true,\"changed\":true,\"paths\":[\"A:/one.txt\"]}"},
            {"name":"write_file","status":"denied","result":"no"},
            {"name":"run_command","status":"failed","exit_code":1,"arguments":{"command":"test"}},
            {"name":"OCR","result":"text"}
        ]);
        let report = execution_report(tools.as_array().unwrap());
        assert_eq!(report["actions"], 4);
        assert_eq!(report["succeeded"], 2);
        assert_eq!(report["failed"], 1);
        assert_eq!(report["stopped"], 1);
        assert_eq!(
            report["verified_changed_files"].as_array().unwrap().len(),
            1
        );
        assert_eq!(report["commands"][0]["exit_code"], 1);
        let mut config = AssistantConfig::default();
        assert!(config.validate().is_ok());
        config.agent_temperature = f64::NAN;
        assert!(config.validate().is_err());
    }

    #[test]
    fn compressed_file_context_keeps_status_and_pagination() {
        let raw=tool_context("read_file","succeeded",&serde_json::json!({"path":"A:/sample.txt","text":"example line\n".repeat(2000),"start_line":1,"end_line":2000,"has_more":true,"next_start_line":2001}).to_string(),None);
        let mut history = vec![
            serde_json::json!({"role":"system","content":"instructions"}),
            serde_json::json!({"role":"user","content":"read file"}),
            serde_json::json!({"role":"tool","content":raw}),
        ];
        assert!(fit_history(&mut history, 1500).unwrap());
        let compact: serde_json::Value =
            serde_json::from_str(history[2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(compact["status"], "succeeded");
        assert_eq!(compact["context_truncated"], true);
        assert!(compact["metadata"]
            .as_str()
            .unwrap()
            .contains("NEXT_START_LINE: 2001"));
    }

    #[test]
    fn document_context_reports_its_own_truncation() {
        let text=serde_json::json!({"text":"long document ".repeat(1500),"complete":true,"format":"pdf","start_page":2,"end_page":3}).to_string();
        let context: serde_json::Value =
            serde_json::from_str(&tool_context("read_document", "succeeded", &text, None)).unwrap();
        assert_eq!(context["status"], "succeeded");
        assert_eq!(context["complete"], false);
        assert_eq!(context["context_truncated"], true);
        assert_eq!(context["start_page"], 2);
    }
    #[test]
    fn file_context_is_raw_source_not_json_or_line_prefixes() {
        let source = "def add(a, b):\r\n    return a - b\r\n";
        let result=serde_json::json!({"path":"A:/fixture/calc.py","start_line":1,"end_line":2,"has_more":false,"next_start_line":null,"text":source}).to_string();
        let context = tool_context("read_file", "succeeded", &result, None);
        assert!(context.contains(source));
        assert!(!context.contains("1: def"));
        assert!(!context.contains("\\r\\n"));
        let error = tool_context("read_file", "failed", "missing", None);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&error).unwrap()["status"],
            "failed"
        );
    }
    #[test]
    fn plans_require_valid_steps_and_one_active_step() {
        use serde_json::json;
        assert!(plan(&json!({"steps":[]})).is_err());
        assert!(plan(&json!({"steps":[{"title":"Read","status":"in_progress"},{"title":"Edit","status":"in_progress"}]})).is_err());
        assert!(plan(&json!({"steps":[{"title":"Read","status":"completed"},{"title":"Edit","status":"in_progress"},{"title":"Check","status":"pending"}]})).is_ok());
    }
    #[test]
    fn only_explicit_action_quota_stops_execution() {
        let mut b = Budget::new(3);
        let args = serde_json::json!({"path":"file"});
        assert!(b.reserve("read_file", &args).is_ok());
        assert!(b.reserve("read_file", &args).is_ok());
        assert!(b.reserve("read_file", &args).is_ok());
        b.files_changed();
        assert!(b.reserve("read_file", &args).is_err());
        assert!(b.reserve("other", &args).is_err());
        let mut unlimited = Budget::new(0);
        for _ in 0..100 {
            assert!(unlimited.reserve("read_file", &args).is_ok());
        }
    }
    #[test]
    fn context_keeps_current_task_and_whole_rounds() {
        use serde_json::json;
        let mut history = vec![
            json!({"role":"system","content":"rules"}),
            json!({"role":"user","content":"old".repeat(8000)}),
            json!({"role":"assistant","tool_calls":[{}]}),
            json!({"role":"tool","content":"old result"}),
            json!({"role":"user","content":"current task"}),
        ];
        assert!(fit_history(&mut history, 5500).unwrap());
        assert_eq!(history.len(), 2);
        assert_eq!(history[1]["content"], "current task");
        history[1]["content"] = json!("large".repeat(12000));
        assert!(fit_history(&mut history, 5500).is_err());
    }
    #[test]
    fn conversation_does_not_inherit_old_execution_intent() {
        for s in [
            "ты кто?",
            "Ты будешь это делать? Ответь на вопрос",
            "ты знаешь, что значит это слово?",
            "Напиши программу на Python и отправь код в чат",
        ] {
            assert!(reply_only(s), "{s}");
        }
        for s in [
            "Запусти скрипт",
            "Создай файл example.py",
            "Ты знаешь этот проект? Исправь ошибку",
            "Прочитай файл",
        ] {
            assert!(!reply_only(s), "{s}");
        }
    }
    #[test]
    fn large_web_context_keeps_evidence_and_can_shrink() {
        use serde_json::json;
        let page = json!({"tool_status":"succeeded","url":"https://example.org/actual","title":"Actual page","text":"page data ".repeat(4000),"elements":[{"ref":"r123","name":"button"}],"links":[{"url":"https://example.org/next","title":"next"}]});
        let mut history = vec![
            json!({"role":"system","content":"Use real sources"}),
            json!({"role":"user","content":"Read the page"}),
            json!({"role":"tool","content":page.to_string()}),
        ];
        assert!(super::fit_history(&mut history, 1000).unwrap());
        let p: serde_json::Value =
            serde_json::from_str(history[2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(p["url"], "https://example.org/actual");
        assert_eq!(p["tool_status"], "succeeded");
        assert_eq!(p["complete"], false);
        assert_eq!(p["context_truncated"], true);
    }
}
