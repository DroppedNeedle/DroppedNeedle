import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const providerInvalidation = vi.hoisted(() => ({
	run: vi.fn().mockResolvedValue(undefined)
}));
const providerCaches = vi.hoisted(() => ({
	clear: vi.fn()
}));
const afterSaveHook = vi.hoisted(() => ({
	current: null as
		| ((data: {
				api_url: string;
				rate_limit: number;
				concurrent_searches: number;
				clamped_to_official_limits?: boolean;
		  }) => void | Promise<void>)
		| null
}));

const h = vi.hoisted(() => ({
	data: {
		api_url: 'https://musicbrainz.org/ws/2',
		rate_limit: 1.0,
		concurrent_searches: 6,
		clamped_to_official_limits: false
	},
	testResult: null as { valid: boolean; message: string } | null,
	save: vi.fn()
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateMusicBrainzProviderQueries: providerInvalidation.run
}));

vi.mock('$lib/utils/albumDetailCache', () => ({
	clearMusicBrainzProviderCaches: providerCaches.clear
}));

vi.mock('$lib/utils/settingsForm.svelte', () => ({
	createSettingsForm: (config: {
		afterSave?: (data: {
			api_url: string;
			rate_limit: number;
			concurrent_searches: number;
			clamped_to_official_limits?: boolean;
		}) => void | Promise<void>;
	}) => {
		afterSaveHook.current = config.afterSave ?? null;
		return {
			get data() {
				return h.data;
			},
			loading: false,
			saving: false,
			testing: false,
			get testResult() {
				return h.testResult;
			},
			message: '',
			messageType: 'success',
			load: vi.fn(),
			save: h.save,
			test: vi.fn(),
			cleanup: vi.fn()
		};
	}
}));

import SettingsMusicBrainz from './SettingsMusicBrainz.svelte';

function setData(overrides: Partial<typeof h.data>) {
	h.data = { ...h.data, ...overrides };
}

describe('SettingsMusicBrainz three-way source picker', () => {
	beforeEach(() => {
		h.testResult = null;
		h.save.mockReset();
		h.save.mockImplementation(async () => {
			if (afterSaveHook.current) await afterSaveHook.current(h.data);
			return true;
		});
		providerInvalidation.run.mockReset();
		providerInvalidation.run.mockResolvedValue(undefined);
		providerCaches.clear.mockReset();
		providerCaches.clear.mockReturnValue(true);
		setData({
			api_url: 'https://musicbrainz.org/ws/2',
			rate_limit: 1.0,
			concurrent_searches: 6,
			clamped_to_official_limits: false
		});
	});

	it('renders the three selectable cards with Official marked recommended', async () => {
		render(SettingsMusicBrainz);

		await expect.element(page.getByRole('radio', { name: 'Official' })).toBeVisible();
		await expect.element(page.getByRole('radio', { name: 'Self-hosted mirror' })).toBeVisible();
		await expect
			.element(page.getByRole('radio', { name: 'Community / external server' }))
			.toBeVisible();
		await expect
			.element(
				page.getByRole('radio', { name: 'Official' }).getByText('Recommended', { exact: true })
			)
			.toBeVisible();
	});

	it('highlights Official as selected for an official URL and shows the cap copy', async () => {
		render(SettingsMusicBrainz);

		await expect
			.element(page.getByRole('radio', { name: 'Official' }))
			.toHaveAttribute('aria-checked', 'true');
		await expect
			.element(page.getByRole('radio', { name: 'Self-hosted mirror' }))
			.toHaveAttribute('aria-checked', 'false');
		await expect.element(page.getByText(/clamped here, not refused/)).toBeVisible();
	});

	it.each(['https://musicbrainz.org:443/ws/2', 'http://musicbrainz.org:80/ws/2'])(
		'treats an official host with its default port as official (%s)',
		async (apiUrl) => {
			setData({ api_url: apiUrl });
			render(SettingsMusicBrainz);

			await expect
				.element(page.getByRole('radio', { name: 'Official' }))
				.toHaveAttribute('aria-checked', 'true');
		}
	);

	it.each(['https://musicbrainz.org:8443/ws/2', 'http://musicbrainz.org:8080/ws/2'])(
		'treats an official host with a custom port as a mirror (%s)',
		async (apiUrl) => {
			setData({ api_url: apiUrl });
			render(SettingsMusicBrainz);

			await expect
				.element(page.getByRole('radio', { name: 'Self-hosted mirror' }))
				.toHaveAttribute('aria-checked', 'true');
			await expect
				.element(page.getByRole('radio', { name: 'Official' }))
				.toHaveAttribute('aria-checked', 'false');
		}
	);

	it('requires the community acknowledgment before saving', async () => {
		h.testResult = { valid: true, message: 'Connected to MusicBrainz' };
		render(SettingsMusicBrainz);

		await page.getByRole('radio', { name: 'Community / external server' }).click();

		const save = page.getByRole('button', { name: 'Save Settings' });
		await expect.element(save).toBeDisabled();
		await expect.element(page.getByText(/routing identity data through a server/)).toBeVisible();

		// the protocol caveat lives in the collapsed More info disclosure - open it
		await page
			.getByRole('radio', { name: 'Community / external server' })
			.getByText('More info')
			.click();
		await expect.element(page.getByText(/BrainzMash shared pool/)).toBeVisible();

		await page.getByRole('checkbox').click();

		await expect.element(save).toBeEnabled();
	});

	it('shows the clamp warning when the backend applied official limits', async () => {
		setData({ clamped_to_official_limits: true });
		render(SettingsMusicBrainz);

		await expect.element(page.getByText(/Values were clamped to official limits/)).toBeVisible();
	});

	it('loads a non-official URL on the mirror card with banner and guide link', async () => {
		setData({ api_url: 'http://mirror-host:5000/ws/2', rate_limit: 25, concurrent_searches: 20 });
		render(SettingsMusicBrainz);

		await expect
			.element(page.getByRole('radio', { name: 'Self-hosted mirror' }))
			.toHaveAttribute('aria-checked', 'true');
		await expect
			.element(page.getByRole('radio', { name: 'Official' }))
			.toHaveAttribute('aria-checked', 'false');
		await expect.element(page.getByText(/reindex schedule/)).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Mirror setup guide' }))
			.toHaveAttribute('href', '/docs/musicbrainz-mirror-selfhosting.md');
		await expect.element(page.getByText('Unlimited', { exact: true })).not.toBeInTheDocument();
	});
	it('invalidates provider queries and caches once when the source changes', async () => {
		h.testResult = { valid: true, message: 'Connected to MusicBrainz' };
		setData({ api_url: 'http://mirror-host:5000/ws/2', rate_limit: 25, concurrent_searches: 20 });
		render(SettingsMusicBrainz);

		await expect
			.element(page.getByRole('radio', { name: 'Self-hosted mirror' }))
			.toHaveAttribute('aria-checked', 'true');
		setData({ api_url: 'http://another-mirror:5000/ws/2' });

		await page.getByRole('button', { name: 'Save Settings' }).click();
		await vi.waitFor(() => expect(providerInvalidation.run).toHaveBeenCalledTimes(1));
		expect(providerCaches.clear).toHaveBeenCalledTimes(1);
	});

	it('does not invalidate provider data when only rate or concurrency settings change', async () => {
		h.testResult = { valid: true, message: 'Connected to MusicBrainz' };
		render(SettingsMusicBrainz);
		await expect
			.element(page.getByRole('radio', { name: 'Official' }))
			.toHaveAttribute('aria-checked', 'true');
		setData({ rate_limit: 2, concurrent_searches: 4 });

		await page.getByRole('button', { name: 'Save Settings' }).click();
		await vi.waitFor(() => expect(h.save).toHaveBeenCalledTimes(1));
		expect(providerInvalidation.run).not.toHaveBeenCalled();
		expect(providerCaches.clear).not.toHaveBeenCalled();
	});

	it('does not invalidate provider data when the saved source is unchanged', async () => {
		h.testResult = { valid: true, message: 'Connected to MusicBrainz' };
		render(SettingsMusicBrainz);
		await expect
			.element(page.getByRole('radio', { name: 'Official' }))
			.toHaveAttribute('aria-checked', 'true');

		await page.getByRole('button', { name: 'Save Settings' }).click();
		await vi.waitFor(() => expect(h.save).toHaveBeenCalledTimes(1));
		expect(providerInvalidation.run).not.toHaveBeenCalled();
		expect(providerCaches.clear).not.toHaveBeenCalled();
	});

	it('continues cleanup and retries a failed sweep on the same saved source', async () => {
		h.testResult = { valid: true, message: 'Connected to MusicBrainz' };
		setData({ api_url: 'http://mirror-host:5000/ws/2', rate_limit: 25, concurrent_searches: 20 });
		render(SettingsMusicBrainz);

		await expect
			.element(page.getByRole('radio', { name: 'Self-hosted mirror' }))
			.toHaveAttribute('aria-checked', 'true');
		setData({ api_url: 'http://another-mirror:5000/ws/2' });
		providerInvalidation.run.mockRejectedValueOnce(
			new Error('https://user:secret@another-mirror:5000/ws/2')
		);
		providerCaches.clear.mockImplementationOnce(() => {
			throw new Error('browser cache unavailable');
		});

		await page.getByRole('button', { name: 'Save Settings' }).click();
		await vi.waitFor(() => expect(h.save).toHaveBeenCalledTimes(1));
		expect(providerInvalidation.run).toHaveBeenCalledTimes(1);
		expect(providerCaches.clear).toHaveBeenCalledTimes(1);

		await page.getByRole('button', { name: 'Save Settings' }).click();
		await vi.waitFor(() => expect(h.save).toHaveBeenCalledTimes(2));
		expect(providerInvalidation.run).toHaveBeenCalledTimes(2);
		expect(providerCaches.clear).toHaveBeenCalledTimes(2);
	});

	it('keeps the save successful and retries when a provider namespace remains uncleared', async () => {
		h.testResult = { valid: true, message: 'Connected to MusicBrainz' };
		setData({ api_url: 'http://mirror-host:5000/ws/2', rate_limit: 25, concurrent_searches: 20 });
		render(SettingsMusicBrainz);

		await expect
			.element(page.getByRole('radio', { name: 'Self-hosted mirror' }))
			.toHaveAttribute('aria-checked', 'true');
		setData({ api_url: 'http://another-mirror:5000/ws/2' });
		providerCaches.clear.mockReturnValueOnce(false).mockReturnValue(true);

		await page.getByRole('button', { name: 'Save Settings' }).click();
		await vi.waitFor(() => expect(h.save).toHaveBeenCalledTimes(1));
		expect(providerInvalidation.run).toHaveBeenCalledTimes(1);
		expect(providerCaches.clear).toHaveBeenCalledTimes(1);

		await page.getByRole('button', { name: 'Save Settings' }).click();
		await vi.waitFor(() => expect(h.save).toHaveBeenCalledTimes(2));
		expect(providerInvalidation.run).toHaveBeenCalledTimes(2);
		expect(providerCaches.clear).toHaveBeenCalledTimes(2);
	});
});
