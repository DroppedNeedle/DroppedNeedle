import { userIdSegment } from '../userKeySegment';

export const AuthQueryKeyFactory = {
	prefix: ['auth'] as const,
	providers: () => [...AuthQueryKeyFactory.prefix, 'providers'] as const,
	/** Admin-only enumerated import candidates per provider. Kept under the `auth`
	 *  prefix so the login/logout cache-clear (AMU-5) covers it. The userId segment
	 *  isolates persisted admin data across users sharing a browser (F-19). */
	importCandidates: (provider: string, userId: string | null | undefined) =>
		[...AuthQueryKeyFactory.prefix, 'import', provider, userIdSegment(userId)] as const
};
