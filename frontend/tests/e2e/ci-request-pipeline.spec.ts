import { expect, test, type Route } from '@playwright/test';

import { adminUser, installApiMocks, json, loginAsLocal, type MockTable } from './mockApi';

/**
 * Request pipeline: an admin signs in, spots an album on Discover, requests
 * it, approves the request, watches it land in the download queue, and finds
 * it counted in the library. All API answers are stubbed; no live network.
 */

const MBID = '11111111-2222-3333-4444-555555555555';

const discoverFeed = {
	because_you_listen_to: [],
	discover_queue_enabled: false,
	genre_artwork_schema_version: 'v2',
	fresh_releases: null,
	missing_essentials: null,
	rediscover: null,
	artists_you_might_like: null,
	popular_in_your_genres: null,
	genre_list: null,
	globally_trending: null,
	weekly_exploration: null,
	lastfm_weekly_artist_chart: null,
	lastfm_weekly_album_chart: null,
	lastfm_recent_scrobbles: null,
	daily_mixes: [],
	radio_sections: [],
	top_picks: {
		title: 'Top picks for you',
		source: 'ci-fixture',
		personalizing: false,
		items: [
			{
				album: {
					mbid: MBID,
					local_id: null,
					name: 'Copper Static',
					artist_name: 'The Fixtures',
					artist_mbid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
					image_url: null,
					release_date: '2026-01-01',
					listen_count: null,
					in_library: false
				},
				match_pct: 97,
				reasons: ['Because you like The Fixtures'],
				seed_artist: 'The Fixtures'
			}
		]
	},
	listeners_like_you: null,
	anniversaries: null,
	new_from_followed: null,
	unexplored_genres: null,
	generated_at: 1759400000,
	refreshing: false
};

const approvalItem = {
	album_title: 'Copper Static',
	artist_mbid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
	artist_name: 'The Fixtures',
	completed_at: null,
	duration_seconds: null,
	musicbrainz_id: MBID,
	request_kind: 'album',
	requested_at: 1759400000,
	requested_by_name: 'Ada Admin',
	requester_count: 0,
	reviewed_by_name: null,
	status: 'pending',
	task_id: null,
	track_title: null,
	user_id: 'admin-1'
};

const queueTask = {
	id: 'task-1',
	user_id: 'admin-1',
	download_type: 'album',
	source: 'soulseek',
	release_group_mbid: MBID,
	release_mbid: null,
	release_track_mbid: null,
	recording_mbid: null,
	artist_mbid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
	artist_name: 'The Fixtures',
	album_title: 'Copper Static',
	track_title: null,
	year: 2026,
	status: 'downloading',
	progress_percent: 42,
	total_size_bytes: 1000000,
	downloaded_bytes: 420000,
	files_total: 10,
	files_completed: 4,
	files_failed: 0,
	source_username: 'ci-peer',
	created_at: 1759400000,
	updated_at: 1759400100,
	wrong_product_verdict_at: null
};

test.describe('request pipeline', () => {
	test('login, discover, request, approve, download, library', async ({ page }) => {
		let intakeBody: Record<string, unknown> | null = null;
		let session: typeof adminUser | null = null;
		let approved = false;

		const extra: MockTable = {
			'POST /api/v3/auth/login': async (route: Route) => {
				session = adminUser;
				await json(route, { user: adminUser, token: 'ci-token' });
			},
			'GET /api/v3/discover': discoverFeed,
			'POST /api/v1/discover/activity': {},
			'POST /api/v3/discover/activity': {},
			'POST /api/v3/requests/albums': async (route: Route) => {
				intakeBody = (route.request().postDataJSON() ?? {}) as Record<string, unknown>;
				await json(route, {
					message: 'Request recorded',
					musicbrainz_id: MBID,
					status: 'pending',
					success: true,
					task_id: null
				});
			},
			'GET /api/v3/requests/approvals': async (route: Route) => {
				const pathname = new URL(route.request().url()).pathname;
				if (pathname.endsWith('/count')) {
					await json(route, { count: approved ? 0 : 1 });
					return;
				}
				await json(route, {
					count: approved ? 0 : 1,
					items: approved ? [] : [approvalItem]
				});
			},
			'POST /api/v3/requests/approvals/': async (route: Route) => {
				approved = true;
				await json(route, { success: true, message: 'Approved' });
			},
			'GET /api/v3/requests/active': { count: 0, items: [] },
			'GET /api/v1/downloads/activity-summary': { active: 1, queued: 0, failed: 0 },
			'GET /api/v1/downloads': { items: [queueTask], total: 1, page: 1, page_size: 100 },
			'GET /api/v1/library/stats': {
				total_albums: 1,
				total_artists: 1,
				total_tracks: 10,
				total_size_bytes: 1000000,
				format_breakdown: { FLAC: 10 },
				review_count: 0,
				local_only_count: 0,
				last_scan_at: 1759400000
			}
		};
		await installApiMocks(page, { user: () => session, extra });

		// 1. Local login lands on the home page.
		await loginAsLocal(page, 'ada', 'correct-horse');
		await expect(page.getByTestId('app-shell')).toBeVisible();

		// 2. Discover shows the mocked top pick.
		await page.goto('/discover');
		await expect(page.getByText('Copper Static').first()).toBeVisible();

		// 3. Requesting posts the album intake.
		await page.getByRole('button', { name: 'Request Copper Static' }).click();
		await expect.poll(() => intakeBody, { timeout: 10_000 }).not.toBeNull();
		expect(intakeBody).toMatchObject({ musicbrainz_id: MBID, album: 'Copper Static' });

		// 4. The approval queue holds it until an admin approves.
		await page.goto('/requests?tab=approvals');
		await expect(page.getByText('Copper Static').first()).toBeVisible();
		await page.getByRole('button', { name: 'Approve' }).click();
		await expect(page.getByText('Copper Static')).toHaveCount(0, { timeout: 10_000 });

		// 5. The download queue picks up the dispatched task.
		await page.goto('/downloads');
		await expect(page.getByText('Copper Static').first()).toBeVisible();

		// 6. The library counts the imported album.
		await page.goto('/library');
		await expect(page.getByRole('link', { name: 'Browse all albums' })).toContainText('1');
	});
});
