"""Taste Graph — self-contained discovery from the user's OWN signals only.

Seeds are weighted from this user's library (artists + album counts), followed
artists, and recent play history (followed > recently played > library
presence). The top seeds are expanded through canonical MusicBrainz metadata
only: artist relationships (band members, collaborations, tributes), shared
labels, and shared tags. HARD RULE (by design): no ListenBrainz/Last.fm charts,
no sitewide/global popularity, no other users' listens anywhere in this path.

MB call budget per build: <= _MAX_SEEDS expansion lookups + _MAX_LABELS label
browses + _MAX_TAGS tag searches (~14 worst case), every one 24h-cached inside
the MusicBrainz repository, and the assembled graph itself is cached per user
for 24h — so steady state is zero MB calls.
"""

from __future__ import annotations

import asyncio
import logging
import math
from datetime import datetime, timezone

from api.v1.schemas.taste_graph import (
    TasteGraphItem,
    TasteGraphReason,
    TasteGraphResponse,
    TasteGraphSeed,
)
from infrastructure.cache.cache_keys import TASTE_GRAPH_PREFIX
from services.discover.genre_balance import (
    EMPTY_GENRE_PREFS,
    GenrePrefs,
    balanced_seed_selection,
    genre_family,
)

logger = logging.getLogger(__name__)

_MAX_SEEDS = 10
_MAX_LABELS = 2
_MAX_TAGS = 2
_MAX_ITEMS = 30
_MAX_PER_SEED = 3  # diversity: max candidates attributed to one seed
_GENRE_CAP_RATIO = 0.4  # diversity: max 40% of items sharing one genre
_GRAPH_TTL_SECONDS = 86400
# on a completely cold MB cache the expansion is rate-limited (~1 req/s); bound the
# request so the endpoint stays responsive and the next request reuses the MB-layer
# cache entries that did land.
_BUILD_BUDGET_SECONDS = 60

_RECENT_PLAYS_LIMIT = 200

# signal weights: followed > recently played > library presence
_FOLLOW_WEIGHT = 3.0
_RECENT_PLAY_WEIGHT = 0.2  # per play, capped
_RECENT_PLAY_MAX = 2.0
_LIBRARY_BASE_WEIGHT = 1.0
_LIBRARY_ALBUM_WEIGHT = 0.25  # per owned album, capped at 8

# relationship strength: member/collab > label-mate > shared tag
_MEMBER_STRENGTH = 1.0
_COLLAB_STRENGTH = 0.9
_LABEL_STRENGTH = 0.7
_SCENE_STRENGTH = 0.5

_MEMBER_REL_TYPES = {"member of band", "founder", "subgroup"}
_COLLAB_REL_TYPES = {
    "collaboration",
    "is person",
    "tribute",
    "supporting musician",
    "instrumental supporting musician",
    "vocal supporting musician",
    "voice actor",
}
_ARTIST_LABEL_REL_TYPES = {"recording contract", "label founder", "publishing"}

# never recommend these placeholder artists
_FILTERED_CANDIDATE_NAMES = {"various artists", "[unknown]", "/v/"}


class _Seed:
    __slots__ = ("mbid", "name", "weight", "norm_weight", "top_tag")

    def __init__(self, mbid: str, name: str, weight: float):
        self.mbid = mbid
        self.name = name
        self.weight = weight
        self.norm_weight = 1.0
        self.top_tag: str = ""


class _Candidate:
    __slots__ = ("kind", "mbid", "name", "artist_mbid", "artist_name", "score", "reasons", "genre")

    def __init__(self, kind, mbid, name, artist_mbid, artist_name, score, reason, genre):
        self.kind = kind
        self.mbid = mbid
        self.name = name
        self.artist_mbid = artist_mbid
        self.artist_name = artist_name
        self.score = score
        self.reasons: list[TasteGraphReason] = [reason]
        self.genre = genre


class TasteGraphService:
    def __init__(
        self,
        library_db,
        follow_store,
        play_history_store,
        mb_repo,
        cache,
        genre_index=None,
        genre_prefs_store=None,
    ) -> None:
        self._library_db = library_db
        self._follow_store = follow_store
        self._play_history = play_history_store
        self._mb_repo = mb_repo
        self._cache = cache
        # optional: library genre index for breadth-based seed balancing, and the
        # user's genre reduce/mute levels; both degrade to the old behaviour when absent
        self._genre_index = genre_index
        self._genre_prefs_store = genre_prefs_store

    async def get_taste_graph(self, user_id: str) -> TasteGraphResponse:
        cache_key = f"{TASTE_GRAPH_PREFIX}{user_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        response = await self._build(user_id)
        # a timed-out/failed expansion returns seeds with no items — don't pin
        # that for 24h, let the next request retry on the warmed MB cache
        if response.cold_start or response.items:
            await self._cache.set(cache_key, response, ttl_seconds=_GRAPH_TTL_SECONDS)
        return response

    async def _build(self, user_id: str) -> TasteGraphResponse:
        generated_at = datetime.now(timezone.utc).isoformat()
        prefs = await self._load_genre_prefs(user_id)
        seeds, owned_artist_mbids, owned_album_mbids, owned_artist_names = (
            await self._collect_seeds(user_id, prefs)
        )
        if not seeds:
            return TasteGraphResponse(
                cold_start=True, generated_at=generated_at, seeds=[], items=[]
            )

        try:
            items = await asyncio.wait_for(
                self._expand(
                    seeds, owned_artist_mbids, owned_album_mbids, owned_artist_names, prefs
                ),
                timeout=_BUILD_BUDGET_SECONDS,
            )
        except asyncio.TimeoutError:
            logger.warning("Taste graph expansion exceeded its budget for user %s", user_id[:8])
            items = []
        except Exception:  # noqa: BLE001 - the endpoint must degrade, not 500
            logger.error("Taste graph expansion failed for user %s", user_id[:8], exc_info=True)
            items = []

        return TasteGraphResponse(
            cold_start=False,
            generated_at=generated_at,
            seeds=[
                TasteGraphSeed(artist_mbid=s.mbid, name=s.name, weight=round(s.norm_weight, 4))
                for s in seeds
            ],
            items=items,
        )

    # ------------------------------------------------------------------ seeds

    async def _load_genre_prefs(self, user_id: str) -> GenrePrefs:
        if self._genre_prefs_store is None:
            return EMPTY_GENRE_PREFS
        try:
            return GenrePrefs(await self._genre_prefs_store.get_levels(user_id))
        except Exception:  # noqa: BLE001 - prefs must never break the graph
            return EMPTY_GENRE_PREFS

    async def _collect_seeds(
        self, user_id: str, prefs: GenrePrefs = EMPTY_GENRE_PREFS
    ) -> tuple[list[_Seed], set[str], set[str], set[str]]:
        artists, albums_for_matching, album_mbids, followed, recent = await asyncio.gather(
            self._library_db.get_artists(),
            self._library_db.get_all_albums_for_matching(),
            self._library_db.get_all_album_mbids(),
            self._follow_store.list_followed_artists(user_id),
            self._play_history.recent(user_id, limit=_RECENT_PLAYS_LIMIT),
        )

        owned_artist_mbids = {
            str(a.get("mbid", "")).lower() for a in artists if a.get("mbid")
        }
        owned_artist_names = {
            str(a.get("name", "")).lower() for a in artists if a.get("name")
        }
        owned_album_mbids = {m.lower() for m in album_mbids}

        album_counts: dict[str, int] = {}
        for _title, _artist_name, _album_mbid, artist_mbid in albums_for_matching:
            key = artist_mbid.lower()
            if key:
                album_counts[key] = album_counts.get(key, 0) + 1

        name_to_mbid = {
            str(a.get("name", "")).lower(): str(a.get("mbid", ""))
            for a in artists
            if a.get("name") and a.get("mbid")
        }
        play_counts: dict[str, int] = {}
        for record in recent:
            mbid = name_to_mbid.get((record.artist_name or "").lower())
            if mbid:
                play_counts[mbid.lower()] = play_counts.get(mbid.lower(), 0) + 1

        weights: dict[str, float] = {}
        names: dict[str, str] = {}

        for a in artists:
            mbid = str(a.get("mbid", "")).lower()
            if not mbid:
                continue
            names[mbid] = str(a.get("name", "")) or mbid
            weights[mbid] = _LIBRARY_BASE_WEIGHT + _LIBRARY_ALBUM_WEIGHT * min(
                album_counts.get(mbid, 0), 8
            )

        for mbid, plays in play_counts.items():
            weights[mbid] = weights.get(mbid, 0.0) + min(
                plays * _RECENT_PLAY_WEIGHT, _RECENT_PLAY_MAX
            )

        for f in followed:
            mbid = f.artist_mbid.lower()
            names.setdefault(mbid, f.artist_name)
            weights[mbid] = weights.get(mbid, 0.0) + _FOLLOW_WEIGHT

        if not weights:
            return [], owned_artist_mbids, owned_album_mbids, owned_artist_names

        candidates = sorted(weights.items(), key=lambda kv: kv[1], reverse=True)
        top = await self._balance_seed_candidates(candidates, prefs)
        if not top:
            return [], owned_artist_mbids, owned_album_mbids, owned_artist_names
        max_weight = max(w for _, w in top) or 1.0
        seeds = [_Seed(mbid, names.get(mbid, mbid), weight) for mbid, weight in top]
        for s in seeds:
            s.norm_weight = s.weight / max_weight
        return seeds, owned_artist_mbids, owned_album_mbids, owned_artist_names

    async def _balance_seed_candidates(
        self, candidates: list[tuple[str, float]], prefs: GenrePrefs
    ) -> list[tuple[str, float]]:
        """Breadth-based seeding: instead of the raw top-N by weight (which a
        genre-skewed library dominates), spread the _MAX_SEEDS picks round-robin
        across genre FAMILIES, sqrt-damped, from a 3x-deep candidate pool. The
        user's prefs apply first: muted families are dropped, reduced families'
        weights halved. Without a genre index this stays the plain top-N."""
        pool = candidates[: _MAX_SEEDS * 3]
        genres: dict[str, list[str]] = {}
        if self._genre_index is not None and pool:
            try:
                genres = await self._genre_index.get_genres_for_artists(
                    [mbid for mbid, _ in pool]
                )
            except Exception:  # noqa: BLE001 - genre data is best-effort
                genres = {}

        if not prefs.is_empty():
            adjusted: list[tuple[str, float]] = []
            for mbid, weight in pool:
                families = {genre_family(g) for g in genres.get(mbid, [])} - {""}
                if families and all(prefs.is_muted(f) for f in families):
                    continue
                live = [prefs.multiplier(f) for f in families if not prefs.is_muted(f)]
                adjusted.append((mbid, weight * (min(live) if live else 1.0)))
            adjusted.sort(key=lambda kv: kv[1], reverse=True)
            pool = adjusted

        if not genres:
            return pool[:_MAX_SEEDS]
        selected = balanced_seed_selection(
            pool, lambda kv: genres.get(kv[0], []), _MAX_SEEDS, prefs
        )
        # keep the seed list weight-ordered so norm_weight scoring stays intuitive
        selected.sort(key=lambda kv: kv[1], reverse=True)
        return selected

    # -------------------------------------------------------------- expansion

    async def _expand(
        self,
        seeds: list[_Seed],
        owned_artist_mbids: set[str],
        owned_album_mbids: set[str],
        owned_artist_names: set[str],
        prefs: GenrePrefs = EMPTY_GENRE_PREFS,
    ) -> list[TasteGraphItem]:
        expansions = await asyncio.gather(
            *(self._mb_repo.get_artist_expansion(s.mbid) for s in seeds),
            return_exceptions=True,
        )

        candidates: dict[str, _Candidate] = {}
        per_seed_count: dict[str, int] = {}
        # label_mbid -> (label_name, best seed)
        labels: dict[str, tuple[str, _Seed]] = {}
        # tag -> best seed
        tags: dict[str, tuple[float, _Seed]] = {}

        seed_mbids = {s.mbid.lower() for s in seeds}

        for seed, expansion in zip(seeds, expansions):
            if not isinstance(expansion, dict):
                continue
            seed_tags = sorted(
                expansion.get("tags") or [],
                key=lambda t: int(t.get("count", 0) or 0),
                reverse=True,
            )
            seed.top_tag = str(seed_tags[0].get("name", "")).lower() if seed_tags else ""
            for tag in seed_tags[:3]:
                name = str(tag.get("name", "")).lower()
                if not name:
                    continue
                score = seed.norm_weight * (1 + int(tag.get("count", 0) or 0))
                if name not in tags or score > tags[name][0]:
                    tags[name] = (score, seed)

            for rel in expansion.get("relations") or []:
                rel_type = str(rel.get("type", "")).lower()
                target = rel.get("artist")
                if isinstance(target, dict) and target.get("id"):
                    self._consider_related_artist(
                        seed, rel_type, target, candidates, per_seed_count,
                        owned_artist_mbids, seed_mbids,
                    )
                    continue
                label = rel.get("label")
                if (
                    isinstance(label, dict)
                    and label.get("id")
                    and rel_type in _ARTIST_LABEL_REL_TYPES
                ):
                    label_id = str(label["id"])
                    if label_id not in labels or seed.norm_weight > labels[label_id][1].norm_weight:
                        labels[label_id] = (str(label.get("name", "")), seed)

        top_labels = sorted(
            labels.items(), key=lambda kv: kv[1][1].norm_weight, reverse=True
        )[:_MAX_LABELS]
        top_tags = sorted(tags.items(), key=lambda kv: kv[1][0], reverse=True)[:_MAX_TAGS]

        label_results, tag_results = await asyncio.gather(
            asyncio.gather(
                *(self._mb_repo.get_label_releases(label_id) for label_id, _ in top_labels),
                return_exceptions=True,
            ),
            asyncio.gather(
                *(self._mb_repo.search_release_groups_by_tag(tag, limit=15) for tag, _ in top_tags),
                return_exceptions=True,
            ),
        )

        for (label_id, (label_name, seed)), releases in zip(top_labels, label_results):
            if not isinstance(releases, list):
                continue
            self._consider_label_releases(
                seed, label_id, label_name, releases, candidates, per_seed_count,
                owned_artist_mbids, owned_album_mbids, seed_mbids,
            )

        for (tag, (_score, seed)), results in zip(top_tags, tag_results):
            if not isinstance(results, list):
                continue
            self._consider_scene_release_groups(
                seed, tag, results, candidates, per_seed_count,
                owned_album_mbids, owned_artist_names,
            )

        return self._select_items(candidates, prefs)

    def _consider_related_artist(
        self, seed, rel_type, target, candidates, per_seed_count,
        owned_artist_mbids, seed_mbids,
    ) -> None:
        if rel_type in _MEMBER_REL_TYPES:
            reason_type, strength = "member", _MEMBER_STRENGTH
            reason_label = f"Band-member connection with {seed.name}"
        elif rel_type in _COLLAB_REL_TYPES:
            reason_type, strength = "collaborator", _COLLAB_STRENGTH
            reason_label = f"Collaborated with {seed.name}"
        else:
            return

        mbid = str(target["id"]).lower()
        name = str(target.get("name", ""))
        if (
            mbid in owned_artist_mbids  # novelty: not already in the library
            or mbid in seed_mbids
            or name.lower() in _FILTERED_CANDIDATE_NAMES
        ):
            return
        reason = TasteGraphReason(
            type=reason_type, label=reason_label, via_mbid=seed.mbid, via_name=seed.name
        )
        self._add_candidate(
            candidates, per_seed_count, seed,
            _Candidate(
                kind="artist", mbid=mbid, name=name, artist_mbid=None, artist_name=None,
                score=seed.norm_weight * strength, reason=reason, genre=seed.top_tag,
            ),
        )

    def _consider_label_releases(
        self, seed, label_id, label_name, releases, candidates, per_seed_count,
        owned_artist_mbids, owned_album_mbids, seed_mbids,
    ) -> None:
        for rank, release in enumerate(releases):
            if not isinstance(release, dict):
                continue
            rg = release.get("release-group") or {}
            rg_mbid = str(rg.get("id", "")).lower()
            credits = release.get("artist-credit") or []
            first = credits[0] if credits and isinstance(credits[0], dict) else {}
            credit_artist = first.get("artist") or {}
            artist_mbid = str(credit_artist.get("id", "")).lower()
            artist_name = str(first.get("name") or credit_artist.get("name") or "")
            if (
                not rg_mbid
                or rg_mbid in owned_album_mbids  # novelty
                or not artist_mbid
                or artist_mbid in owned_artist_mbids
                or artist_mbid in seed_mbids
                or artist_name.lower() in _FILTERED_CANDIDATE_NAMES
            ):
                continue
            reason = TasteGraphReason(
                type="label",
                label=f"On {label_name or 'the same label'} with {seed.name}",
                via_mbid=label_id,
                via_name=label_name or None,
            )
            decay = 1.0 / (1.0 + 0.1 * rank)
            self._add_candidate(
                candidates, per_seed_count, seed,
                _Candidate(
                    kind="album", mbid=rg_mbid, name=str(rg.get("title") or release.get("title", "")),
                    artist_mbid=artist_mbid, artist_name=artist_name,
                    score=seed.norm_weight * _LABEL_STRENGTH * decay,
                    reason=reason, genre=seed.top_tag,
                ),
            )

    def _consider_scene_release_groups(
        self, seed, tag, results, candidates, per_seed_count,
        owned_album_mbids, owned_artist_names,
    ) -> None:
        for rank, result in enumerate(results):
            rg_mbid = (result.musicbrainz_id or "").lower()
            artist_name = result.artist or ""
            if (
                not rg_mbid
                or rg_mbid in owned_album_mbids  # novelty
                or artist_name.lower() in owned_artist_names
                or artist_name.lower() in _FILTERED_CANDIDATE_NAMES
            ):
                continue
            reason = TasteGraphReason(
                type="scene",
                label=f"From the {tag} scene, like {seed.name}",
                via_mbid=seed.mbid,
                via_name=tag,
            )
            decay = 1.0 / (1.0 + 0.15 * rank)
            self._add_candidate(
                candidates, per_seed_count, seed,
                _Candidate(
                    kind="album", mbid=rg_mbid, name=result.title,
                    artist_mbid=None, artist_name=artist_name or None,
                    score=seed.norm_weight * _SCENE_STRENGTH * decay,
                    reason=reason, genre=tag,
                ),
            )

    @staticmethod
    def _add_candidate(candidates, per_seed_count, seed, candidate: _Candidate) -> None:
        existing = candidates.get(candidate.mbid)
        if existing is not None:
            # corroborated by multiple connections: merge the reason, small bonus
            if len(existing.reasons) < 3 and all(
                r.label != candidate.reasons[0].label for r in existing.reasons
            ):
                existing.reasons.append(candidate.reasons[0])
                existing.score = max(existing.score, candidate.score) + 0.05
            return
        if per_seed_count.get(seed.mbid, 0) >= _MAX_PER_SEED:
            return  # diversity: this seed already placed its quota
        per_seed_count[seed.mbid] = per_seed_count.get(seed.mbid, 0) + 1
        candidates[candidate.mbid] = candidate

    @staticmethod
    def _select_items(
        candidates: dict[str, _Candidate], prefs: GenrePrefs = EMPTY_GENRE_PREFS
    ) -> list[TasteGraphItem]:
        genre_cap = max(1, math.ceil(_MAX_ITEMS * _GENRE_CAP_RATIO))
        genre_counts: dict[str, int] = {}
        items: list[TasteGraphItem] = []
        for c in sorted(candidates.values(), key=lambda c: c.score, reverse=True):
            if len(items) >= _MAX_ITEMS:
                break
            if c.genre:
                # the cap counts genre FAMILIES ("k-pop"/"korean pop"/"kpop" are one),
                # and the user's mute/reduce levels apply: muted families are excluded,
                # reduced families get half the cap
                family = genre_family(c.genre) or c.genre
                if prefs.is_muted(family):
                    continue
                cap = genre_cap
                if prefs.level(family) == "reduce":
                    cap = max(1, genre_cap // 2)
                if genre_counts.get(family, 0) >= cap:
                    continue  # diversity: max 40% of items from one genre family
                genre_counts[family] = genre_counts.get(family, 0) + 1
            items.append(TasteGraphItem(
                kind=c.kind,
                mbid=c.mbid,
                name=c.name,
                artist_mbid=c.artist_mbid or None,
                artist_name=c.artist_name or None,
                score=round(c.score, 4),
                reasons=c.reasons,
                in_library=False,  # candidates are novelty-filtered against the library
            ))
        return items
