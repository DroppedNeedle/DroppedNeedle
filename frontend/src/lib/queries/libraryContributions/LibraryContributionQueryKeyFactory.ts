import { key, userScope } from '../keys';

export const LibraryContributionQueryKeyFactory = {
	root: (userId: string | null | undefined) => key(userScope(userId), 'library-contributions'),
	detail: (userId: string | null | undefined, contributionId: string) =>
		[...LibraryContributionQueryKeyFactory.root(userId), 'detail', contributionId] as const
};
