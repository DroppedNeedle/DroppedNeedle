import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import UpgradePage from './+page.svelte';

type Progress = RouteData<'GET /api/v3/upgrade/progress'>;
type Run = NonNullable<Progress['run']>;

const signedOut = { layout: 'needs_upgrade', signed_in: false } as const;
const signedIn = { layout: 'needs_upgrade', signed_in: true } as const;

const spotify = {
	feature: 'spotify',
	message: 'Change the redirect URI in the Spotify developer dashboard.',
	old_address: { kind: 'full', value: 'https://music.example.com/api/v1/spotify/callback' },
	new_address: { kind: 'path', value: '/api/v3/acquire/spotify/auth/callback' }
} as const;

const run = (fields: Partial<Run>): Run => ({ task: 'upgrade', running: true, ...fields });

describe('the upgrade page', () => {
	it('signs in, holds Upgrade until each address change is ticked, then runs and opens the app', async () => {
		let progress: Progress = { layout: 'needs_upgrade', run: null };
		const api = fakeApi({
			'GET /api/v3/upgrade/state': signedOut,
			'POST /api/v3/upgrade/sign-in': signedIn,
			'GET /api/v3/upgrade/progress': () => progress,
			'POST /api/v3/upgrade/preflight': {
				check: {
					layout: 'v2',
					counts: { user: 2, playlist: 5 },
					orphans: 0,
					blockers: [],
					warnings: [spotify]
				},
				failure: null
			},
			'POST /api/v3/upgrade/start': () => {
				progress = {
					layout: 'needs_upgrade',
					run: run({ step: 'export', label: 'Exporting from v2' })
				};
				return progress;
			},
			'POST /api/v3/upgrade/continue': undefined,
			'GET /api/v3/auth/setup/status': () => {
				throw new TypeError('Failed to fetch');
			}
		});
		await renderWithQuery(UpgradePage, {});

		await page.getByLabelText('Username').fill('alice');
		await page.getByLabelText('Password').fill('alice-v2-pass');
		await page.getByRole('button', { name: 'Sign in' }).click();

		const upgrade = page.getByRole('button', { name: 'Upgrade', exact: true });
		await expect.element(page.getByText('Spotify playlist import')).toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/sign-in')[0]?.body).toEqual({
			method: 'password',
			username: 'alice',
			password: 'alice-v2-pass'
		});
		await expect.element(upgrade).toBeDisabled();
		await page.getByLabelText('I changed it').click();
		await expect.element(upgrade).toBeEnabled();
		await upgrade.click();

		await expect.element(page.getByRole('heading', { name: 'Upgrading' })).toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/start')[0]?.body).toEqual({
			acknowledged: [
				{
					feature: spotify.feature,
					old_address: spotify.old_address,
					new_address: spotify.new_address
				}
			]
		});
		await expect.element(page.getByText('Exporting from v2')).toBeVisible();

		progress = {
			layout: 'finished',
			run: run({
				running: false,
				step: 'import',
				result: {
					outcome: 'upgraded',
					backup: { config: '/app/config/v2-backup', cache: '/app/cache/v2-backup' },
					summary: {
						moved: { user: 2, playlist: 5 },
						needs_attention: 0,
						orphans_removed: 0,
						left_behind: ['Sign-in sessions: 3'],
						notes: [],
						settings_defaulted: [],
						report: '/app/cache/upgrade-report.json'
					}
				}
			})
		};
		await expect
			.element(page.getByRole('heading', { name: 'Upgrade done' }), { timeout: 4000 })
			.toBeVisible();
		await expect.element(page.getByText('/app/cache/v2-backup')).toBeVisible();

		await page.getByRole('button', { name: 'Continue' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Opening DroppedNeedle' }))
			.toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/continue')).toHaveLength(1);
	});

	it('shows a failed run with where the v2 files are, and checks again on Try again', async () => {
		const api = fakeApi({
			'GET /api/v3/upgrade/state': signedIn,
			'GET /api/v3/upgrade/progress': {
				layout: 'needs_upgrade',
				run: run({
					running: false,
					step: 'roll_back',
					failure: {
						code: 'IMPORT_FAILED',
						message: 'The import failed: a playlist names no user.',
						v2_files: 'put_back'
					}
				})
			},
			'POST /api/v3/upgrade/preflight': {
				check: { layout: 'v2', counts: {}, orphans: 0, blockers: [], warnings: [] },
				failure: null
			}
		});
		await renderWithQuery(UpgradePage, {});

		await expect.element(page.getByRole('heading', { name: 'The upgrade failed' })).toBeVisible();
		await expect.element(page.getByText('a playlist names no user')).toBeVisible();
		await expect.element(page.getByText('Your v2 files are back where they were.')).toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/preflight')).toHaveLength(0);

		await page.getByRole('button', { name: 'Try again' }).click();
		await expect.element(page.getByRole('heading', { name: 'Before you upgrade' })).toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/preflight')).toHaveLength(1);
	});

	it('offers to finish an interrupted upgrade without running a pre-flight', async () => {
		const api = fakeApi({
			'GET /api/v3/upgrade/state': { layout: 'interrupted', signed_in: true },
			'GET /api/v3/upgrade/progress': { layout: 'interrupted', run: null },
			'POST /api/v3/upgrade/start': {
				layout: 'interrupted',
				run: run({ step: 'recover', total: 2, done: 1 })
			}
		});
		await renderWithQuery(UpgradePage, {});

		await expect
			.element(page.getByRole('heading', { name: "An upgrade didn't finish" }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Finish the upgrade' }).click();

		await expect.element(page.getByText('1 / 2')).toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/start')[0]?.body).toEqual({ acknowledged: [] });
		expect(api.calls('POST /api/v3/upgrade/preflight')).toHaveLength(0);
	});

	it('starts fresh only once the confirm is accepted', async () => {
		const api = fakeApi({
			'GET /api/v3/upgrade/state': signedIn,
			'GET /api/v3/upgrade/progress': { layout: 'needs_upgrade', run: null },
			'POST /api/v3/upgrade/preflight': {
				check: {
					layout: 'v2',
					counts: null,
					orphans: 0,
					blockers: [
						{
							code: 'V2_WAL_UNREADABLE',
							message: "Cannot read v2's library.db-wal.",
							fix: 'Make it readable by the user the upgrade runs as.'
						}
					],
					warnings: []
				},
				failure: null
			},
			'POST /api/v3/upgrade/fresh': {
				layout: 'needs_upgrade',
				run: run({ task: 'fresh', step: 'move_aside', total: 2, done: 0 })
			}
		});
		await renderWithQuery(UpgradePage, {});

		await expect
			.element(page.getByText('Make it readable by the user the upgrade runs as.'))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Upgrade', exact: true })).toBeDisabled();
		await page.getByRole('button', { name: 'Start fresh instead' }).click();
		await expect
			.element(page.getByText("Users, playlists, requests and history won't come across."))
			.toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/fresh')).toHaveLength(0);

		await page.getByRole('button', { name: 'Start fresh', exact: true }).click();
		await expect.element(page.getByRole('heading', { name: 'Starting fresh' })).toBeVisible();
		expect(api.calls('POST /api/v3/upgrade/fresh')[0]?.body).toEqual({ confirm: true });
	});

	it('asks to sign in again when the server no longer holds the sign-in', async () => {
		fakeApi({
			'GET /api/v3/upgrade/state': signedIn,
			'GET /api/v3/upgrade/progress': apiError(
				401,
				'UNAUTHORIZED',
				'Sign in to the upgrade page first'
			)
		});
		await renderWithQuery(UpgradePage, {});

		await expect
			.element(page.getByRole('button', { name: 'Use a code from the server log' }))
			.toBeVisible();
	});
});
