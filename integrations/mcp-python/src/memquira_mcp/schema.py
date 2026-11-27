"""A small, strict validator for the JSON Schema subset the tool input schemas use.

The schemas published in ``tools/list`` are the ones enforced here, so the two cannot drift. Supported keywords:
``type`` (object, string, integer, boolean, array), ``properties``, ``required``, ``additionalProperties: false``,
``minLength``, ``maxLength``, ``pattern``, ``enum``, ``minimum``, ``maximum``, ``items``, ``maxItems``,
``uniqueItems``. Error messages name the field and the rule, never the rejected value, so oversized or secret
input is not echoed back to the model.
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from functools import lru_cache
from typing import Any


class ArgumentError(ValueError):
    """Tool arguments do not match the tool's input schema."""


@lru_cache(maxsize=64)
def _regex(pattern: str) -> re.Pattern[str]:
    return re.compile(pattern)


def _check(schema: Mapping[str, Any], value: Any, path: str) -> None:
    kind = schema.get("type")
    if kind == "object":
        if not isinstance(value, dict):
            raise ArgumentError(f"{path} must be an object")
        props: Mapping[str, Any] = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            unknown = sorted(k for k in value if k not in props)
            if unknown:
                names = ", ".join(repr(k[:40]) for k in unknown[:5])
                raise ArgumentError(
                    f"unknown argument(s) {names}; this tool accepts only: {', '.join(props) or 'none'}"
                )
        for name in schema.get("required", []):
            if name not in value:
                raise ArgumentError(f"missing required argument '{name}'")
        for name, sub in props.items():
            if name in value:
                _check(sub, value[name], name if path == "arguments" else f"{path}.{name}")
        return
    if kind == "string":
        if not isinstance(value, str):
            raise ArgumentError(f"{path} must be a string")
        if "minLength" in schema and len(value) < schema["minLength"]:
            raise ArgumentError(f"{path} must be at least {schema['minLength']} characters")
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            raise ArgumentError(f"{path} must be at most {schema['maxLength']} characters (got {len(value)})")
        if "pattern" in schema and not _regex(schema["pattern"]).search(value):
            raise ArgumentError(f"{path} has an invalid format: {schema.get('description', schema['pattern'])}")
    elif kind == "integer":
        # bool is a subclass of int in Python; JSON true is not an integer.
        if not isinstance(value, int) or isinstance(value, bool):
            raise ArgumentError(f"{path} must be an integer")
        if "minimum" in schema and value < schema["minimum"]:
            raise ArgumentError(f"{path} must be at least {schema['minimum']}")
        if "maximum" in schema and value > schema["maximum"]:
            raise ArgumentError(f"{path} must be at most {schema['maximum']}")
    elif kind == "boolean":
        if not isinstance(value, bool):
            raise ArgumentError(f"{path} must be true or false")
    elif kind == "array":
        if not isinstance(value, list):
            raise ArgumentError(f"{path} must be an array")
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            raise ArgumentError(f"{path} must have at most {schema['maxItems']} items")
        if schema.get("uniqueItems") and len({repr(v) for v in value}) != len(value):
            raise ArgumentError(f"{path} must not repeat items")
        for i, item in enumerate(value):
            _check(schema.get("items", {}), item, f"{path}[{i}]")
    if "enum" in schema and value not in schema["enum"]:
        allowed = ", ".join(str(v) for v in schema["enum"])
        raise ArgumentError(f"{path} must be one of: {allowed}")


def validate(schema: Mapping[str, Any], arguments: Any) -> dict[str, Any]:
    """Check ``arguments`` against ``schema`` and return them. Raises ``ArgumentError``."""
    value = {} if arguments is None else arguments
    _check(schema, value, "arguments")
    return dict(value)
