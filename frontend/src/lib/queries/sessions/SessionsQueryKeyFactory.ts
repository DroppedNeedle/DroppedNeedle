import { AuthQueryKeyFactory } from '../auth/AuthQueryKeyFactory';
import { userIdSegment } from '../userKeySegment';

// Sessions nest under the shared auth prefix so the login/logout cache
// clears (AMU-5) cover them. The userId segment keeps one user's sessions
// out of another's persisted cache on a shared browser.
export const SessionsQueryKeyFactory = {
	prefix: [...AuthQueryKeyFactory.prefix, 'sessions'] as const,
	list: (userId: string | null | undefined) =>
		[...SessionsQueryKeyFactory.prefix, 'list', userIdSegment(userId)] as const
};
