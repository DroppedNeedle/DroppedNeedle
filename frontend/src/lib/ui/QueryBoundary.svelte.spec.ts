import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

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
	it('shows the loader while the first fetch is pending', async () => {
		await renderBoundary(query({ isPending: true }));

		await expect.element(page.getByText('Loading skeleton')).toBeVisible();
		await expect.element(page.getByText(/Albums:/)).not.toBeInTheDocument();
	});

	it('shows the error with a retry that refetches', async () => {
		const refetch = vi.fn();
		await renderBoundary(query({ isError: true, error: new Error('boom'), refetch }));

		await expect.element(page.getByText('Loading skeleton')).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Try again' }).click();

		expect(refetch).toHaveBeenCalledOnce();
	});

	it('shows the empty state for an empty result, never the data snippet', async () => {
		await renderBoundary(query({ data: [] }));

		await expect.element(page.getByText('No albums yet')).toBeVisible();
		await expect.element(page.getByText(/Albums:/)).not.toBeInTheDocument();
	});

	it('shows the data', async () => {
		await renderBoundary(query({ data: ['Blue Lines', 'Mezzanine'] }));

		await expect.element(page.getByText('Albums: Blue Lines, Mezzanine')).toBeVisible();
	});

	it('keeps data on screen when a background refetch fails', async () => {
		await renderBoundary(query({ data: ['Blue Lines'], isError: true, error: new Error('boom') }));

		await expect.element(page.getByText('Albums: Blue Lines')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Try again' })).not.toBeInTheDocument();
	});
});
