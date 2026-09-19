import json
import logging
from collections.abc import Mapping
from typing import Any, Callable, TypeVar, get_args, get_origin

import msgspec
from fastapi import Body, Depends, HTTPException
from pydantic_core import core_schema
from fastapi.routing import APIRoute
from fastapi.responses import JSONResponse
from starlette.requests import Request
from starlette.responses import Response

from infrastructure.observability.provider_counters import route_scope

T = TypeVar("T")
logger = logging.getLogger(__name__)


class AppStruct(msgspec.Struct, kw_only=True):
    def __iter__(self):
        return iter(msgspec.to_builtins(self).items())

    @classmethod
    def __get_pydantic_core_schema__(
        cls,
        source_type: Any,
        _handler: Any,
    ) -> core_schema.CoreSchema:
        def validate(value: Any) -> Any:
            if isinstance(value, cls):
                return value

            try:
                return msgspec.convert(value, type=source_type, strict=False)
            except (msgspec.ValidationError, TypeError, ValueError) as exc:
                raise ValueError(str(exc)) from exc

        return core_schema.no_info_plain_validator_function(
            validate,
            serialization=core_schema.plain_serializer_function_ser_schema(
                lambda value: msgspec.to_builtins(value)
            ),
        )

    @classmethod
    def __get_pydantic_json_schema__(
        cls,
        _core_schema: core_schema.CoreSchema,
        _handler: Any,
    ) -> dict[str, Any]:
        try:
            schema = dict(msgspec.json.schema(cls))
        except TypeError as exc:
            logger.warning("Falling back to generic OpenAPI schema for %s: %s", cls.__name__, exc)
            return {"type": "object", "title": cls.__name__}

        if "$ref" in schema or "$defs" in schema:
            logger.warning(
                "Falling back to generic OpenAPI schema for %s due to unsupported refs/defs in msgspec schema",
                cls.__name__,
            )
            return {"type": "object", "title": cls.__name__}

        return schema


class MsgSpecJSONResponse(JSONResponse):
    def render(self, content: Any) -> bytes:
        try:
            return msgspec.json.encode(content)
        except TypeError:
            return super().render(content)


class MsgSpecJSONRequest(Request):
    async def json(self) -> Any:
        if not hasattr(self, "_msgspec_json"):
            body = await self.body()
            try:
                self._msgspec_json = msgspec.json.decode(body)
            except msgspec.DecodeError as exc:
                body_text = body.decode("utf-8", errors="replace")
                raise json.JSONDecodeError(str(exc), body_text, 0) from exc
        return self._msgspec_json


class MsgSpecRoute(APIRoute):
    def __init__(
        self,
        *args: Any,
        response_model: Any = None,
        openapi_extra: Any = None,
        **kwargs: Any,
    ) -> None:
        route_openapi_extra = openapi_extra
        resolved_response_model = response_model

        if _contains_msgspec_struct(response_model):
            try:
                schema = msgspec.json.schema(response_model)
            except TypeError:
                schema = None

            if schema is not None:
                route_openapi_extra = _merge_response_schema(route_openapi_extra, schema)
                resolved_response_model = None

        super().__init__(*args, response_model=resolved_response_model, openapi_extra=route_openapi_extra, **kwargs)

    def get_route_handler(self) -> Callable[[Request], Response]:
        original_route_handler = super().get_route_handler()

        async def custom_route_handler(request: Request) -> Response:
            request = MsgSpecJSONRequest(request.scope, request.receive)
            # Post-match: scope["route"].path is the template, never the raw
            # path. Unmatched requests stamp "unknown"; pure ContextVar work,
            # never raises.
            route = request.scope.get("route")
            template = getattr(route, "path", None) or "unknown"
            with route_scope(template):
                return await original_route_handler(request)

        return custom_route_handler


def MsgSpecBody(model: type[T]) -> Any:
    async def dependency(payload: Any = Body(...)) -> T:
        try:
            return msgspec.convert(payload, type=model, strict=False)
        except (msgspec.ValidationError, ValueError) as exc:
            raise HTTPException(status_code=422, detail=str(exc)) from exc

    dependency.__annotations__["payload"] = model

    return Depends(dependency)


def _contains_msgspec_struct(value: Any) -> bool:
    if value is None:
        return False

    if isinstance(value, type) and issubclass(value, msgspec.Struct):
        return True

    origin = get_origin(value)
    if origin is None:
        return False

    args = get_args(value)
    return any(_contains_msgspec_struct(argument) for argument in args)


def _merge_response_schema(openapi_extra: Any, schema: Mapping[str, Any]) -> dict[str, Any]:
    merged = dict(openapi_extra) if isinstance(openapi_extra, Mapping) else {}

    responses = merged.setdefault("responses", {})
    if not isinstance(responses, dict):
        responses = {}
        merged["responses"] = responses

    response_200 = responses.setdefault("200", {})
    if not isinstance(response_200, dict):
        response_200 = {}
        responses["200"] = response_200

    content = response_200.setdefault("content", {})
    if not isinstance(content, dict):
        content = {}
        response_200["content"] = content

    app_json = content.setdefault("application/json", {})
    if not isinstance(app_json, dict):
        app_json = {}
        content["application/json"] = app_json

    app_json["schema"] = dict(schema)
    return merged
