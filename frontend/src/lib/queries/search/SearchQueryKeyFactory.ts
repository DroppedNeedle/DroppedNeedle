import { musicBrainzSourceKey } from '../musicbrainz/sourceScope.svelte';
import { userIdSegment } from '../userKeySegment';

export type SearchV3UserId = string | null | undefined;

export type SearchV3Bucket = 'artists' | 'albums' | 'tracks';

export interface SearchV3Limits {
	artists: number;
	albums: number;
	tracks: number;
}

export interface SearchV3EnrichFingerprint {
	artists: string[];
	albums: string[];
}

const normalizeQuery = (query: string) => query.trim().toLowerCase();

export const SearchQueryKeyFactory = {
	// v3 search keys. Unified search, bucket drill-downs, and suggestions read
	// the local catalog, so they carry only the userId. Enrichment fans out to
	// the metadata provider, so it also embeds the provider source identity and
	// joins the source-switch sweep.
	v3: {
		root: (userId: SearchV3UserId) => ['search', 'v3', userIdSegment(userId)] as const,
		unified: (
			userId: SearchV3UserId,
			query: string,
			limits: SearchV3Limits,
			buckets: SearchV3Bucket[] | null
		) =>
			[
				...SearchQueryKeyFactory.v3.root(userId),
				'unified',
				normalizeQuery(query),
				limits,
				buckets
			] as const,
		bucket: (
			userId: SearchV3UserId,
			bucket: SearchV3Bucket,
			query: string,
			limit: number,
			offset: number
		) =>
			[
				...SearchQueryKeyFactory.v3.root(userId),
				'bucket',
				bucket,
				normalizeQuery(query),
				limit,
				offset
			] as const,
		suggest: (userId: SearchV3UserId, query: string, limit: number) =>
			[...SearchQueryKeyFactory.v3.root(userId), 'suggest', normalizeQuery(query), limit] as const,
		enrich: (userId: SearchV3UserId, fingerprint: SearchV3EnrichFingerprint) => {
			const normalizedUserId = userIdSegment(userId);
			return [
				...SearchQueryKeyFactory.v3.root(normalizedUserId),
				musicBrainzSourceKey(normalizedUserId),
				'enrich',
				fingerprint
			] as const;
		}
	}
};
