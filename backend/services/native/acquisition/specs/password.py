"""Password / encrypted spec (Usenet).

A password-protected NZB can't be auto-unpacked by SABnzbd — it would download, fail
to extract, and blocklist — so reject it at grab time (Lidarr/Prowlarr). Soulseek
candidates carry ``password=0`` so the spec passes. Lifted from the inline Usenet
check (step 1) into a shared spec (step 2).
"""

from models.download import TargetAlbum

from ..context import DecisionContext
from ..decision import Accept, Candidate, Decision, Disposition, Reject, RejectCode, SpecPolicy


def password(
    candidate: Candidate,
    target: TargetAlbum,
    context: DecisionContext,
    policy: SpecPolicy,
) -> Decision:
    if candidate.password > 0:
        return Reject(
            code=RejectCode.PASSWORD_PROTECTED,
            detail="password-protected NZB cannot be auto-unpacked",
            disposition=Disposition.PERMANENT,
        )
    return Accept()
