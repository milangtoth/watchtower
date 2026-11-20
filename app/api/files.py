"""Serves stored document bytes at /files/{file_id}."""
from fastapi import APIRouter, HTTPException

router = APIRouter()


@router.get("/files/{file_id}")
def get_file(file_id: str):
    raise HTTPException(501, "TODO")
