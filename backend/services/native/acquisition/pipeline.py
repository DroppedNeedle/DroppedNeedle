"""The ordered spec pipeline.

``run`` applies the shared specs in order and returns the FIRST ``Reject``, else
``Accept``. Both scorers call this before computing their source-specific ranking
signals (blueprint step 1), so the shared identity/quality/blocklist decisions can
never drift between Soulseek and Usenet.

Order is Lidarr-style: blocklist → identity → quality (cheapest, most-decisive
first). Source-specific gates that aren't shared yet (Soulseek codec/no-audio,
Usenet password/edition/size/video) still live in their scorers and move to specs
in step 2.
"""

from models.download import TargetAlbum

from .context import DecisionContext
from .decision import Accept, Candidate, Decision, Reject, SpecPolicy
from .specs.free_space import free_space
from .specs.max_size import max_size
from .specs.min_age import min_age
from .specs.password import password
from .specs.quality_range import quality_range
from .specs.quarantine import quarantine
from .specs.retention import retention
from .specs.sample import sample
from .specs.terms import ignored_terms, required_terms
from .specs.upgrade_floor import upgrade_floor
from .specs.wrong_edition import wrong_edition
from .specs.wrong_album import wrong_album

# The ordered, source-agnostic spec list (Lidarr-style: cheap + decisive first -
# blocklist, then identity/product, then user policy, then quality/size/age/space).
# Specs whose config/context is off or unknown (terms empty, sizes 0, no usenet_date,
# free_bytes None) short-circuit to Accept, so an unconfigured install is unchanged.
SPECS = (
    quarantine,
    password,
    wrong_edition,
    wrong_album,
    sample,
    ignored_terms,
    required_terms,
    quality_range,
    upgrade_floor,
    max_size,
    retention,
    min_age,
    free_space,
)


def run(
    candidate: Candidate,
    target: TargetAlbum,
    context: DecisionContext,
    policy: SpecPolicy,
) -> Decision:
    for spec in SPECS:
        decision = spec(candidate, target, context, policy)
        if isinstance(decision, Reject):
            return decision
    return Accept()
