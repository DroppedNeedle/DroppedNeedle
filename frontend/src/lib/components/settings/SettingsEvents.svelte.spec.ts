import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { SECRET_MASK } from '$lib/ui/secretMask';

import SettingsEvents from './SettingsEvents.svelte';

const READ = 'GET /api/v3/settings/events';
const SAVE = 'PUT /api/v3/settings/events';

const stored: RouteData<typeof READ> = {
	enabled: true,
	poll_time: '06:00',
	skiddle_api_key: SECRET_MASK,
	skiddle_enabled: false,
	sweep_scope: 'followed',
	ticketmaster_api_key: SECRET_MASK,
	ticketmaster_enabled: false
};

describe('SettingsEvents', () => {
	it("shows the server's reason on the daily check time it refused", async () => {
		const reason = 'must be HH:MM (00:00-23:59), got ""';
		fakeApi({
			[READ]: stored,
			[SAVE]: apiError(400, 'INVALID_INPUT', `Poll time: ${reason}`, {
				field: 'poll_time',
				reason
			})
		});
		await renderWithQuery(SettingsEvents, {});

		const time = page.getByLabelText('Check for new events daily at');
		await time.fill('07:30');
		await page.getByRole('button', { name: 'Save' }).click();

		await expect.element(time).toHaveAttribute('aria-invalid', 'true');
		await expect.element(page.getByRole('alert').getByText(reason)).toBeVisible();
	});
});
