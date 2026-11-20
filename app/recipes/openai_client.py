"""OpenAI calls used to learn recipes and cross-check extracted tenders."""
import json
import logging

import httpx

from app import config


logger = logging.getLogger(__name__)
CHAT_COMPLETIONS = "https://api.openai.com/v1/chat/completions"


def parse_answer(payload: dict) -> dict:
    """Only a complete assistant answer can become a saved recipe or verdict."""
    choices = payload.get("choices") or []
    if not choices or choices[0].get("finish_reason") != "stop":
        raise ValueError("OpenAI returned an incomplete answer")
    message = choices[0].get("message") or {}
    if message.get("refusal"):
        raise ValueError("OpenAI declined the extraction request")
    try:
        answer = json.loads(message.get("content"))
    except (TypeError, ValueError) as exc:
        raise ValueError("OpenAI returned invalid recipe JSON") from exc
    if not isinstance(answer, dict):
        raise ValueError("Expected a JSON object from OpenAI")
    if answer.get("error"):
        raise ValueError(str(answer["error"])[:300])
    return answer


async def ask_json(instructions: str, evidence: str) -> dict:
    if not config.OPENAI_API_KEY:
        raise ValueError("Recipe learning requires OPENAI_API_KEY")

    # Keep untrusted portal content in the user message and the API key on this client.
    request = {
        "model": config.MODEL,
        "messages": [
            {"role": "developer", "content": instructions},
            {"role": "user", "content": evidence},
        ],
        "response_format": {"type": "json_object"},
        "max_completion_tokens": 1400,
        "store": False,
    }
    async with httpx.AsyncClient(timeout=45, trust_env=False) as client:
        response = await client.post(
            CHAT_COMPLETIONS,
            headers={"Authorization": f"Bearer {config.OPENAI_API_KEY}"},
            json=request,
        )
    # Portal failure reasons are public. Never copy provider error bodies into them.
    if not response.is_success:
        raise ValueError(f"OpenAI model request failed (HTTP {response.status_code})")
    payload = response.json()
    usage = payload.get("usage") or {}
    logger.info("Model %s used %s input and %s output tokens",
                config.MODEL, usage.get("prompt_tokens"), usage.get("completion_tokens"))
    return parse_answer(payload)
