use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    routing::post,
};
use reqwest::{Client, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{env, sync::Arc, time::Duration};

const TOKEN_URL: &str = "https://api.elevenlabs.io/v1/convai/conversation/token";

struct ElevenLabs {
    client: Client,
    api_key: HeaderValue,
    agent_id: String,
}

#[derive(Deserialize)]
struct ConversationToken {
    token: String,
}

impl ElevenLabs {
    fn from_env() -> Option<Self> {
        let agent_id = env::var("ELEVENLABS_AGENT_ID").ok()?.trim().to_owned();
        let key = env::var("ELEVENLABS_API_KEY").ok()?;
        if agent_id.is_empty() || key.trim().is_empty() {
            return None;
        }

        let mut api_key = key.trim().parse::<HeaderValue>().ok()?;
        api_key.set_sensitive(true);
        let client = Client::builder()
            .timeout(Duration::from_secs(8))
            .redirect(Policy::none())
            .build()
            .ok()?;
        Some(Self {
            client,
            api_key,
            agent_id,
        })
    }

    async fn conversation_token(&self) -> reqwest::Result<ConversationToken> {
        self.client
            .get(TOKEN_URL)
            .query(&[("agent_id", &self.agent_id)])
            .header("xi-api-key", self.api_key.clone())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }
}

pub(crate) fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/assistant/voice/session", post(start_session))
        .with_state(ElevenLabs::from_env().map(Arc::new))
}

type SessionResponse = (StatusCode, [(HeaderName, &'static str); 1], Json<Value>);

fn response(status: StatusCode, body: Value) -> SessionResponse {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(body))
}

async fn start_session(
    State(voice): State<Option<Arc<ElevenLabs>>>,
    headers: HeaderMap,
) -> SessionResponse {
    let fetch_site = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok());
    if matches!(fetch_site, Some("cross-site" | "same-site")) {
        return response(
            StatusCode::FORBIDDEN,
            json!({ "detail": "Start the voice session from the Watchtower dashboard." }),
        );
    }

    let Some(voice) = voice else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "detail": "Voice setup requires ELEVENLABS_API_KEY and ELEVENLABS_AGENT_ID on the server." }),
        );
    };

    match voice.conversation_token().await {
        Ok(ConversationToken { token }) if !token.trim().is_empty() => {
            response(StatusCode::OK, json!({ "conversationToken": token }))
        }
        _ => response(
            StatusCode::BAD_GATEWAY,
            json!({ "detail": "Could not start an ElevenLabs session. Check the agent and API key." }),
        ),
    }
}
