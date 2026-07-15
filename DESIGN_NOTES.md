# Request Backend Extensibility - Design Notes

## Motivation

DroppedNeedle currently has a monolithic acquisition pipeline tied directly to slskd/Usenet via `DownloadService`. This makes it difficult to:

1. Support alternative acquisition backends (e.g., Lidarr)
2. Test acquisition logic without a full download system
3. Route requests through different backends based on configuration

The request backend seam addresses this by introducing a **unified dispatch contract** that all acquisition flows must go through.

## Architecture

```
User/Admin Request
       ↓
RequestService / RequestsPageService
       ↓
RequestBackendService.dispatch_request()
       ↓
   ┌──────┴──────┐
   │             │
native       lidarr (future)
   │             │
   ↓             ↓
DownloadService  LidarrClient
```

### Key components

1. **`RequestBackendSettings`** (`backend/core/request_backend_settings.py`):
   - Pydantic model for `request_backend.backend` config
   - Default: `"native"` - no config change required

2. **`RequestBackendService`** (`backend/services/request_backend_service.py`):
   - Unified dispatcher with `dispatch_request()` method
   - Routes to backend based on config
   - Returns task IDs or sentinel values

3. **Dependency injection** (`backend/core/dependencies/backend_providers.py`):
   - `get_request_backend_settings()`: Singleton settings provider
   - `get_request_backend_service()`: Singleton service provider
   - Wired into `service_providers.py`

### Sentinel values

- `ALREADY_IN_LIBRARY`: Album already exists in the target backend
- `DISPATCH_FAILED`: Backend error (network, API, validation)

These match the native backend's error handling pattern.

## Integration points

### Updated request flow

**Before:**
```
RequestService.request_album()
  → DownloadService.request_album()
```

**After:**
```
RequestService.request_album()
  → RequestBackendService.dispatch_request()
    → DownloadService.request_album() (native)
    → LidarrClient (future)
```

Both user submission and admin approval/retry paths use the same seam, ensuring consistent behavior.

### Configuration example

```json
{
  "request_backend": {
    "backend": "native"
  }
}
```

## Future Lidarr backend: Safety patterns

### 1. Artist library check

**Pattern**: Always check if artist exists before mutation

```python
# First, check if artist exists in Lidarr
artist = await lidarr_client.get_artist_by_mbid(mbid)
if not artist:
    # Artist not in library - require manual review
    return "pending_manual_review"

# Only then proceed with monitoring changes
await lidarr_client.put_artist(artist["id"], monitored=True)
```

**Why**: Prevents uncontrolled library expansion. New artists should go through a review workflow.

### 2. Full-object update pattern

**Pattern**: GET → modify → PUT for all mutations

```python
# Step 1: GET full object
album = await lidarr_client.get_album(album_id)

# Step 2: Modify specific field
album["monitored"] = True

# Step 3: PUT entire modified object
await lidarr_client.put_album(album["id"], album)
```

**Why**: Lidarr API requires full objects for PUT to prevent partial update corruption.

### 3. AlbumSearch payload format

**Pattern**: Use `{"albumIds": [123, 456]}` array payload

```python
search_payload = {"albumIds": [album_id_1, album_id_2]}
result = await lidarr_client.post_album_search(search_payload)
```

**Why**: Lidarr's AlbumSearch command expects an array, not a single ID.

### 4. Preserve queued vs already-in-library semantics

**Pattern**: Distinguish return values clearly

```python
if album_already_exists:
    return ALREADY_IN_LIBRARY  # Sentinel value
elif backend_error:
    return DISPATCH_FAILED     # Sentinel value
else:
    return command_id          # Actual Lidarr command ID
```

**Why**: Matches native backend's error handling and prevents silent failures.

## Testing strategy

### 1. Dependency injection tests (`test_request_backend_di.py`)
- Verify DI wiring works correctly
- Test default settings (native backend)
- Test service creation with different backends

### 2. Contract tests (`test_lidarr_contract.py`)
- Validate the unified dispatch contract
- Test error propagation (ValidationError, Exception)
- Test sentinel value handling
- Test already-in-library scenario

### 3. Safety tests (`test_lidarr_v3_safety.py`)
- Document and validate all safety patterns
- Test artist validation before mutation
- Test full-object update pattern
- Test AlbumSearch payload format
- Test error handling consistency
- Test concurrent request safety
- Test command ID tracking

### Running tests

```bash
# All request backend tests
pytest backend/tests/test_request_backend_di.py
pytest backend/tests/test_lidarr_contract.py
pytest backend/tests/test_lidarr_v3_safety.py

# All tests (includes existing tests)
make test
```

## Migration path

### For existing deployments
- **Zero migration**: Default config uses `"native"` backend
- All existing behavior is preserved
- No database changes required

### For future Lidarr support
1. Configure `"backend": "lidarr"` in `config.json`
2. Implement `LidarrClient` with V3 API methods
3. Add Lidarr connection settings to config
4. Wire Lidarr backend into `RequestBackendService`
5. Add artist discovery/review UI (future PR)

## Open questions for maintainers

1. **Artist discovery**: Should non-library artists require manual approval before being added to Lidarr?
2. **Monitoring semantics**: Should requests automatically monitor albums, or require separate admin action?
3. **Error granularity**: Should Lidarr-specific errors be surfaced to users, or kept generic?
4. **Task ID mapping**: How should Lidarr command IDs be exposed in the request history?

## Related files

- `backend/core/request_backend_settings.py` - Config schema
- `backend/services/request_backend_service.py` - Dispatcher
- `backend/core/dependencies/backend_providers.py` - DI providers
- `backend/tests/test_request_backend_di.py` - DI tests
- `backend/tests/test_lidarr_contract.py` - Contract tests
- `backend/tests/test_lidarr_v3_safety.py` - Safety tests