use crate::{api::Shared, engine, tools};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Temporary(Vec<PathBuf>);
impl Drop for Temporary {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}
pub async fn worker(app: &Shared, mut request: Value, cancel: &CancellationToken) -> Result<Value> {
    let python = tools::python(app).context("Для документов установите компоненты приложения")?;
    let path = app
        .data
        .join("temp")
        .join(format!("{}.document.json", uuid::Uuid::new_v4()));
    let output_path = path.with_extension("result.json");
    let _cleanup = Temporary(vec![path.clone(), output_path.clone()]);
    request["_result_path"] = json!(output_path);
    tokio::fs::write(&path, serde_json::to_vec(&request)?).await?;
    let mut command = tokio::process::Command::new(python);
    command
        .arg(app.root.join("runtime/document_worker.py"))
        .arg(path)
        .env("PYTHONUTF8", "1");
    let (code, out, err) = tools::capture(command, cancel, 600).await?;
    let mut value: Value = serde_json::from_str(out.trim())
        .with_context(|| format!("Обработка документа: {}", tools::truncate(&err, 1000)))?;
    if value["result_file"] == true {
        anyhow::ensure!(
            tokio::fs::metadata(&output_path).await?.len() <= 1_000_000,
            "Результат документа слишком большой"
        );
        value = serde_json::from_slice(&tokio::fs::read(&output_path).await?)?;
    }
    if let Some(error) = value["error"].as_str() {
        anyhow::bail!("{error}");
    }
    anyhow::ensure!(
        code == 0,
        "Обработчик документа завершился с ошибкой: {}",
        tools::truncate(&err, 1000)
    );
    Ok(value)
}
fn request(path: &Path, options: &Value, operation: &str) -> Value {
    json!({"path":path,"operation":operation,"paragraph_mode":true,"start_page":options["start_page"].as_u64().unwrap_or(1),"max_pages":options["max_pages"].as_u64().unwrap_or(5),"ocr":options["ocr"].as_bool().unwrap_or(false),"language":options["language"].as_str().unwrap_or("en-US")})
}
pub async fn extract(
    app: &Shared,
    path: &Path,
    options: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let mut value = worker(app, request(path, options, "extract"), cancel).await?;
    value["path"] = json!(path);
    Ok(value)
}
fn edges(original: &str, translated: &str) -> String {
    let first = original.len() - original.trim_start().len();
    let last = original.trim_end().len();
    format!(
        "{}{}{}",
        &original[..first],
        translated.trim(),
        &original[last..]
    )
}
pub async fn translate(
    app: &Shared,
    path: &Path,
    options: &Value,
    tx: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
) -> Result<(Vec<u8>, String, Value)> {
    engine::ensure_ollama(app).await?;
    let plan = worker(app, request(path, options, "plan"), cancel).await?;
    let paragraph_mode = plan["translation_mode"] == "paragraph-v2";
    let selected = app.settings.lock().await.translation_model.clone();
    let units = plan["units"]
        .as_array()
        .context("Нет фрагментов документа")?;
    let sample = units
        .iter()
        .filter_map(|u| u["text"].as_str())
        .take(10)
        .collect::<Vec<_>>()
        .join("\n");
    let source = crate::system_translate::source_language(
        &sample,
        options["source"].as_str().unwrap_or("auto"),
    );
    let target = options["target"].as_str().unwrap_or("ru");
    anyhow::ensure!(
        !units.is_empty() && !sample.trim().is_empty(),
        "Текст документа не найден. Для сканированного PDF включите OCR."
    );
    let mut translations = serde_json::Map::new();
    let mut markdown = String::new();
    let mut count = 0;
    let mut translated_count = 0;
    let mut inference_calls = 0;
    let mut format_retries = 0;
    for (index, unit) in units.iter().enumerate() {
        anyhow::ensure!(!cancel.is_cancelled(), "Перевод документа остановлен");
        engine::emit(tx,json!({"type":"progress","completed":index,"total":units.len(),"message":format!("Перевод {} {}/{}",if paragraph_mode {"абзаца"} else {"фрагмента"},index+1,units.len())})).await?;
        let text = unit["text"].as_str().unwrap_or("");
        let input = if paragraph_mode {
            unit["input"].as_str().context("Нет текста абзаца")?
        } else {
            text
        };
        let unchanged = source == target || !text.chars().any(char::is_alphabetic);
        let mut result = if text.trim().is_empty() {
            String::new()
        } else if unchanged {
            input.to_owned()
        } else if paragraph_mode {
            inference_calls += 1;
            engine::translate_document_paragraph(
                app,
                input,
                &source,
                target,
                &selected,
                unit["marked"] == true,
                false,
                cancel,
            )
            .await?
        } else {
            inference_calls += engine::chunks(text, 1800).len();
            engine::translate_tool_with_model(app, text, &source, target, &selected, cancel).await?
        };
        if paragraph_mode && !unchanged && unit["marked"] == true {
            let check = worker(
                app,
                json!({"operation":"validate_paragraph","unit":unit,"translation":result}),
                cancel,
            )
            .await?;
            if check["valid"] != true {
                engine::emit(tx,json!({"type":"progress","completed":index,"total":units.len(),"message":format!("Проверка форматирования абзаца {}: повторный перевод",index+1)})).await?;
                anyhow::ensure!(!cancel.is_cancelled(), "Перевод документа остановлен");
                inference_calls += 1;
                format_retries += 1;
                result = engine::translate_document_paragraph(
                    app, input, &source, target, &selected, true, true, cancel,
                )
                .await?;
                let check = worker(
                    app,
                    json!({"operation":"validate_paragraph","unit":unit,"translation":result}),
                    cancel,
                )
                .await?;
                anyhow::ensure!(check["valid"] == true, "Модель не сохранила форматирование абзаца {}: {}. Документ не создан. Попробуйте другую переводческую модель.", index+1, check["validation_error"].as_str().unwrap_or("неверные метки"));
            }
        }
        if !unchanged && !text.trim().is_empty() {
            translated_count += 1;
        }
        if paragraph_mode && !unchanged && unit["marked"] != true {
            result = edges(text, &result);
        }
        anyhow::ensure!(
            text.trim().is_empty() || !result.trim().is_empty(),
            "Пустой перевод фрагмента {}",
            index + 1
        );
        if !text.trim().is_empty() {
            count += 1;
        }
        if plan["format"] == "docx" {
            translations.insert(
                unit["id"].as_str().context("Нет ID фрагмента")?.to_owned(),
                if paragraph_mode {
                    json!({"text":result,"unchanged":unchanged})
                } else {
                    json!(edges(text, &result))
                },
            );
        } else {
            markdown.push_str(&format!("## Страница {}\n\n{}\n\n", unit["page"], result));
        }
    }
    let mut report = plan.clone();
    report.as_object_mut().unwrap().remove("units");
    report["processed_units"] = json!(count);
    report["translated_units"] = json!(translated_count);
    report["inference_calls"] = json!(inference_calls);
    report["format_retries"] = json!(format_retries);
    report["model"] = json!(selected);
    report["unit_kind"] = json!(if paragraph_mode { "paragraph" } else { "page" });
    report["source"] = json!(source);
    report["target"] = json!(target);
    let (bytes, extension) = if plan["format"] == "docx" {
        let output = app
            .data
            .join("temp")
            .join(format!("{}.translated.docx", uuid::Uuid::new_v4()));
        let _cleanup = Temporary(vec![output.clone()]);
        worker(app,json!({"operation":"export","path":path,"output":output,"translations":translations,"source_sha256":plan["source_sha256"],"translation_mode":plan["translation_mode"]}),cancel).await?;
        (tokio::fs::read(output).await?, "docx")
    } else {
        (markdown.into_bytes(), "md")
    };
    anyhow::ensure!(!cancel.is_cancelled(), "Перевод документа остановлен");
    engine::emit(tx,json!({"type":"progress","completed":units.len(),"total":units.len(),"message":"Перевод документа завершён"})).await?;
    Ok((bytes, extension.into(), report))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn run_edges_are_preserved() {
        assert_eq!(edges(" Hello ", "Привет\n"), " Привет ");
        assert_eq!(edges("\tword\n", "слово"), "\tслово\n");
    }
}
