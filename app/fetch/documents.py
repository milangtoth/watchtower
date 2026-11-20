from hashlib import sha256
from io import BytesIO
from pathlib import PurePosixPath
from zipfile import ZipFile, is_zipfile
from uuid import uuid4

from app import config


def store(name, data):
    digest = sha256(data).hexdigest()
    path = config.FILES_DIR / digest
    if not path.exists():
        stage = path.with_suffix('.' + uuid4().hex)
        stage.write_bytes(data)
        stage.replace(path)
    return dict(fileName=name, sizeBytes=len(data), sha256=digest, path='/files/' + digest)


def unpack(name, data):
    stream = BytesIO(data)
    if not is_zipfile(stream):
        return [store(name, data)]
    result = []
    with ZipFile(stream) as archive:
        for entry in archive.infolist():
            if not entry.is_dir() and entry.file_size <= config.MAX_FILE_BYTES:
                result.append(store(PurePosixPath(entry.filename).name, archive.read(entry)))
    return result
