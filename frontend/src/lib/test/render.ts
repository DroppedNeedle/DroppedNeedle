import { QueryClient, QueryClientProvider } from '@tanstack/svelte-query';
import type { Component } from 'svelte';
import { onTestFinished } from 'vitest';
import { render } from 'vitest-browser-svelte';

/**
 * Render a component under a fresh QueryClient (no retries, no persister),
 * provided through the same context the app's QueryProvider sets. Pair it with
 * `fakeApi` so the real query modules and api client run against fixtures.
 *
 * Pass `client` to render on another one, such as the app's own when a spec
 * must see the query modules' cache writes, which go to the app's client.
 */
export async function renderWithQuery<Props extends object>(
	component: Component<Props>,
	props: Props,
	options: { client?: QueryClient } = {}
) {
	const client =
		options.client ??
		new QueryClient({
			defaultOptions: {
				queries: { retry: false, refetchOnWindowFocus: false, notifyOnChangeProps: 'all' },
				mutations: { retry: false }
			}
		});
	onTestFinished(() => client.clear());
	const view = await render(
		component as Component,
		{ props },
		{ wrapper: QueryClientProvider, wrapperProps: { client } }
	);
	return {
		...view,
		client,
		rerender: (next: Partial<Props>) => view.rerender(next)
	};
}
