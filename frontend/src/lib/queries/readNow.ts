import type { QueryKey } from '@tanstack/svelte-query';
import { queryClient } from './QueryClient';

/**
 * A read a person starts by hand (a click, a menu, a play button). It always
 * asks the server and never answers from the persisted copy, and it runs in
 * the app's query cache, so a repeat while one is in flight joins it.
 */
export function readNow<T>(
	queryKey: QueryKey,
	read: (signal: AbortSignal) => Promise<T>
): Promise<T> {
	return queryClient.fetchQuery({
		queryKey,
		queryFn: ({ signal }) => read(signal),
		staleTime: 0,
		persister: undefined
	});
}
