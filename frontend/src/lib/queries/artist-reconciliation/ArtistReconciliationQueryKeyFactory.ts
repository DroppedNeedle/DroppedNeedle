import { key, userScope } from '../keys';
import type { ArtistDuplicateGroupParams } from './ArtistReconciliationTypes';

const normalizedParams = (params: ArtistDuplicateGroupParams) => ({
	state: params.state ?? null,
	search: params.search?.trim() || null
});

type UserId = string | null | undefined;

export const ArtistReconciliationQueryKeyFactory = {
	user: (userId: UserId) => key(userScope(userId), 'library', 'artist-reconciliation'),
	progress: (userId: UserId) =>
		[...ArtistReconciliationQueryKeyFactory.user(userId), 'progress'] as const,
	groupsPrefix: (userId: UserId) =>
		[...ArtistReconciliationQueryKeyFactory.user(userId), 'groups'] as const,
	groups: (userId: UserId, params: ArtistDuplicateGroupParams) =>
		[
			...ArtistReconciliationQueryKeyFactory.groupsPrefix(userId),
			'list',
			normalizedParams(params)
		] as const,
	group: (userId: UserId, groupId: string) =>
		[...ArtistReconciliationQueryKeyFactory.groupsPrefix(userId), 'detail', groupId] as const
};
