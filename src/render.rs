use serde::{Deserialize, Serialize};
use temple_dsl::{Template, Value};

use crate::error::{Result, WireJackError};
use crate::request::ApiRequest;
use crate::response::ApiResponse;

const PHASE_REQUEST: &str = "request";
const PHASE_RESPONSE: &str = "response";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub output: serde_json::Value,
    pub response: ApiResponse,
}

pub fn render_request(template: &Template, data: impl Into<Value>) -> Result<ApiRequest> {
    let input = Value::obj([
        ("phase", Value::Str(PHASE_REQUEST.into())),
        ("request", data.into()),
    ]);
    template
        .render(input)
        .map_err(|e| WireJackError::Render(e.to_string()))
}

pub fn render_response(
    template: &Template,
    request: Value,
    response: &ApiResponse,
) -> Result<serde_json::Value> {
    let response_value: Value = serde_json::to_value(response)
        .map_err(|e| WireJackError::Render(e.to_string()))?
        .into();
    let input = Value::obj([
        ("phase", Value::Str(PHASE_RESPONSE.into())),
        ("request", request),
        ("response", response_value),
    ]);
    template.render_value(input).map(Into::into).map_err(|e| {
        WireJackError::Render(format!(
            "response phase failed for {} {} response: {e}",
            response.status, response.status_text
        ))
    })
}

pub async fn run(template: &Template, data: impl Into<Value>) -> Result<RunResult> {
    let request_data = data.into();
    let request = render_request(template, request_data.clone())?;
    let response = crate::execute(&request).await?;
    let output = render_response(template, request_data, &response)?;
    Ok(RunResult { output, response })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Template {
        Template::compile(
            r#"{{
                input.phase == "request"
                  ? {
                      "url": concat("https://api.example.com/users/", to_string(input.request.id)),
                      "method": "GET"
                    }
                  : {
                      "userId": input.response.body_json.id,
                      "ok": input.response.ok
                    }
            }}"#,
        )
        .unwrap()
    }

    #[test]
    fn renders_request_phase_into_api_request() {
        let req = render_request(&contract(), Value::obj([("id", Value::Int(7))])).unwrap();
        assert_eq!(req.url, "https://api.example.com/users/7");
        assert_eq!(req.method, "GET");
    }

    #[test]
    fn renders_response_phase_free_form() {
        let response = ApiResponse {
            ok: true,
            body_json: Some(serde_json::json!({ "id": 7 })),
            ..Default::default()
        };
        let out =
            render_response(&contract(), Value::obj([("id", Value::Int(7))]), &response).unwrap();
        assert_eq!(out["userId"], serde_json::json!(7));
        assert_eq!(out["ok"], serde_json::json!(true));
    }
}
