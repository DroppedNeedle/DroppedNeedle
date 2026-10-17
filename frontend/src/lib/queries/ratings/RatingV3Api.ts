import { v3 } from '$lib/api/v3/endpoint';
import type { RatingKind } from './RatingQueryKeyFactory';

// /api/v3 rating URLs, built through the typed registry. Nothing outside
// this feature imports them.
export const RatingV3Api = {
	lookup: () => v3('/api/v3/ratings/lookup'),
	rating: (kind: RatingKind, itemId: string) =>
		v3('/api/v3/ratings/{kind}/{item_id}', { path: { kind, item_id: itemId } })
};
