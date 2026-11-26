import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import TastePage from './+page.svelte';

const TASTE = 'GET /api/v3/discover/taste';
const SAY = 'POST /api/v3/discover/tunings';
const UNDO = 'DELETE /api/v3/discover/tunings/{kind}/{target}';
const TUNINGS = 'GET /api/v3/discover/tunings';
const STATED = 'GET /api/v3/discover/stated-artists';
const FEEDBACK = 'GET /api/v3/discover/feedback';
type Taste = RouteData<typeof TASTE>;
type Group = Taste['groups'][number];
type Tuning = RouteData<typeof TUNINGS>['items'][number];

const PURPLE = '11111111-1111-4111-8111-111111111111';
const CREAM = '22222222-2222-4222-8222-222222222222';
const GRIMES = '33333333-3333-4333-8333-333333333333';
const SAID_AT = Date.UTC(2026, 10, 26, 12) / 1000;

function signIn() {
	authStore.setUser(
		toAuthUser({
			id: 'u1',
			display_name: 'Ada',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'ada'
		})
	);
}

function artist(artist_mbid: string, name: string, plays: number): Group['artists'][number] {
	return { artist_mbid, name, plays, imported_listens: 0, is_seed: true, tuning: null };
}

const GROUPS: Group[] = [
	{
		group_no: 0,
		anchor_artist_mbid: PURPLE,
		anchor_name: 'Deep Purple',
		share: 0.62,
		trend: 'rising',
		last_played_at: SAID_AT,
		artists: [artist(PURPLE, 'Deep Purple', 212), artist(CREAM, 'Cream', 96)]
	},
	{
		group_no: 1,
		anchor_artist_mbid: GRIMES,
		anchor_name: 'Grimes',
		share: 0.38,
		trend: 'steady',
		last_played_at: null,
		artists: [artist(GRIMES, 'Grimes', 40)]
	}
];

/**
 * A fake server holding the caller's statements: each tuning shows on the
 * page's artists and in the list, and undoing one takes it off both.
 */
function server(groups: Group[]) {
	let said: Tuning[] = [];
	const taste = (): Taste => ({
		made_from: groups.length
			? { plays_counted: 459, imported_listens_counted: 1204, history_from_at: SAID_AT }
			: null,
		updating: said.length > 0,
		groups: groups.map((group) => ({
			...group,
			artists: group.artists.map((one) => {
				const tuning = said.find((row) => row.target === one.artist_mbid);
				return {
					...one,
					tuning: tuning
						? {
								kind: tuning.kind,
								group_anchor_mbid: tuning.group_anchor_mbid,
								group_label: tuning.group_label
							}
						: null
				};
			})
		}))
	});
	return fakeApi({
		[TASTE]: () => taste(),
		[TUNINGS]: () => ({ items: said }),
		[STATED]: { items: [], limit: 30 },
		[FEEDBACK]: { items: [] },
		[SAY]: ({ body }) => {
			const asked = body as { kind: Tuning['kind']; target?: string; group_no?: number };
			const group = groups.find((one) => one.group_no === asked.group_no);
			const rows = (group?.artists ?? groups.flatMap((one) => one.artists))
				.filter((one) => group || one.artist_mbid === asked.target)
				.map((one) => ({
					kind: asked.kind,
					target: one.artist_mbid,
					name: one.name,
					group_anchor_mbid: group?.anchor_artist_mbid ?? null,
					group_label: group?.anchor_name ?? null,
					created_at: SAID_AT
				}));
			said = [...said.filter((row) => !rows.some((added) => added.target === row.target)), ...rows];
			return { items: rows };
		},
		[UNDO]: ({ params, query }) => {
			const row = said.find((one) => one.kind === params.kind && one.target === params.target);
			said = said.filter((one) =>
				query.get('group') === 'true' && row?.group_anchor_mbid
					? one.group_anchor_mbid !== row.group_anchor_mbid
					: one !== row
			);
		}
	});
}

describe('Your taste page', () => {
	it('lists the groups and opens one under its tab', async () => {
		signIn();
		server(GROUPS);
		await renderWithQuery(TastePage, {}, { client: queryClient });

		await expect.element(page.getByRole('heading', { name: 'Your taste' })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /Around Deep Purple/ })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /Around Grimes/ })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /Named by you/ })).toBeVisible();
		await expect.element(page.getByText('62% of each set')).toBeVisible();
		await expect.element(page.getByText('212 plays')).toBeVisible();

		await page.getByRole('tab', { name: /Around Grimes/ }).click();
		await expect.element(page.getByText('40 plays')).toBeVisible();
		await expect.element(page.getByText('212 plays')).not.toBeInTheDocument();
	});

	it('shows a statement at once and undoes it, for an artist and a group', async () => {
		signIn();
		const api = server(GROUPS);
		await renderWithQuery(TastePage, {}, { client: queryClient });

		const cream = page.getByRole('group', { name: 'Cream' });
		await cream.getByRole('button', { name: 'Less' }).click();
		await expect.element(page.getByText('Fewer picks from it')).toBeVisible();
		await expect
			.element(cream.getByRole('button', { name: 'Less' }))
			.toHaveAttribute('aria-pressed', 'true');
		await expect.element(page.getByText('Updating')).toBeVisible();
		await page.getByRole('button', { name: 'Undo for Cream' }).click();
		await expect.element(page.getByText('Fewer picks from it')).not.toBeInTheDocument();

		const whole = page.getByRole('group', { name: 'The whole group' });
		await whole.getByRole('button', { name: 'That was a phase' }).click();
		await expect.poll(() => page.getByText('Its plays no longer count').elements().length).toBe(2);
		await expect
			.element(page.getByRole('button', { name: 'Undo the phase for Around Deep Purple' }))
			.toBeVisible();
		await whole.getByRole('button', { name: 'That was a phase' }).click();
		await expect
			.element(page.getByText('Its plays no longer count').first())
			.not.toBeInTheDocument();

		expect(api.calls(SAY).map((call) => call.body)).toEqual([
			{ kind: 'less_from_seed', target: CREAM },
			{ kind: 'phase', group_no: 0 }
		]);
		expect(
			api.calls(UNDO).map((call) => [call.params.kind, call.query.get('group') ?? 'false'])
		).toEqual([
			['less_from_seed', 'false'],
			['phase', 'true']
		]);
	});

	it('with no groups yet, offers to name artists', async () => {
		signIn();
		server([]);
		await renderWithQuery(TastePage, {}, { client: queryClient });

		await expect
			.element(page.getByText('Play something or name a few artists and this page fills in.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('searchbox', { name: 'Name an artist you love' }))
			.toBeVisible();
		await expect.element(page.getByRole('tab')).not.toBeInTheDocument();
	});
});
