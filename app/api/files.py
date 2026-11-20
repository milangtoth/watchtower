import re
from fastapi import APIRouter, HTTPException
from fastapi.responses import FileResponse
from app import config

router = APIRouter()


@router.get('/files/{file_id}')
def get_file(file_id: str):
    if re.fullmatch('[a-f0-9]{64}', file_id):
        path = config.FILES_DIR / file_id
        if path.is_file():
            return FileResponse(path, media_type='application/octet-stream')
    raise HTTPException(404, 'Unknown file')
