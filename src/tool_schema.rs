//! Validate the typed tool contract before defaults, approvals, or side effects.
//! This implements the subset used by our definitions, not arbitrary JSON Schema.
use anyhow::Result;
use serde_json::Value;
use std::sync::OnceLock;
pub fn validate(name:&str,args:&Value)->Result<()> {
    static DEFINITIONS:OnceLock<Value>=OnceLock::new();
    let definitions=DEFINITIONS.get_or_init(crate::tools::definitions);
    let definition=definitions.as_array().unwrap().iter().find(|item|item["function"]["name"]==name).ok_or_else(||anyhow::anyhow!("Неизвестный инструмент: {name}"))?;
    check(args,&definition["function"]["parameters"],name)?;
    if matches!(name,"run_command"|"start_process") {if let Some(seconds)=args["timeout_seconds"].as_u64(){crate::lifecycle::deadline(seconds)?;}}
    Ok(())
}
fn check(value:&Value,schema:&Value,path:&str)->Result<()> {
    let valid=match schema["type"].as_str(){Some("object")=>value.is_object(),Some("array")=>value.is_array(),Some("string")=>value.is_string(),Some("integer")=>value.is_i64()||value.is_u64(),Some("number")=>value.is_number(),Some("boolean")=>value.is_boolean(),_=>true};
    anyhow::ensure!(valid,"{path}: ожидается {}",schema["type"].as_str().unwrap_or("значение"));
    if let Some(choices)=schema["enum"].as_array(){anyhow::ensure!(choices.contains(value),"{path}: недопустимое значение");}
    if let Some(number)=value.as_f64(){
        if let Some(min)=schema["minimum"].as_f64(){anyhow::ensure!(number>=min,"{path}: значение должно быть не меньше {min}");}
        if let Some(max)=schema["maximum"].as_f64(){anyhow::ensure!(number<=max,"{path}: значение должно быть не больше {max}");}
    }
    if let Some(object)=value.as_object(){
        let required=schema["required"].as_array().cloned().unwrap_or_default();
        for key in &required {let key=key.as_str().unwrap();anyhow::ensure!(object.contains_key(key),"{path}: не задан {key}");}
        for (key,value) in object {
            let definition=schema["properties"].get(key).ok_or_else(||anyhow::anyhow!("{path}: неизвестный параметр {key}"))?;
            if value.is_null()&&!required.iter().any(|item|item==key){continue;}
            check(value,definition,&format!("{path}.{key}"))?;
        }
    }
    if let Some(array)=value.as_array(){
        if let Some(min)=schema["minItems"].as_u64(){anyhow::ensure!(array.len() as u64>=min,"{path}: слишком мало элементов");}
        if let Some(max)=schema["maxItems"].as_u64(){anyhow::ensure!(array.len() as u64<=max,"{path}: слишком много элементов");}
        for (index,value) in array.iter().enumerate(){check(value,&schema["items"],&format!("{path}[{index}]"))?;}
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[test]
    fn malformed_deadlines_offsets_and_nested_arguments_do_not_fall_back_to_defaults(){
        for timeout in [json!(-1),json!(1.5),json!("60"),json!(true)]{assert!(validate("run_command",&json!({"command":"never execute","timeout_seconds":timeout})).is_err());assert!(validate("start_process",&json!({"command":"never execute","timeout_seconds":timeout})).is_err());}
        assert!(validate("poll_process",&json!({"execution_id":"id","stdout_offset":-1})).is_err());
        assert!(validate("run_command",&json!({"command":"never execute","timeout_seconds":u64::MAX})).is_err());
        assert!(validate("write_file",&json!({"path":"file"})).is_err());
        assert!(validate("run_command",&json!({"command":"never execute","cwd":"wrong_parameter"})).is_err());
        assert!(validate("update_plan",&json!({"steps":[{"title":"Check","status":"invented"}]})).is_err());
        assert!(validate("run_command",&json!({"command":"allowed","timeout_seconds":0,"directory":null})).is_ok());
    }
}
