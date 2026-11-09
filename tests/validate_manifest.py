"""Validate against the registry driver schema captured on 2026-11-09."""
import json
from pathlib import Path

from jsonschema import Draft202012Validator

root = Path(__file__).resolve().parent.parent
manifest = json.loads((root / ".tabularium").read_text(encoding="utf-8"))
schema = json.loads((root / "tests/driver-manifest.schema.json").read_text(encoding="utf-8"))
Draft202012Validator.check_schema(schema)
Draft202012Validator(schema).validate(manifest)
print("Manifest matches the Tabularis registry driver schema (2026-11-09 snapshot).")
