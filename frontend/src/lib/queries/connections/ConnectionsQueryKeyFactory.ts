import { userIdSegment } from '../userKeySegment';

// userId dimension is mandatory (AMU-5): without it the persisted cache leaks one
// user's linked accounts to another on a shared browser
export const ConnectionsQueryKeyFactory = {
	prefix: ['me', 'connections'] as const,
	list: (userId: string | null | undefined) =>
		[...ConnectionsQueryKeyFactory.prefix, userIdSegment(userId)] as const
};
