import { userIdSegment } from '$lib/queries/userKeySegment';
import type { ArtistDuplicateGroupParams } from './ArtistReconciliationTypes';

const normalizedParams = (params: ArtistDuplicateGroupParams) => ({
	state: params.state ?? null,
	search: params.search?.trim() || null
});

export const ArtistReconciliationQueryKeyFactory = {
	prefix: ['library', 'artist-reconciliation'] as const,
	user: (userId: string | null | undefined) =>
		[...ArtistReconciliationQueryKeyFactory.prefix, userIdSegment(userId)] as const,
	progress: (userId: string | null | undefined) =>
		[...ArtistReconciliationQueryKeyFactory.user(userId), 'progress'] as const,
	groupsPrefix: (userId: string | null | undefined) =>
		[...ArtistReconciliationQueryKeyFactory.user(userId), 'groups'] as const,
	groups: (userId: string | null | undefined, params: ArtistDuplicateGroupParams) =>
		[
			...ArtistReconciliationQueryKeyFactory.groupsPrefix(userId),
			'list',
			normalizedParams(params)
		] as const,
	group: (userId: string | null | undefined, groupId: string) =>
		[...ArtistReconciliationQueryKeyFactory.groupsPrefix(userId), 'detail', groupId] as const
};
