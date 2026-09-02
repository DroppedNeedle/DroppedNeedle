import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	policy: {
		summary: 'Lossless preferred.',
		source_mode: 'source_first',
		legacy_rollback_compatible: true,
		quality_recipe_status: 'invalid',
		quality_recipe_error: 'The saved quality recipe is incomplete.'
	} as Record<string, unknown>,
	formData: { enabled: true },
	load: vi.fn(),
	save: vi.fn(),
	cleanup: vi.fn()
}));

vi.mock('$lib/queries/downloads/PolicyQueries.svelte', () => ({
	getPolicySummaryQuery: () => ({
		get data() {
			return h.policy;
		},
		isError: false
	})
}));

vi.mock('$lib/utils/settingsForm.svelte', () => ({
	createSettingsForm: () => ({
		data: h.formData,
		loading: false,
		saving: false,
		message: '',
		messageType: 'success',
		load: h.load,
		save: h.save,
		cleanup: h.cleanup
	})
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import SettingsFreeMusic from './SettingsFreeMusic.svelte';

describe('SettingsFreeMusic policy status', () => {
	beforeEach(() => {
		h.policy = {
			summary: 'Lossless preferred.',
			source_mode: 'source_first',
			legacy_rollback_compatible: true,
			quality_recipe_status: 'invalid',
			quality_recipe_error: 'The saved quality recipe is incomplete.'
		};
		h.load.mockClear();
		h.save.mockClear();
		h.cleanup.mockClear();
	});

	it.each([
		['invalid', 'The saved quality recipe is incomplete.'],
		[
			'non_convertible',
			'The saved policy includes formats outside the supported FLAC and MP3 recipe.'
		]
	] as const)(
		'makes a %s policy actionable instead of showing a healthy summary',
		async (status, detail) => {
			h.policy = {
				...h.policy,
				quality_recipe_status: status,
				quality_recipe_error: status === 'invalid' ? detail : null
			};
			render(SettingsFreeMusic);

			await expect
				.element(page.getByRole('alert'))
				.toHaveTextContent(/Acquisition quality policy needs attention/);
			await expect.element(page.getByRole('alert')).toHaveTextContent(detail);
			await expect.element(page.getByTestId('free-music-policy-summary')).not.toBeInTheDocument();
			await expect
				.element(page.getByRole('link', { name: 'Open acquisition policy' }))
				.toHaveAttribute('href', '/settings?tab=download-client');
		}
	);
});
