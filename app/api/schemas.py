"""Response shapes, exactly as the brief's appendix defines them. Keep field names in camelCase."""
from typing import List, Literal, Optional

from pydantic import BaseModel

PortalState = Literal["integrating", "serving", "healing", "failed"]
RunKind = Literal["read", "repair"]
RunResult = Literal["ok", "failed"]
TenderStatus = Literal["open", "closed", "withdrawn"]


class PortalIn(BaseModel):
    url: str


class PortalOut(BaseModel):
    id: str
    url: str
    state: PortalState
    reason: str
    tenderCount: int
    lastSuccessAt: Optional[str]  # date-time with offset, or null


class Run(BaseModel):
    id: str
    kind: RunKind
    startedAt: str
    finishedAt: Optional[str]
    result: Optional[RunResult]  # null while running


class Document(BaseModel):
    fileName: str
    sizeBytes: int
    sha256: str
    path: str  # e.g. /files/ab12


class Place(BaseModel):
    postalCode: Optional[str] = None
    city: Optional[str] = None
    countryCode: Optional[str] = None  # two letters, e.g. DE


class Tender(BaseModel):
    # required: send null when the portal does not show it
    id: Optional[str]
    url: Optional[str]
    title: Optional[str]
    buyerName: Optional[str]
    publicationDate: Optional[str]  # YYYY-MM-DD
    submissionDeadline: Optional[str]  # date-time with offset
    status: TenderStatus
    documents: List[Document]
    lastChangedAt: str
    # if shown: leave out entirely when absent (serialize with exclude_unset)
    referenceNumber: Optional[str] = None
    description: Optional[str] = None
    procedureType: Optional[Literal["open", "restricted", "negotiated", "other"]] = None
    contractNature: Optional[Literal["works", "services", "supplies"]] = None
    cpvCodes: Optional[List[str]] = None  # eight digits, no check digit
    placeOfPerformance: Optional[Place] = None
