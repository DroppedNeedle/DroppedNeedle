import { key, userScope } from '../keys';

type UserId = string | null | undefined;

export const GenreQueryKeyFactory = {
	feature: 'genre',
	detail: (userId: UserId, genre: string) =>
		key(userScope(userId), 'genre', genre.trim().toLocaleLowerCase()),
	artistPages: (userId: UserId, genre: string) =>
		[...GenreQueryKeyFactory.detail(userId, genre), 'artists'] as const,
	albumPages: (userId: UserId, genre: string) =>
		[...GenreQueryKeyFactory.detail(userId, genre), 'albums'] as const
};
