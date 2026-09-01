import logging
from fastapi import Request, HTTPException, status
from fastapi.exceptions import RequestValidationError
from starlette.exceptions import HTTPException as StarletteHTTPException
from starlette.responses import Response

from core.exceptions import (
    ResourceNotFoundError,
    ExternalServiceError,
    SourceResolutionError,
    ValidationError,
    ConfigurationError,
    ClientDisconnectedError,
    PermissionDeniedError,
    ConflictError,
    RevisionOverflowError,
    StaleRevisionError,
    AutomaticManagementHoldError,
)
from infrastructure.msgspec_fastapi import MsgSpecJSONResponse
from infrastructure.resilience.retry import CircuitOpenError
from models.error import (
    error_response,
    VALIDATION_ERROR,
    NOT_FOUND,
    EXTERNAL_SERVICE_UNAVAILABLE,
    CONFIGURATION_ERROR,
    SOURCE_RESOLUTION_ERROR,
    INTERNAL_ERROR,
    CIRCUIT_BREAKER_OPEN,
    FORBIDDEN,
    CONFLICT,
    REVISION_OVERFLOW,
    STALE_REVISION,
    AUTOMATIC_MANAGEMENT_HOLD,
    STATUS_TO_CODE,
)

logger = logging.getLogger(__name__)


async def resource_not_found_handler(
    request: Request, exc: ResourceNotFoundError
) -> MsgSpecJSONResponse:
    logger.warning(
        "Resource not found: %s - %s %s", exc, request.method, request.url.path
    )
    return error_response(status.HTTP_404_NOT_FOUND, NOT_FOUND, str(exc))


async def external_service_error_handler(
    request: Request, exc: ExternalServiceError
) -> MsgSpecJSONResponse:
    logger.error(
        "External service error: %s - %s %s", exc, request.method, request.url.path
    )
    return error_response(
        status.HTTP_503_SERVICE_UNAVAILABLE,
        EXTERNAL_SERVICE_UNAVAILABLE,
        "External service unavailable",
    )


async def circuit_open_error_handler(
    request: Request, exc: CircuitOpenError
) -> MsgSpecJSONResponse:
    logger.error(
        "Circuit breaker open: %s - %s %s", exc, request.method, request.url.path
    )
    name = (
        exc.breaker_name.replace("_", " ").title()
        if getattr(exc, "breaker_name", "")
        else "Service"
    )
    return error_response(
        status.HTTP_503_SERVICE_UNAVAILABLE,
        CIRCUIT_BREAKER_OPEN,
        f"{name} is temporarily unavailable due to repeated connection failures. Check your settings or wait for the service to recover.",
    )


async def validation_error_handler(
    request: Request, exc: ValidationError
) -> MsgSpecJSONResponse:
    logger.warning(
        "Validation error: %s - %s %s", exc, request.method, request.url.path
    )
    return error_response(
        status.HTTP_400_BAD_REQUEST,
        getattr(exc, "error_code", VALIDATION_ERROR),
        str(exc),
    )


async def configuration_error_handler(
    request: Request, exc: ConfigurationError
) -> MsgSpecJSONResponse:
    logger.warning(
        "Configuration error: %s - %s %s", exc, request.method, request.url.path
    )
    return error_response(status.HTTP_400_BAD_REQUEST, CONFIGURATION_ERROR, str(exc))


async def permission_denied_handler(
    request: Request, exc: PermissionDeniedError
) -> MsgSpecJSONResponse:
    logger.warning(
        "Permission denied: %s - %s %s", exc, request.method, request.url.path
    )
    return error_response(status.HTTP_403_FORBIDDEN, FORBIDDEN, str(exc))


async def conflict_error_handler(
    request: Request, exc: ConflictError
) -> MsgSpecJSONResponse:
    logger.warning("Conflict: %s - %s %s", exc, request.method, request.url.path)
    return error_response(
        status.HTTP_409_CONFLICT,
        getattr(exc, "error_code", CONFLICT),
        str(exc),
    )


async def automatic_management_hold_handler(
    request: Request, exc: AutomaticManagementHoldError
) -> MsgSpecJSONResponse:
    logger.warning(
        "Automatic management hold (%s) - %s %s",
        exc.reason_code,
        request.method,
        request.url.path,
    )
    return error_response(
        status.HTTP_409_CONFLICT,
        AUTOMATIC_MANAGEMENT_HOLD,
        "Import blocked by a Library Management safety check.",
        details={"reason_code": exc.reason_code},
    )


async def stale_revision_error_handler(
    request: Request, exc: StaleRevisionError
) -> MsgSpecJSONResponse:
    logger.warning("Stale revision: %s - %s %s", exc, request.method, request.url.path)
    return error_response(status.HTTP_409_CONFLICT, STALE_REVISION, str(exc))


async def revision_overflow_error_handler(
    request: Request, exc: RevisionOverflowError
) -> MsgSpecJSONResponse:
    logger.error("Revision overflow: %s - %s %s", exc, request.method, request.url.path)
    return error_response(
        status.HTTP_500_INTERNAL_SERVER_ERROR,
        REVISION_OVERFLOW,
        "A library revision could not be advanced.",
    )


async def source_resolution_error_handler(
    request: Request, exc: SourceResolutionError
) -> MsgSpecJSONResponse:
    logger.warning(
        "Source resolution error: %s - %s %s", exc, request.method, request.url.path
    )
    return error_response(
        status.HTTP_422_UNPROCESSABLE_ENTITY, SOURCE_RESOLUTION_ERROR, str(exc)
    )


async def general_exception_handler(
    request: Request, exc: Exception
) -> MsgSpecJSONResponse:
    logger.exception(
        "Unexpected error: %s - %s %s", exc, request.method, request.url.path
    )
    return error_response(
        status.HTTP_500_INTERNAL_SERVER_ERROR, INTERNAL_ERROR, "Internal server error"
    )


async def http_exception_handler(
    request: Request, exc: HTTPException
) -> MsgSpecJSONResponse:
    code = STATUS_TO_CODE.get(exc.status_code, INTERNAL_ERROR)
    message = exc.detail if isinstance(exc.detail, str) else "Request failed"
    return error_response(exc.status_code, code, message)


async def starlette_http_exception_handler(
    request: Request, exc: StarletteHTTPException
) -> MsgSpecJSONResponse:
    code = STATUS_TO_CODE.get(exc.status_code, INTERNAL_ERROR)
    message = exc.detail if isinstance(exc.detail, str) else "Request failed"
    return error_response(exc.status_code, code, message)


async def request_validation_error_handler(
    request: Request, exc: RequestValidationError
) -> MsgSpecJSONResponse:
    logger.warning("Request validation error: %s %s", request.method, request.url.path)
    clean_errors = [
        {k: v for k, v in err.items() if k != "ctx"} for err in exc.errors()
    ]
    return error_response(
        status.HTTP_422_UNPROCESSABLE_ENTITY,
        VALIDATION_ERROR,
        "Validation failed",
        details=clean_errors,
    )


async def client_disconnected_handler(
    request: Request, exc: ClientDisconnectedError
) -> Response:
    return Response(status_code=status.HTTP_204_NO_CONTENT)
