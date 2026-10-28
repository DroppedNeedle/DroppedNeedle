import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

const h = vi.hoisted(() => ({
	page: { url: new URL('http://localhost/library/review') },
	goto: vi.fn()
}));
vi.mock('$app/state', () => ({ page: h.page }));
vi.mock('$app/navigation', () => ({ goto: h.goto }));

import ReviewQueue from './ReviewQueue.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

type ReviewPage = RouteData<'GET /api/v3/library/reviews'>;
type Review = ReviewPage['items'][number];

function review(id: string, title: string, reasonCode: string): Review {
	return {
		id,
		album_id: `album-${id}`,
		album_title: title,
		artist_name: 'Someone',
		reason_code: reasonCode,
		reason: { code: reasonCode, message: `Reason ${reasonCode}`, action: 'Pick one.' },
		state: 'pending',
		resolved_by: null,
		selected_candidate_key: null,
		candidates: [],
		created_ms: 1,
		updated_ms: 1,
		manual_identity: false,
		row_revision: 3
	};
}

const REVIEWS = [review('r1', 'Kid A', 'EDITION_MERGED'), review('r2', 'Amnesiac', 'REPAIR')];

// Answers like the server: only the reviews of the asked reason.
function reviews(query: URLSearchParams): ReviewPage {
	const reason = query.get('reason_code');
	const items = REVIEWS.filter((item) => !reason || item.reason_code === reason);
	return {
		items,
		total: items.length,
		next_cursor: null,
		reason_counts: [
			{ reason_code: 'EDITION_MERGED', reason: REVIEWS[0].reason, count: 1 },
			{ reason_code: 'REPAIR', reason: REVIEWS[1].reason, count: 1 }
		],
		state_counts: { pending: 2, approved: 0, rejected: 4, dismissed: 1, excluded: 0 }
	};
}

function signIn(role: 'user' | 'admin'): void {
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role,
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
}

beforeEach(() => {
	localStorage.clear();
	h.goto.mockReset();
	h.goto.mockImplementation((url: URL) => {
		h.page.url = new URL(url);
	});
});

describe('review queue', () => {
	it('opens the view the address names, and keeps a changed filter there', async () => {
		signIn('user');
		h.page.url = new URL('http://localhost/library/review?state=pending&reason=EDITION_MERGED');
		const api = fakeApi({
			'GET /api/v3/library/reviews': (request) => reviews(request.query)
		});
		const first = await renderWithQuery(ReviewQueue, {});

		await expect.element(page.getByText('Kid A')).toBeVisible();
		await expect.element(page.getByText('Amnesiac')).not.toBeInTheDocument();
		await expect.element(page.getByRole('tab', { name: 'Kept as tagged 4' })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: 'Resolved 1' })).toBeVisible();

		await page.getByRole('combobox', { name: 'Order' }).selectOptions('title');
		await vi.waitFor(() => expect(h.page.url.searchParams.get('sort')).toBe('title'));
		expect(h.page.url.searchParams.get('reason')).toBe('EDITION_MERGED');

		// A reload reads the same address back.
		first.unmount();
		const before = api.calls('GET /api/v3/library/reviews').length;
		await renderWithQuery(ReviewQueue, {});
		await expect.element(page.getByText('Kid A')).toBeVisible();
		const reload = api.calls('GET /api/v3/library/reviews')[before].query;
		expect(reload.get('reason_code')).toBe('EDITION_MERGED');
		expect(reload.get('sort')).toBe('title');
		await expect.element(page.getByText('Amnesiac')).not.toBeInTheDocument();
	});

	it("asks in the app's dialog before taking an album out", async () => {
		signIn('admin');
		const native = vi.spyOn(window, 'confirm');
		h.page.url = new URL('http://localhost/library/review?state=pending');
		const api = fakeApi({
			'GET /api/v3/library/reviews': (request) => reviews(request.query),
			'GET /api/v3/settings/library/policy-tree': {
				policy_revision: '1',
				roots: [],
				warnings: []
			},
			'POST /api/v3/library/reviews/{id}/exclude': {
				...REVIEWS[0],
				state: 'excluded',
				row_revision: 4
			}
		});
		await renderWithQuery(ReviewQueue, {});

		await page.getByRole('button', { name: /Kid A/ }).click();
		await page.getByRole('button', { name: 'Take out of library' }).click();
		const dialog = page.getByRole('dialog', { name: 'Take out of library?' });
		await expect.element(dialog).toBeVisible();
		expect(api.calls('POST /api/v3/library/reviews/{id}/exclude')).toHaveLength(0);

		await dialog.getByRole('button', { name: 'Take out' }).click();
		await vi.waitFor(() =>
			expect(api.calls('POST /api/v3/library/reviews/{id}/exclude')).toHaveLength(1)
		);
		expect(api.calls('POST /api/v3/library/reviews/{id}/exclude')[0].body).toEqual({
			expected_revision: 3
		});
		expect(native).not.toHaveBeenCalled();
	});
});
