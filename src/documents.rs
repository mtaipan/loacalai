use crate::{api::Shared, engine, tools};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio::io::AsyncBufReadExt;
use sha2::{Digest,Sha256};
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
    let (code, out, err) = tools::capture(command, cancel, 0).await?;
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
        if value["code"]=="source_changed"{return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists,error.to_owned()).into());}
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
    json!({"path":path,"operation":operation,"paragraph_mode":true,"text_offset":options["text_offset"].as_u64().unwrap_or(0),"max_characters":options["max_characters"].as_u64().unwrap_or(20000),"source_sha256":options["source_sha256"],"start_page":options["start_page"].as_u64().unwrap_or(1),"max_pages":options["max_pages"].as_u64().unwrap_or(5),"ocr":options["ocr"].as_bool().unwrap_or(false),"language":options["language"].as_str().unwrap_or("en-US")})
}
fn validate_options(options:&Value)->Result<()>{
    anyhow::ensure!(options.is_object(),"Параметры документа должны быть объектом");
    for key in ["text_offset","max_characters","start_page","max_pages"]{anyhow::ensure!(options[key].is_null()||options[key].as_u64().is_some(),"Неверное числовое поле документа: {key}");}
    anyhow::ensure!(options["start_page"].is_null()||options["start_page"].as_u64().is_some_and(|v|v>=1),"Начальная страница от1");
    anyhow::ensure!(options["max_pages"].is_null()||options["max_pages"].as_u64().is_some_and(|v|(1..=5).contains(&v)),"Размер пакета PDF от1 до5 страниц");
    anyhow::ensure!(options["max_characters"].is_null()||options["max_characters"].as_u64().is_some_and(|v|(1..=20000).contains(&v)),"Размер текстового пакета от1 до20000 символов");
    anyhow::ensure!(options["ocr"].is_null()||options["ocr"].is_boolean(),"OCR должен быть логическим значением");
    for key in ["source","target","language"]{anyhow::ensure!(options[key].is_null()||options[key].as_str().is_some_and(|v|!v.trim().is_empty()),"Неверное поле языка: {key}");}
    anyhow::ensure!(options["source_sha256"].is_null()||options["source_sha256"].as_str().is_some_and(|v|v.len()==64&&v.bytes().all(|b|b.is_ascii_hexdigit())),"Неверная версия исходного документа");
    anyhow::ensure!(options["text_offset"].as_u64().unwrap_or(0)==0||options["source_sha256"].is_string(),"Для продолжения текста передайте source_sha256 предыдущего диапазона");Ok(())
}
pub async fn extract(
    app: &Shared,
    path: &Path,
    options: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    validate_options(options)?;
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
    let config=crate::generation::capture(app).await;
    crate::generation::scope(config,translate_inner(app,path,options,tx,cancel)).await
}
async fn translate_inner(
    app: &Shared, path: &Path, options: &Value, tx: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
) -> Result<(Vec<u8>, String, Value)> {
    validate_options(options)?;
    engine::ensure_ollama(app).await?;
    let spool=app.data.join("temp").join(format!("{}.document-units.jsonl",uuid::Uuid::new_v4()));
    let _cleanup_units=Temporary(vec![spool.clone()]);
    let mut preparation=request(path,options,"prepare");preparation["_plan_path"]=json!(spool);
    let plan=worker(app,preparation,cancel).await?;
    let verify=spool.clone();let expected=plan["plan_sha256"].as_str().context("Нет контрольной суммы плана")?.to_string();let token=cancel.clone();
    tokio::task::spawn_blocking(move||->Result<()>{
        use std::io::Read;let mut file=std::fs::File::open(verify)?;let mut buffer=[0u8;65536];let mut digest=Sha256::new();
        loop{anyhow::ensure!(!token.is_cancelled(),"Подготовка перевода остановлена");let n=file.read(&mut buffer)?;if n==0{break;}digest.update(&buffer[..n]);}
        anyhow::ensure!(format!("{:x}",digest.finalize())==expected,"Контрольная сумма плана документа не совпадает");Ok(())
    }).await??;
    let paragraph_mode = plan["translation_mode"] == "paragraph-v2";
    let selected = crate::generation::capture(app).await.translation_model.clone();
    let total=usize::try_from(plan["units_count"].as_u64().context("Нет числа фрагментов документа")?)?;
    let sample=plan["sample_text"].as_str().unwrap_or("");
    let source = crate::system_translate::source_language(
        &sample,
        options["source"].as_str().unwrap_or("auto"),
    );
    let target = options["target"].as_str().unwrap_or("ru");
    anyhow::ensure!(
        total>0 && !sample.trim().is_empty(),
        "Текст документа не найден. Для сканированного PDF включите OCR."
    );
    let owner=if let Ok(run)=crate::permissions::TASK.try_with(Clone::clone){
        if let Some((task,_))=app.tasks.permission_task(run.clone()).await?{format!("task:{task}")}else{
            let stored=app.tasks.get(run.clone()).await?;stored["conversation"].as_str().filter(|s|!s.is_empty()).map(|id|format!("conversation:{id}")).unwrap_or_else(||format!("run:{run}"))
        }
    }else{"document-ui".into()};
    let tags=crate::model_storage::tags(app).await;
    let digest=tags["models"].as_array().into_iter().flatten().find(|row|row["name"]==selected||row["model"]==selected||row["name"].as_str().is_some_and(|name|name==format!("{selected}:latest"))).and_then(|row|row["digest"].as_str()).filter(|s|!s.is_empty()).map(str::to_owned);
    static SESSION:std::sync::OnceLock<String>=std::sync::OnceLock::new();
    let identity=digest.clone().unwrap_or_else(||SESSION.get_or_init(||uuid::Uuid::new_v4().to_string()).clone());
    let settings=crate::generation::capture(app).await;
    let definition=json!({"version":2,"path":path,"source_sha256":plan["source_sha256"],"plan_sha256":plan["plan_sha256"],"source":source,"target":target,"model":selected,"model_identity":identity,"model_identity_verified":digest.is_some(),"endpoint_scope":if engine::owns_engine(){"managed-engine".to_owned()}else{engine::ollama_url()},"configuration":crate::document_checkpoints::hash(&json!({"assistant":settings.assistant,"context":settings.context}))?});
    let checkpoint=app.tasks.document_begin(owner,definition,total).await?;
    let checkpoint_id=checkpoint["id"].as_str().context("Нет ID сохранённого перевода")?.to_string();
    engine::emit(tx,json!({"type":"document_checkpoint","checkpoint":checkpoint,"model_identity_verified":digest.is_some()})).await?;
    let mut resumed_units=0;
    let mut markdown = String::new();
    let mut count = 0;
    let mut translated_count = 0;
    let mut inference_calls = 0;
    let mut format_retries = 0;
    let mut reader=tokio::io::BufReader::with_capacity(64*1024,tokio::fs::File::open(&spool).await?);
    let mut spool_digest=Sha256::new();
    for index in 0..total {
        let mut line=String::new();
        let bytes=tokio::select!{_=cancel.cancelled()=>anyhow::bail!("Перевод документа остановлен"),value=reader.read_line(&mut line)=>value?};
        anyhow::ensure!(bytes>0,"Сохранённый план документа оборван");
        spool_digest.update(line.as_bytes());
        let unit:Value=serde_json::from_str(&line)?;
        anyhow::ensure!(!cancel.is_cancelled(), "Перевод документа остановлен");
        engine::emit(tx,json!({"type":"progress","completed":index,"total":total,"message":format!("Перевод {} {}/{}",if paragraph_mode {"абзаца"} else {"фрагмента"},index+1,total)})).await?;
        let text = unit["text"].as_str().unwrap_or("");
        let input = if paragraph_mode {
            unit["input"].as_str().context("Нет текста абзаца")?
        } else {
            text
        };
        let unchanged = source == target || !text.chars().any(char::is_alphabetic);
        let cached=app.tasks.document_part(checkpoint_id.clone(),unit.clone(),None).await?;
        let result=if let Some(cached)=cached{
            resumed_units+=1;cached["text"].as_str().context("Повреждён сохранённый перевод")?.to_owned()
        }else{
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
                json!({"operation":"validate_paragraph","unit":&unit,"translation":result}),
                cancel,
            )
            .await?;
            if check["valid"] != true {
                engine::emit(tx,json!({"type":"progress","completed":index,"total":total,"message":format!("Проверка форматирования абзаца {}: повторный перевод",index+1)})).await?;
                anyhow::ensure!(!cancel.is_cancelled(), "Перевод документа остановлен");
                inference_calls += 1;
                format_retries += 1;
                result = engine::translate_document_paragraph(
                    app, input, &source, target, &selected, true, true, cancel,
                )
                .await?;
                let check = worker(
                    app,
                    json!({"operation":"validate_paragraph","unit":&unit,"translation":result}),
                    cancel,
                )
                .await?;
                anyhow::ensure!(check["valid"] == true, "Модель не сохранила форматирование абзаца {}: {}. Документ не создан. Попробуйте другую переводческую модель.", index+1, check["validation_error"].as_str().unwrap_or("неверные метки"));
            }
        }
        if paragraph_mode && !unchanged && unit["marked"] != true {
            result = edges(text, &result);
        }
        anyhow::ensure!(
            text.trim().is_empty() || !result.trim().is_empty(),
            "Пустой перевод фрагмента {}",
            index + 1
        );
        app.tasks.document_part(checkpoint_id.clone(),unit.clone(),Some(json!({"text":result,"unchanged":unchanged}))).await?.context("Не удалось сохранить фрагмент")?["text"].as_str().context("Повреждён сохранённый перевод")?.to_owned()
        };
        anyhow::ensure!(text.trim().is_empty()||!result.trim().is_empty(),"Пустой сохранённый перевод фрагмента {}",index+1);
        engine::emit(tx,json!({"type":"document_checkpoint","checkpoint":{"id":checkpoint_id,"saved_units":index+1,"total_units":total}})).await?;
        if !unchanged&&!text.trim().is_empty(){translated_count+=1;}
        if !text.trim().is_empty() {count += 1;}
        if plan["format"]!="docx"{markdown.push_str(&format!("## Страница {}\n\n{}\n\n",unit["page"],result));}
    }
    let mut extra=String::new();anyhow::ensure!(reader.read_line(&mut extra).await?==0,"В плане появились лишние фрагменты");drop(reader);
    anyhow::ensure!(format!("{:x}",spool_digest.finalize())==plan["plan_sha256"].as_str().unwrap_or(""),"План документа изменился во время перевода; результат не экспортирован");

    let mut report = plan.clone();
    report.as_object_mut().unwrap().remove("sample_text");
    report["processed_units"] = json!(count);
    report["translated_units"] = json!(translated_count);
    report["inference_calls"] = json!(inference_calls);
    report["format_retries"] = json!(format_retries);
    report["checkpoint"]=json!({"id":checkpoint_id,"reused_units":resumed_units,"model_identity_verified":digest.is_some(),"resume_scope":if digest.is_some(){"same_model_digest"}else{"current_app_process"}});
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
        worker(app,json!({"operation":"export","path":path,"output":output,"checkpoint_database":app.data.join("tasks.sqlite3"),"checkpoint":checkpoint_id,"source_sha256":plan["source_sha256"],"translation_mode":plan["translation_mode"]}),cancel).await?;
        (tokio::fs::read(output).await?, "docx")
    } else {
        (markdown.into_bytes(), "md")
    };
    anyhow::ensure!(!cancel.is_cancelled(), "Перевод документа остановлен");
    engine::emit(tx,json!({"type":"progress","completed":total,"total":total,"message":"Перевод документа завершён"})).await?;
    Ok((bytes, extension.into(), report))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_document_parameters_never_silently_select_defaults(){
        for bad in [json!([]),json!({"start_page":0}),json!({"max_pages":6}),json!({"text_offset":true}),json!({"max_characters":"20000"}),json!({"text_offset":-1}),json!({"ocr":"false"}),json!({"target":false}),json!({"source_sha256":"wrong"}),json!({"text_offset":1})]{assert!(validate_options(&bad).is_err(),"{bad}");}
        assert!(validate_options(&json!({})).is_ok());assert!(validate_options(&json!({"text_offset":20000,"source_sha256":"a".repeat(64),"max_pages":5,"ocr":false})).is_ok());
    }
    #[test]
    fn run_edges_are_preserved() {
        assert_eq!(edges(" Hello ", "Привет\n"), " Привет ");
        assert_eq!(edges("\tword\n", "слово"), "\tслово\n");
    }
}
