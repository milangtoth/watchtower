"""Background loop: schedules reads per portal, handles refresh requests and deletions."""
import asyncio
from typing import Optional

_task: Optional[asyncio.Task] = None


async def _loop() -> None:
    while True:
        # TODO: pick due portals, start runs (attach / read / repair)
        await asyncio.sleep(5)


def start() -> None:
    global _task
    _task = asyncio.get_event_loop().create_task(_loop())


async def stop() -> None:
    if _task:
        _task.cancel()
