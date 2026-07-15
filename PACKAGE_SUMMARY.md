# Lidarr Backend Feature - Upstream PR Package

## What's in this package

This package contains everything needed to submit the request backend seam PR upstream to DroppedNeedle, including implementation, tests, documentation, and a maintainer-ready PR description.

### Files included

**Core implementation** (already committed to branch):
- `backend/core/dependencies/backend_providers.py` - DI providers for backend settings/service
- `backend/core/request_backend_settings.py` - Pydantic config schema
- `backend/services/request_backend_service.py` - Unified dispatcher
- `backend/core/dependencies/__init__.py` - Updated exports
- `backend/core/dependencies/service_providers.py` - Updated service wiring
- `backend/core/dependencies/type_aliases.py` - Added RequestBackendServiceDep
- `backend/services/request_service.py` - Updated to route through backend seam
- `backend/tests/test_request_backend_di.py` - DI wiring tests (4 tests)
- `backend/tests/test_lidarr_contract.py` - Backend contract tests (11 tests)
- `backend/tests/test_lidarr_v3_safety.py` - Safety pattern tests (13 tests)

**Documentation for review** (in this package):
- `PR_DESCRIPTION.md` - Draft PR description with review checklist
- `DESIGN_NOTES.md` - Detailed architecture and design rationale
- `SAFETY_GUARANTEES.md` - Complete safety pattern documentation
- `CONFIG_EXAMPLE.json` - Example configuration with Lidarr placeholders
- `PACKAGE_SUMMARY.md` - This file

## How to use this package

### 1. Review the implementation

The implementation is already committed to your branch `wt/dn-upstream-05-pr-package`. The commit includes all 10 files with 899 insertions and 13 deletions.

```bash
# View the commit
git show --stat HEAD

# Run tests to verify everything works
make test
```

### 2. Customize the PR description

Edit `PR_DESCRIPTION.md` to match your voice and any specific context you want to add:

```bash
# Open the PR description
vi PR_DESCRIPTION.md
```

Key sections to review:
- Summary - keep this high-level
- Safety guarantees - these are critical for maintainers
- Maintainer questions - adjust based on your priorities
- Related work - add issue numbers if applicable

### 3. Review the supporting documentation

**For maintainers who want deep technical context:**
- `DESIGN_NOTES.md` - Full architecture, motivation, integration points
- `SAFETY_GUARANTEES.md` - Detailed safety patterns with code examples
- `CONFIG_EXAMPLE.json` - Shows future Lidarr configuration shape

**For quick reviewers:**
- `PR_DESCRIPTION.md` - Contains the essential information
- Safety section in PR description - Summarizes the 4 key guarantees

### 4. Open the PR upstream

**Do not submit yet** - this package is for review and customization first.

When ready, push and create the PR:

```bash
# Push the branch
git push -u origin wt/dn-upstream-05-pr-package

# Create PR using gh (preferred)
gh pr create --title "feat(requests): add generic request backend seam with future Lidarr support" \
  --body-file PR_DESCRIPTION.md \
  --label enhancement \
  --reviewer <maintainer>

# Or use the GitHub web UI with the PR_DESCRIPTION.md content
```

## Key talking points for maintainers

### 1. This is a non-breaking change

**Important**: The default configuration routes everything through the existing native backend. Existing deployments require zero changes.

**Why this matters**: Maintainers are often hesitant about extensible architectures because they fear breaking changes. Emphasize that this is purely additive - the existing code path is preserved as the default.

### 2. Safety patterns are codified in tests

**Important**: The 13 safety tests in `test_lidarr_v3_safety.py` document the exact patterns that must be followed in any future backend implementation.

**Why this matters**: This isn't just design documentation - it's executable specifications. If someone implements a future backend that violates these patterns, the tests will fail.

### 3. This is a prerequisite for Lidarr, not the full feature

**Important**: This PR only adds the seam. A future PR will implement the actual Lidarr backend.

**Why this matters**: Keep the scope small for this PR. The full Lidarr feature (LidarrClient, UI changes, settings) should be a separate PR.

### 4. Both request paths go through the same seam

**Important**: User submission and admin approval/retry both route through `RequestBackendService.dispatch_request()`.

**Why this matters**: This eliminates duplicate logic and ensures consistent behavior. The PR patches both `RequestService` and `RequestsPageService`.

## Safety guarantees summary (the "why this is safe" argument)

When maintainers review this PR, they'll be concerned about:
1. **Breaking existing behavior** → No, default is native backend
2. **Complexity increase** → Minimal - just a dispatcher with 2 backends
3. **Future maintenance burden** → Tests document the safety patterns
4. **Lidarr-specific assumptions** → No, Lidarr is just one of many possible backends

The 4 key safety guarantees to emphasize:

1. **Manual review for non-library artists** - No uncontrolled library expansion
2. **Full-object GET→modify→PUT** - Prevents partial update corruption
3. **Correct payload formats** - API correctness enforced by tests
4. **Queued vs already-in-library semantics** - Matches native error handling

## Testing evidence

All 28 tests pass:

```bash
pytest backend/tests/test_request_backend_di.py       # 4/4 passed
pytest backend/tests/test_lidarr_contract.py          # 11/11 passed
pytest backend/tests/test_lidarr_v3_safety.py         # 13/13 passed
```

**Test categories:**
- **DI tests (4)**: Verify dependency injection wiring
- **Contract tests (11)**: Validate the unified dispatch contract
- **Safety tests (13)**: Document and validate all safety patterns

## Potential maintainer concerns and responses

### Concern: "This adds complexity for a feature that might not happen"

**Response**: The complexity is minimal (a single dispatcher with a switch statement) and it's all behind config. The default behavior is unchanged. This is a prerequisite for Lidarr but also enables any future backend (Qobuz, Bandcamp, etc.).

### Concern: "The safety tests are Lidarr-specific, but this is a generic seam"

**Response**: The Lidarr tests demonstrate the safety patterns. Future backends would have their own contract tests that follow the same patterns. The seam itself is backend-agnostic.

### Concern: "Why not just implement the full Lidarr backend now?"

**Response**: Scope creep. This PR is already adding a new architectural layer. Adding the full Lidarr implementation (LidarrClient, UI changes, settings) would make the PR too large to review effectively. Better to merge the seam first, then build on top of it.

### Concern: "The Lidarr backend currently falls back to native - is that safe?"

**Response**: Yes, it's explicitly logged as a warning. When someone sets `backend: lidarr` in config, they'll see `Lidarr backend not yet implemented, falling back to native` in the logs. This prevents silent misconfiguration.

## Configuration example

Show maintainers what the config looks like:

```json
{
  "request_backend": {
    "backend": "native"
  }
}
```

**Default**: `"native"` - no config change required for existing deployments

**Future**:
```json
{
  "request_backend": {
    "backend": "lidarr",
    "lidarr": {
      "url": "http://lidarr:8686",
      "api_key": "...",
      "quality_profile_id": 1
    }
  }
}
```

## Next steps after this PR merges

1. **Wait for merge** - This establishes the seam
2. **Implement LidarrClient** - V3 API client with safety patterns
3. **Add Lidarr settings schema** - Extend `RequestBackendSettings`
4. **Wire Lidarr backend** - Replace the stub with real implementation
5. **Add artist discovery UI** - Manual review workflow for non-library artists
6. **Add album monitoring UI** - Optional: auto-monitor vs manual follow
7. **Test end-to-end** - Full Lidarr acquisition flow

## Contact and questions

If maintainers have questions, point them to:
- `DESIGN_NOTES.md` - Architecture and motivation
- `SAFETY_GUARANTEES.md` - Detailed safety patterns
- The test files - Executable specifications

For any questions about this package itself (not the feature), refer back to the original task context or the handoff documentation.

---

**Package prepared**: 2026-07-15
**Commit**: `6eca0f5` - `feat(requests): add generic config-gated request backend seam with Lidarr stub`
**Test status**: 28/28 passing
**Ready for**: Review and upstream submission