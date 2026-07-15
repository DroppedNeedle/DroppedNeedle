# Safety Guarantees for Request Backend Extensibility

## Overview

This document summarizes the critical safety patterns that must be preserved in any backend implementation (native, Lidarr, or future backends). These patterns are codified in the test suite (`test_lidarr_contract.py` and `test_lidarr_v3_safety.py`) and should be treated as non-negotiable requirements.

## Core safety contract

### 1. Artist library verification before mutation

**Rule**: Any artist not already in the target backend's library must go through a manual review/approval workflow before any POST/PUT/command mutations occur.

**Why**: Prevents uncontrolled library expansion and ensures admins maintain control over which artists are added to their acquisition system.

**Implementation pattern**:
```python
artist = await backend_client.get_artist_by_mbid(mbid)
if not artist:
    # Artist not in library - require manual review
    create_pending_artist_request(mbid, artist_name)
    return "pending_manual_review"

# Only proceed with monitoring changes after artist is approved
await backend_client.put_artist(artist["id"], monitored=True)
```

**Test coverage**: `test_lidarr_v3_safety.py::test_non_library_artist_requires_manual_review`

---

### 2. Full-object GET→modify→PUT pattern for mutations

**Rule**: All mutations (POST/PUT) must first GET the full object, modify the specific field, then PUT the entire modified object back. Partial updates are prohibited.

**Why**: The Lidarr API (and similar systems) require full objects to prevent partial update corruption. Missing fields can cause data loss or system instability.

**Implementation pattern**:
```python
# Step 1: GET full object
album = await backend_client.get_album(album_id)
original_album = album.copy()

# Step 2: Modify specific field only
album["monitored"] = True

# Step 3: PUT entire modified object
await backend_client.put_album(album["id"], album)

# Verify: all original fields are preserved
assert album["id"] == original_album["id"]
assert album["title"] == original_album["title"]
assert album["foreignAlbumId"] == original_album["foreignAlbumId"]
# Only 'monitored' changed
assert album["monitored"] != original_album["monitored"]
```

**Test coverage**: 
- `test_lidarr_v3_safety.py::test_album_full_object_pattern`
- `test_lidarr_v3_safety.py::test_monitoring_flag_safety`

---

### 3. Correct payload formats for backend-specific commands

**Rule**: Use the exact payload format required by the backend's API. For Lidarr AlbumSearch, this means `{"albumIds": [123, 456]}` array format.

**Why**: Backend APIs are strict about payload formats. Incorrect formats cause silent failures or API rejections.

**Implementation pattern**:
```python
# CORRECT: AlbumSearch with array payload
search_payload = {"albumIds": [album_id_1, album_id_2]}
result = await backend_client.post_album_search(search_payload)

# INCORRECT: Single ID or wrong format
# search_payload = {"albumIds": 123}  # Wrong - must be array
# search_payload = {"albumId": 123}   # Wrong - wrong key name
```

**Test coverage**: `test_lidarr_v3_safety.py::test_album_search_payload_format`

---

### 4. Preserve queued vs already-in-library semantics

**Rule**: The backend must clearly distinguish between three outcomes:
1. Album already exists in library → return `ALREADY_IN_LIBRARY` sentinel
2. Backend error occurred → return `DISPATCH_FAILED` sentinel
3. Successfully queued for acquisition → return actual task/command ID

**Why**: This matches the native backend's error handling pattern and prevents silent failures. Users should know whether their request was queued, skipped, or failed.

**Implementation pattern**:
```python
# Check if album already exists
if await backend_client.album_exists_in_library(mbid):
    return ALREADY_IN_LIBRARY  # Sentinel value

# Attempt to queue for acquisition
try:
    result = await backend_client.post_album_search({"albumIds": [album_id]})
    return result["id"]  # Actual command ID
except NetworkError as e:
    logger.error("Backend dispatch failed: %s", e)
    return DISPATCH_FAILED  # Sentinel value
```

**Test coverage**: 
- `test_lidarr_contract.py::test_lidarr_backend_already_in_library`
- `test_lidarr_contract.py::test_lidarr_backend_dispatch_failure_returns_sentinel`

---

### 5. Validation before any mutation

**Rule**: All validation (artist exists, album format, quotas, caps) must occur **before** any POST/PUT/DELETE mutations. Failed validation should never result in partial state changes.

**Why**: Prevents "leaky" state where validation fails but some mutations have already occurred.

**Implementation pattern**:
```python
# Step 1: Validate artist exists (read-only)
artist = await backend_client.get_artist_by_mbid(mbid)
if not artist:
    raise ValidationError("Artist not found in library")

# Step 2: Validate user quota (read-only)
if exceeds_user_quota(user_id):
    raise ValidationError("User quota exceeded")

# Step 3: Only then proceed with mutations (write operations)
await backend_client.put_artist(artist["id"], monitored=True)
```

**Test coverage**: `test_lidarr_v3_safety.py::test_validation_before_any_mutation`

---

### 6. Error handling preserves consistency

**Rule**: When backend errors occur, partial state must not be committed. If a mutation fails, the system should remain in its previous state.

**Why**: Prevents corrupted backend state from inconsistent updates.

**Implementation pattern**:
```python
# Get current state
artist = await backend_client.get_artist(artist_id)
original_monitored = artist["monitored"]

# Attempt mutation
try:
    artist["monitored"] = True
    await backend_client.put_artist(artist["id"], artist)
except Exception as e:
    # On failure, verify original state is preserved
    artist_after = await backend_client.get_artist(artist_id)
    assert artist_after["monitored"] == original_monitored
    raise  # Re-raise error for caller to handle
```

**Test coverage**: `test_lidarr_v3_safety.py::test_error_handling_preserves_consistency`

---

### 7. No reliance on native duplicate-task semantics

**Rule**: Backend implementations should not rely on the native backend's duplicate task detection. Each backend must handle its own deduplication semantics.

**Why**: Different backends have different deduplication strategies (Lidarr uses album IDs, native uses MusicBrainz IDs + user ID). Mixing these causes missed duplicates or false positives.

**Implementation pattern**:
```python
# Native backend: checks MusicBrainz ID + user ID
if native_duplicate_exists(mbid, user_id):
    return ALREADY_IN_LIBRARY

# Lidarr backend: checks Lidarr album ID
if lidarr_album_exists(lidarr_album_id):
    return ALREADY_IN_LIBRARY

# These are separate checks - don't mix them
```

**Test coverage**: `test_lidarr_v3_safety.py::test_no_native_duplicate_semantics`

---

## Sentinel value contract

The `RequestBackendService` defines two sentinel values that all backends must respect:

```python
ALREADY_IN_LIBRARY = "already_in_library"
DISPATCH_FAILED = "dispatch_failed"
```

### When to return `ALREADY_IN_LIBRARY`
- The album already exists in the backend's library
- The request is valid but no acquisition is needed
- This is **not** an error - it's a normal outcome

### When to return `DISPATCH_FAILED`
- Network errors (timeout, connection refused)
- API errors (500, 503, rate limiting)
- Authentication errors (invalid API key)
- Configuration errors (wrong URL, missing settings)

### When to return an actual task ID
- Successfully queued for acquisition
- Return the backend's task/command identifier (string or int)
- This should be queryable for status

---

## Dependency injection safety

The backend is wired through dependency injection with singleton providers:

```python
def get_request_backend_service() -> RequestBackendService:
    # Singleton - created once per application lifetime
    settings = get_request_backend_settings()
    download_service = get_download_service()
    return RequestBackendService(download_service, settings)
```

**Safety implications**:
- Backend type is fixed at startup (cannot change mid-request)
- Settings are validated once at startup via Pydantic
- Thread-safe due to singleton pattern

---

## Testing coverage matrix

| Safety Pattern | Test File | Test Method |
|----------------|-----------|-------------|
| Artist library verification | test_lidarr_v3_safety.py | test_non_library_artist_requires_manual_review |
| Full-object update pattern | test_lidarr_v3_safety.py | test_album_full_object_pattern |
| AlbumSearch payload format | test_lidarr_v3_safety.py | test_album_search_payload_format |
| Queued vs already-in-library | test_lidarr_contract.py | test_lidarr_backend_already_in_library |
| Validation before mutation | test_lidarr_v3_safety.py | test_validation_before_any_mutation |
| Error handling consistency | test_lidarr_v3_safety.py | test_error_handling_preserves_consistency |
| No native duplicate semantics | test_lidarr_v3_safety.py | test_no_native_duplicate_semantics |
| Concurrent request safety | test_lidarr_v3_safety.py | test_concurrent_request_handling |
| Command ID tracking | test_lidarr_v3_safety.py | test_command_id_tracking |

**Total**: 28 tests passing (4 DI + 11 contract + 13 safety)

---

## Implementation checklist for future backends

Before any backend implementation can be merged, verify:

- [ ] Artist library verification before mutations
- [ ] Full-object GET→modify→PUT pattern for all mutations
- [ ] Correct payload formats for all backend commands
- [ ] Clear distinction between queued/already-in-library/failed
- [ ] Validation occurs before any mutations
- [ ] Error handling preserves consistency
- [ ] No reliance on native duplicate semantics
- [ ] Sentinel values are returned correctly
- [ ] All safety tests pass
- [ ] Backwards compatibility with native backend
- [ ] Configuration schema is valid (Pydantic)
- [ ] Error messages are clear and actionable

---

## Remaining questions for maintainers

1. **Artist discovery UI**: When a non-library artist is requested, should we:
   - Auto-create a pending artist record?
   - Require manual admin approval before adding to Lidarr?
   - Allow users to submit artist requests separately from album requests?

2. **Monitoring semantics**: Should requests automatically monitor albums in Lidarr, or require a separate "follow" action from admins?

3. **Error granularity**: When Lidarr API calls fail, should we surface:
   - Generic "dispatch failed" to users?
   - Detailed Lidarr-specific errors?
   - Categorized failures (auth, network, validation)?

4. **Task ID exposure**: How should Lidarr command IDs be exposed:
   - Stored as the request's task_id?
   - Mapped internally and shown on request details page?
   - Queryable via API for status tracking?

These decisions should be made before the full Lidarr backend implementation begins.