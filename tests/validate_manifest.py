"""Validate against the registry's live driver schema without publishing anything."""
import json
from pathlib import Path
from urllib.request import urlopen

from jsonschema import Draft202012Validator

root = Path(__file__).resolve().parent.parent
manifest = json.loads((root / ".tabularium").read_text(encoding="utf-8"))
with urlopen(manifest["$schema"], timeout=30) as response:
    schema = json.load(response)
Draft202012Validator.check_schema(schema)
Draft202012Validator(schema).validate(manifest)
print("Manifest matches the live Tabularis registry driver schema.")
