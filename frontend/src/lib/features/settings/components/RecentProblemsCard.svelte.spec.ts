import { page, userEvent } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import RecentProblemsCard from './RecentProblemsCard.svelte';

type Recent = RouteData<'GET /api/v3/admin/problems/recent'>;

function problem(message: string, ref: string): Recent['problems'][number] {
	return {
		first_at: 1_000,
		last_at: 2_000,
		level: 'error',
		target: 'droppedneedle::middleware',
		location: 'src/middleware.rs:254',
		message,
		fields: { cause: 'disk' },
		dropped_fields: 0,
		count: 1,
		refs: [ref]
	};
}

describe('Recent problems', () => {
	it('finds the problem a ref names', async () => {
		const api = fakeApi({
			'GET /api/v3/admin/problems/recent': (request) => ({
				problems:
					request.query.get('ref') === '4f1c2a9e'
						? [problem('request failed', '4f1c2a9e-7d3b-4c1a-9e2f-0123456789ab')]
						: [
								problem('request failed', '4f1c2a9e-7d3b-4c1a-9e2f-0123456789ab'),
								problem('scan stopped', 'b1c2d3e4-7d3b-4c1a-9e2f-0123456789ab')
							]
			})
		});
		await renderWithQuery(RecentProblemsCard, {});
		await expect.element(page.getByText('scan stopped')).toBeInTheDocument();

		await userEvent.type(page.getByLabelText('Ref'), '4f1c2a9e');

		await expect.element(page.getByText('scan stopped')).not.toBeInTheDocument();
		await expect.element(page.getByText('request failed')).toBeInTheDocument();
		await expect.element(page.getByText('src/middleware.rs:254')).toBeInTheDocument();
		expect(
			api
				.calls('GET /api/v3/admin/problems/recent')
				.some((call) => call.query.get('ref') === '4f1c2a9e')
		).toBe(true);
	});
});
