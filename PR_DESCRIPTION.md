# PR: Add generic request backend seam with future Lidarr support

## Summary

This PR introduces a **config-gated request backend seam** that allows album acquisition requests to be routed through different backends (native slskd, future external services like Lidarr) via a unified dispatch contract. The native backend preserves all existing slskd-based acquisition behavior, while the configuration enables extensibility without requiring code changes.

This is a **non-breaking change** - the default configuration routes all requests through the existing native backend, maintaining full backwards compatibility.

## What this changes

### Core seam implementation
- **`RequestBackendSettings`**: New Pydantic config schema for `request_backend.backend` (default: "native")
- **`RequestBackendService`**: Unified dispatcher with consistent `dispatch_request()` contract
- **Sentinel values**: `ALREADY_IN_LIBRARY` and `DISPATCH_FAILED` for special dispatch outcomes
- **Dependency injection**: Added backend providers to `core/dependencies/` with singleton pattern

### Updated request flow
Both user submission and admin approval/retry paths now route through the shared `RequestBackendService`:
- `RequestService.request_album()` → `dispatch_request()` 
- `RequestService.retry_request()` → `dispatch_request()`
- `RequestsPageService.approve_request()` → `dispatch_request()`

This ensures consistent behavior across all acquisition paths and eliminates potential for duplicate tasks.

### Future Lidarr backend stub
The "lidarr" backend type is defined in the config schema but currently falls back to native with a warning. This provides the shape for a future implementation without requiring any Lidarr-specific code in this PR.

## Safety guarantees for future Lidarr backend

The implementation and contract tests document the critical safety patterns that **must** be followed in any Lidarr backend implementation:

### 1. Non-library artists: manual review before mutation
Artists not already in the library must go through a pending/manual review state before any POST/PUT/command mutations occur. This prevents unsupervised library expansion.

### 2. Album monitoring: full-object GET→modify→PUT pattern
All album monitoring changes must:
1. GET the full album object from Lidarr
2. Modify only the `monitored` field
3. PUT the entire modified object back

This prevents partial updates that could corrupt Lidarr's state.

### 3. AlbumSearch command: `albumIds[]` array payload
The Lidarr AlbumSearch command must use the `{"albumIds": [123, 456]}` payload format, not a single ID.

### 4. No collapse of queued vs already-in-library semantics
The backend must maintain the distinction between:
- `ALREADY_IN_LIBRARY` (album already exists in Lidarr)
- `DISPATCH_FAILED` (API error, network failure, etc.)
- Actual task ID (queued for download)

This matches the native backend's sentinel pattern and prevents silent failures.

## Test coverage

All tests pass (28 total):

### Dependency injection (4 tests)
- `test_request_backend_di.py`: Verifies DI wiring, default native backend, and service creation

### Backend contract (11 tests)  
- `test_lidarr_contract.py`: Validates the unified dispatch contract, error propagation, and sentinel handling

### Safety patterns (13 tests)
- `test_lidarr_v3_safety.py`: Documents and validates all safety patterns (artist validation, full-object updates, payload format, error handling, concurrent requests)

Run tests:
```bash
make test
# Or specifically:
pytest backend/tests/test_request_backend_di.py
pytest backend/tests/test_lidarr_contract.py  
pytest backend/tests/test_lidarr_v3_safety.py
```

## Configuration

The backend is controlled via `config.json`:

```json
{
  "request_backend": {
    "backend": "native"
  }
}
```

Available backends:
- `"native"`: Default - routes through existing DownloadService (slskd/Usenet)
- `"lidarr"`: Future - reserved for Lidarr integration (currently falls back to native with warning)

**No configuration change is required** for existing deployments - the default `"native"` backend preserves all current behavior.

## Maintainer questions before full Lidarr implementation

1. **Artist discovery flow**: When a non-library artist is requested, should we:
   - Create a pending artist record immediately?
   - Require manual approval before adding to Lidarr?
   - Auto-add with monitoring disabled, then require approval to enable?

2. **Album monitoring semantics**: Should requests automatically monitor albums in Lidarr, or require a separate "follow" action from admins?

3. **Error handling granularity**: When Lidarr API calls fail, should we surface:
   - Generic "dispatch failed" to users?
   - Detailed Lidarr-specific errors?
   - Categorize failures (auth, network, validation, etc.)?

4. **Task ID mapping**: How should Lidarr command IDs be exposed in the request history?
   - Store Lidarr command ID as the task ID?
   - Maintain an internal mapping between request IDs and Lidarr commands?
   - Query Lidarr for command status on demand?

## Review checklist

- [ ] The seam implementation is sound and non-breaking
- [ ] All 28 tests pass
- [ ] The safety pattern documentation is clear
- [ ] The Lidarr stub falls back to native safely
- [ ] Configuration schema is appropriate
- [ ] No unintended behavioral changes in native mode

## Related work

This PR is the foundation for the full Lidarr backend feature. Future PRs will:
- Implement the Lidarr V3 API client
- Add artist discovery and manual review workflow
- Implement album monitoring and search dispatch
- Add Lidarr-specific error handling and status tracking

Closes: (related issue if applicable)