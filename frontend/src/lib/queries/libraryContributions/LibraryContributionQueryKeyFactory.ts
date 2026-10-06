import { userIdSegment } from '$lib/queries/userKeySegment';

export const LibraryContributionQueryKeyFactory = {
	root: (userId: string | null | undefined) =>
		['library-contributions', userIdSegment(userId)] as const,
	detail: (userId: string | null | undefined, contributionId: string) =>
		[...LibraryContributionQueryKeyFactory.root(userId), 'detail', contributionId] as const
};
