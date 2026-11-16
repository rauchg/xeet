//! OpenAI Chat Completions as one concrete [`ModelClient`].
//!
//! Only the encoding and response handling live here; bytes move through an
//! injected [`Transport`], so this crate opens no sockets and holds no keys.
//! Media other than images cannot be encoded for this endpoint and are
//! refused before sending rather than silently dropped.
//!
//! Verified in this repository: the error path against the live API (an
//! HTTP 429 `insufficient_quota`, recorded in
//! `tests/fixtures/gateway/openai-429-insufficient-quota.json`). Not verified:
//! a successful completion, because the available account has no credits.

use serde_json::{json, Value};

use crate::request::{Content, ModelClient, ModelRequest, ModelResponse, ProviderError, Role};

pub const ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

pub trait Transport {
    /// POSTs JSON; returns the HTTP status and body, or a transport error.
    fn post_json(&mut self, url: &str, body: &str) -> Result<(u16, String), String>;
}

/// The Chat Completions body for a request.
pub fn encode(req: &ModelRequest, max_completion_tokens: Option<u64>) -> Result<Value, String> {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let parts: Result<Vec<Value>, String> = m
                .content
                .iter()
                .map(|c| match c {
                    Content::Text { text } => Ok(json!({"type": "text", "text": text})),
                    Content::Media { media_type, data_base64, .. } if media_type.starts_with("image/") => {
                        Ok(json!({"type": "image_url", "image_url": {"url": format!("data:{media_type};base64,{data_base64}")}}))
                    }
                    Content::Media { media_type, .. } => Err(format!("{media_type} cannot be encoded for chat completions")),
                })
                .collect();
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            Ok(json!({"role": role, "content": parts?}))
        })
        .collect::<Result<_, String>>()?;
    let mut body = json!({"model": req.model, "messages": messages});
    if let Some(n) = max_completion_tokens {
        body["max_completion_tokens"] = json!(n);
    }
    Ok(body)
}

pub struct Client<T: Transport> {
    pub transport: T,
    pub max_completion_tokens: Option<u64>,
}

impl<T: Transport> ModelClient for Client<T> {
    fn send(&mut self, req: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        let body = encode(req, self.max_completion_tokens).map_err(|e| ProviderError {
            status: None,
            body: e,
            transport: true,
        })?;
        let (status, text) = self
            .transport
            .post_json(ENDPOINT, &body.to_string())
            .map_err(|e| ProviderError {
                status: None,
                body: e,
                transport: true,
            })?;
        if !(200..300).contains(&status) {
            return Err(ProviderError {
                status: Some(status),
                body: text,
                transport: false,
            });
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| ProviderError {
            status: Some(status),
            body: format!("unparseable response: {e}"),
            transport: false,
        })?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Ok(ModelResponse { text: content })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Message;

    #[test]
    fn encodes_text_and_images_and_refuses_other_media() {
        let req = ModelRequest {
            provider: "openai".into(),
            model: "m".into(),
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    Content::Text { text: "hi".into() },
                    Content::Media {
                        media_type: "image/png".into(),
                        data_base64: "AAAA".into(),
                        name: None,
                    },
                ],
            }],
        };
        let b = encode(&req, Some(5)).unwrap();
        assert_eq!(
            b["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(b["max_completion_tokens"], 5);
        let mut audio = req.clone();
        audio.messages[0].content[1] = Content::Media {
            media_type: "audio/mpeg".into(),
            data_base64: "AAAA".into(),
            name: None,
        };
        assert!(encode(&audio, None).is_err());
    }
}
