"""Upgrade-floor spec (CollectionManagement D12, shared, both sources).

For an ``origin='upgrade'`` run the strategy resolves the held library tier into
``context.held_tier`` (album worst tier, or the recording's best tier for a
per-track upgrade); a candidate must STRICTLY beat it - equal or worse is never
an upgrade (D4). ``held_tier is None`` means this isn't an upgrade run (or the
target isn't held), so the spec passes.

An ``unknown``/empty candidate tier PASSES, mirroring ``quality_range``: Usenet
titles are often too noisy to read a codec from, and replace-on-import re-checks
strictly-better against the real file before anything on disk is touched - the
floor here only saves wasted bytes, it is not the safety guarantee.
"""

from models.download import TargetAlbum
from services.native.quality_tiers import is_tier, tier_rank

from ..context import DecisionContext
from ..decision import Accept, Candidate, Decision, Disposition, Reject, RejectCode, SpecPolicy


def upgrade_floor(
    candidate: Candidate,
    target: TargetAlbum,
    context: DecisionContext,
    policy: SpecPolicy,
) -> Decision:
    if context.held_tier is None:
        return Accept()
    if not is_tier(candidate.tier):
        return Accept()  # unknown tier: the import-side strictly-better guard decides
    if tier_rank(candidate.tier) > tier_rank(context.held_tier):
        return Accept()
    return Reject(
        code=RejectCode.NOT_AN_UPGRADE,
        detail=f"tier {candidate.tier!r} does not beat held {context.held_tier!r}",
        disposition=Disposition.PERMANENT,
    )
