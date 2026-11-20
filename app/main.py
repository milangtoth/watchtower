"""Entry point: one process serves the API and the dashboard on port 8080 and runs the worker."""
from contextlib import asynccontextmanager

from fastapi import FastAPI

from app.api import files, portals
from app.dashboard import views
from app.db import database
from app.worker import scheduler


@asynccontextmanager
async def lifespan(_: FastAPI):
    database.init()
    scheduler.start()
    yield
    await scheduler.stop()


app = FastAPI(title="Watchtower", lifespan=lifespan)
app.include_router(portals.router)
app.include_router(files.router)
app.include_router(views.router)
