import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';

import QueryBoundary from './QueryBoundary.svelte';

interface FakeQuery {
	data: string[] | undefined;
	isPending: boolean;
	isError: boolean;
	error: unknown;
	refetch: () => unknown;
}

function query(overrides: Partial<FakeQuery>): FakeQuery {
	return {
		data: undefined,
		isPending: false,
		isError: false,
		error: null,
		refetch: vi.fn(),
		...overrides
	};
}

const text = (content: string) => createRawSnippet(() => ({ render: () => `<p>${content}</p>` }));

const albums = createRawSnippet<[string[]]>((data) => ({
	render: () => `<p>Albums: ${data().join(', ')}</p>`
}));

function renderBoundary(q: FakeQuery, withEmpty = true) {
	return render(QueryBoundary<string[]>, {
		query: q,
		what: 'your albums',
		children: albums,
		loading: text('Loading skeleton'),
		empty: withEmpty ? text('No albums yet') : undefined
	});
}

describe('QueryBoundary', () => {
	it('shows the loader, the error with a retry, the empty state or the data, and keeps data through a failed refetch', async () => {
		{
			// Shows the loader while the first fetch is pending.
			await renderBoundary(query({ isPending: true }));

			await expect.element(page.getByText('Loading skeleton')).toBeVisible();
			await expect.element(page.getByText(/Albums:/)).not.toBeInTheDocument();
		}

		{
			// Shows the error with a retry that refetches.
			cleanup();
			const refetch = vi.fn();
			await renderBoundary(query({ isError: true, error: new Error('boom'), refetch }));

			await expect.element(page.getByText('Loading skeleton')).not.toBeInTheDocument();
			await page.getByRole('button', { name: 'Try again' }).click();

			expect(refetch).toHaveBeenCalledOnce();
		}

		{
			// Shows the empty state for an empty result, never the data snippet.
			cleanup();
			await renderBoundary(query({ data: [] }));

			await expect.element(page.getByText('No albums yet')).toBeVisible();
			await expect.element(page.getByText(/Albums:/)).not.toBeInTheDocument();
		}

		{
			// Shows the data.
			cleanup();
			await renderBoundary(query({ data: ['Blue Lines', 'Mezzanine'] }));

			await expect.element(page.getByText('Albums: Blue Lines, Mezzanine')).toBeVisible();
		}

		{
			// Keeps data on screen when a background refetch fails.
			cleanup();
			await renderBoundary(
				query({ data: ['Blue Lines'], isError: true, error: new Error('boom') })
			);

			await expect.element(page.getByText('Albums: Blue Lines')).toBeVisible();
			await expect.element(page.getByRole('button', { name: 'Try again' })).not.toBeInTheDocument();
		}
	});
});
