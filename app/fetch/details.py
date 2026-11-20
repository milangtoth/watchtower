"""Follow observed procurement links and verify the destination against the listing."""
import re
from urllib.parse import urljoin
from bs4 import BeautifulSoup

CONTRACT_LINK = re.compile(
    r'(?:view|open|go to|bekijk|naar) (?:the |de |het )?(?:original |oorspronkelijke )?'
    r'(?:contract notice|tender details|aanbesteding|publicatie)|originele publicatie', re.I)
DETAIL = re.compile(r'submission deadline|contracting authority|procurement documents|'
                    r'aanbestedende dienst|inschrijfdatum|aanbestedingsstukken|CPV', re.I)


async def resolve(reader, url, fields, evidence=None):
    visited = set()
    while url not in visited and len(visited) < 4:
        visited.add(url)
        page = await reader.get(url)
        soup = BeautifulSoup(page.body, 'html.parser')
        text = ' '.join(soup.stripped_strings).casefold()
        links = {urljoin(page.url, a['href']) for a in soup.select('a[href]')
                 if CONTRACT_LINK.search(a.get_text(' ', strip=True))}
        links -= visited | {page.url}
        if len(links) == 1:
            url = links.pop()
            continue
        # A summary breadcrumb or generic "contracts" link alone cannot establish identity.
        identity = any(str(fields[k]).casefold() in text for k in ('title', 'id')
                       if fields.get(k) and len(str(fields[k])) >= 3)
        if not links and identity and ((evidence and soup.select_one(evidence)) or DETAIL.search(text)):
            return page, soup
        break
    raise ValueError('Could not verify the destination as the tender detail page')
