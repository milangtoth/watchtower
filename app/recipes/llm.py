"""Use Jev to select among observed links when discovery is ambiguous."""
import httpx

from app import config


async def choose(candidates):
    if not config.TYPESAFE_API_KEY or len(candidates) < 2:
        return candidates[0][1]
    options = {str(i): f'{label}: {url}' for i, (_, url, label) in enumerate(candidates[:20])}
    # Separate credentials from the portal client. The model can only select observed URLs.
    async with httpx.AsyncClient(timeout=15, trust_env=False) as client:
        try:
            response = await client.post('https://api.typesafe.ai/v1/systemone',
                headers={'Authorization': f'Bearer {config.TYPESAFE_API_KEY}'},
                json={'model': config.JEV_MODEL, 'state': options, 'questions': {
                    'source': {'type': 'choice', 'criteria': options,
                               'instructions': 'Choose the public procurement notice feed or sitemap.'}}})
            response.raise_for_status()
            choice = response.json()['answers']['source']['choice']
            if choice in options:
                return candidates[int(choice)][1]
        except (httpx.HTTPError, ValueError, KeyError, TypeError):
            pass
    return candidates[0][1]
