import logging
import re
import sys
from typing import List, Optional

from pygls.server import LanguageServer
from lsprotocol.types import (
    TEXT_DOCUMENT_DID_OPEN,
    TEXT_DOCUMENT_DID_CHANGE,
    TEXT_DOCUMENT_DID_CLOSE,
    TEXT_DOCUMENT_DID_SAVE,
    TEXT_DOCUMENT_HOVER,
    TEXT_DOCUMENT_DOCUMENT_SYMBOL,
    DidOpenTextDocumentParams,
    DidChangeTextDocumentParams,
    DidCloseTextDocumentParams,
    DidSaveTextDocumentParams,
    HoverParams,
    Hover,
    DocumentSymbolParams,
    DocumentSymbol,
    SymbolKind,
    Range,
    Position,
    Diagnostic,
    DiagnosticSeverity,
)

logging.basicConfig(level=logging.ERROR, stream=sys.stderr)

server = LanguageServer("ostadix-lsp", "1.0.0")

BLOCK_PATTERN = re.compile(r"([A-Z0-9_a-z]+)\^\((.*?)\)_([A-Z0-9_a-z]+)", re.DOTALL)

def validate(ls: LanguageServer, uri: str):
    doc = ls.workspace.get_text_document(uri)
    text = doc.source
    diagnostics = []

    # Find blocks using regex
    for match in BLOCK_PATTERN.finditer(text):
        start_tag = match.group(1)
        end_tag = match.group(3)
        if start_tag != end_tag:
            # Add diagnostic
            start_pos = doc.position_at(match.start())
            end_pos = doc.position_at(match.end())
            diag = Diagnostic(
                range=Range(start=start_pos, end=end_pos),
                message=f"Mismatched O-lang tags: {start_tag}^(...)_{end_tag}",
                severity=DiagnosticSeverity.Error,
                source="ostadix-lsp"
            )
            diagnostics.append(diag)
            
    # Check for unclosed tags
    # A simple heuristic: if we have more `^(` than `)_`
    # Just skipping complex parsing to keep it fast
    
    ls.publish_diagnostics(uri, diagnostics)

@server.feature(TEXT_DOCUMENT_DID_OPEN)
def did_open(ls, params: DidOpenTextDocumentParams):
    validate(ls, params.text_document.uri)

@server.feature(TEXT_DOCUMENT_DID_CHANGE)
def did_change(ls, params: DidChangeTextDocumentParams):
    validate(ls, params.text_document.uri)

@server.feature(TEXT_DOCUMENT_DID_SAVE)
def did_save(ls, params: DidSaveTextDocumentParams):
    validate(ls, params.text_document.uri)

@server.feature(TEXT_DOCUMENT_DID_CLOSE)
def did_close(ls, params: DidCloseTextDocumentParams):
    ls.publish_diagnostics(params.text_document.uri, [])

@server.feature(TEXT_DOCUMENT_HOVER)
def hover(ls: LanguageServer, params: HoverParams) -> Optional[Hover]:
    doc = ls.workspace.get_text_document(params.text_document.uri)
    word = doc.word_at_position(params.position)
    
    # Very naive hover
    if "^" in word or "_" in word or word.isupper():
        return Hover(contents=f"O-lang block tag: `{word}`")
        
    return None

@server.feature(TEXT_DOCUMENT_DOCUMENT_SYMBOL)
def document_symbol(ls: LanguageServer, params: DocumentSymbolParams) -> Optional[List[DocumentSymbol]]:
    doc = ls.workspace.get_text_document(params.text_document.uri)
    text = doc.source
    symbols = []
    
    for match in BLOCK_PATTERN.finditer(text):
        tag = match.group(1)
        start_pos = doc.position_at(match.start())
        end_pos = doc.position_at(match.end())
        r = Range(start=start_pos, end=end_pos)
        
        symbols.append(
            DocumentSymbol(
                name=tag,
                kind=SymbolKind.Module,
                range=r,
                selection_range=r,
                detail=f"O-lang {tag} block"
            )
        )
        
    return symbols

if __name__ == "__main__":
    server.start_io()
