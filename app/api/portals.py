"""The /portals endpoints. Status codes and bodies must match the appendix exactly."""
from typing import List, Optional

from fastapi import APIRouter, HTTPException

from app.api.schemas import PortalIn, PortalOut, Run, Tender

router = APIRouter()


@router.post("/portals", status_code=201)
def add_portal(body: PortalIn) -> dict:
    """201 {id} at once; attach in the background. Same URL -> same id."""
    raise HTTPException(501, "TODO")


@router.get("/portals", response_model=List[PortalOut])
def list_portals():
    raise HTTPException(501, "TODO")


@router.get("/portals/{portal_id}", response_model=PortalOut)
def get_portal(portal_id: str):
    raise HTTPException(501, "TODO")  # unknown id -> 404


@router.delete("/portals/{portal_id}", status_code=204)
def delete_portal(portal_id: str):
    """204. The worker must stop calling the portal within a minute."""
    raise HTTPException(501, "TODO")


@router.post("/portals/{portal_id}/refresh", status_code=202)
def refresh_portal(portal_id: str) -> dict:
    """202 {runId} at once. If a read is already running, return its runId."""
    raise HTTPException(501, "TODO")


@router.get("/portals/{portal_id}/tenders", response_model=List[Tender], response_model_exclude_unset=True)
def list_tenders(portal_id: str, changedSince: Optional[str] = None):
    """While healing, keep serving the last correct tenders."""
    raise HTTPException(501, "TODO")


@router.get("/portals/{portal_id}/runs", response_model=List[Run])
def list_runs(portal_id: str):
    raise HTTPException(501, "TODO")
