import { ADMIN, key, SHARED, userScope } from '../keys';

type UserId = string | null | undefined;

export const AuthQueryKeyFactory = {
	feature: 'auth',
	providers: () => key(SHARED, 'auth', 'providers'),
	/** Admin view of one user's quota; keyed by the user it describes. */
	userQuota: (userId: string) => key(ADMIN, 'auth', 'user-quota', userId),
	/** The import candidates one admin enumerated from a provider. */
	importCandidates: (userId: UserId, provider: string) =>
		key(userScope(userId), 'auth', 'import', provider),
	/** The server's accounts, as the admin reading them sees them. */
	adminUsers: (userId: UserId) => key(userScope(userId), 'auth', 'admin-users'),
	adminUsersPage: (userId: UserId, limit: number, offset: number) =>
		[...AuthQueryKeyFactory.adminUsers(userId), { limit, offset }] as const,
	/** What an app asks to connect as, for the user signed in here. */
	appConnectRequest: (userId: UserId, request: object | null) =>
		key(userScope(userId), 'auth', 'app-connect', request)
};
