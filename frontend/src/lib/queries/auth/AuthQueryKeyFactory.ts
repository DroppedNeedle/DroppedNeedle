export const AuthQueryKeyFactory = {
	prefix: ['auth'] as const,
	providers: () => [...AuthQueryKeyFactory.prefix, 'providers'] as const
};
