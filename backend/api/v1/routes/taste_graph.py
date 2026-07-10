from fastapi import APIRouter, Depends

from api.v1.schemas.taste_graph import TasteGraphResponse
from core.dependencies import get_taste_graph_service
from core.dependencies.type_aliases import CurrentUserDep
from infrastructure.msgspec_fastapi import MsgSpecRoute
from services.taste_graph_service import TasteGraphService

router = APIRouter(route_class=MsgSpecRoute, prefix="/discover", tags=["discover"])


@router.get("/taste-graph", response_model=TasteGraphResponse)
async def get_taste_graph(
    current_user: CurrentUserDep,
    service: TasteGraphService = Depends(get_taste_graph_service),
) -> TasteGraphResponse:
    """Recommendations built ONLY from this user's own signals (library, follows,
    play history) expanded through canonical MusicBrainz metadata — never charts,
    global popularity, or other users' listens. An empty library returns a
    graceful cold-start response."""
    return await service.get_taste_graph(current_user.id)
