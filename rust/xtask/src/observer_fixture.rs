use autorouter_runtime::response_observer::{Observation, ResponseObserver};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub fn execute(input: &Value) -> Result<Value, String> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut observer = ResponseObserver::new(
        input
            .get("content_type")
            .and_then(Value::as_str)
            .unwrap_or("text/event-stream"),
        input
            .get("max_buffer_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(65536) as usize,
        move |event| sink.lock().expect("observer fixture mutex").push(event),
    )
    .map_err(|_| "maxBufferBytes must be a positive integer")?;
    let mut forwarded = Vec::new();
    for chunk in input
        .get("chunks")
        .and_then(Value::as_array)
        .ok_or("Observer fixture needs chunks")?
    {
        let bytes = chunk
            .as_array()
            .ok_or("Observer chunk must be a byte array")?
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or("Invalid observer byte")
            })
            .collect::<Result<Vec<_>, _>>()?;
        observer.push(&bytes);
        forwarded.extend_from_slice(&bytes);
    }
    if input["action"] == "destroy" {
        observer.destroy();
    } else {
        observer.finish();
    }
    let mut result = json!({"forwarded":forwarded,"models":[],"errors":[],"usages":[],"executions":[],"completions":[]});
    let model_value = |model: &autorouter_core::js_json::JsString| -> Result<Value, String> {
        if input["utf16_strings"] == true {
            Ok(json!({"utf16":model.units()}))
        } else {
            serde_json::to_value(model)
                .map_err(|_| "Observer output needs utf16_strings for exact identity".into())
        }
    };
    for event in events.lock().expect("observer fixture mutex").iter() {
        let (key, value) = match event {
            Observation::Model { model } => ("models", json!({"model":model_value(model)?})),
            Observation::Error { error_type } => ("errors", json!({"error_type":error_type})),
            Observation::Usage { usage } => ("usages", json!({"usage":usage})),
            Observation::Execution { model, source } => (
                "executions",
                json!({"model":model_value(model)?,"source":source}),
            ),
            Observation::Complete(evidence) => {
                let mut value = json!({"model":model_value(&evidence.model)?,"stop_reason":evidence.stop_reason,"tool_uses":evidence.tool_uses.iter().map(|tool| Ok(json!({"id":tool.id,"model":model_value(&tool.model)?}))).collect::<Result<Vec<_>,String>>()?});
                if let Some(model) = &evidence.continuation_model {
                    value["continuation_model"] = model_value(model)?;
                }
                ("completions", value)
            }
        };
        result[key]
            .as_array_mut()
            .expect("observer result array")
            .push(value);
    }
    if input["utf16_strings"] == true {
        result = utf16_values(result);
    }
    Ok(result)
}

fn utf16_values(value: Value) -> Value {
    match value {
        Value::String(value) => json!({"utf16":value.encode_utf16().collect::<Vec<_>>()}),
        Value::Array(values) => Value::Array(values.into_iter().map(utf16_values).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, utf16_values(value)))
                .collect(),
        ),
        value => value,
    }
}
