/** The `userId` dimension is mandatory: without it the persisted query
 *  cache would serve one user's profile to another sharing a browser. */
export const ProfileQueryKeyFactory = {
	prefix: ['profile'] as const,
	profile: (userId: string) => [...ProfileQueryKeyFactory.prefix, userId] as const
};
