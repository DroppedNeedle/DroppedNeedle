import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import RemoveTrackFileDialog from './RemoveTrackFileDialog.svelte';

describe('RemoveTrackFileDialog', () => {
	it('confirms the removal with the captured track title', async () => {
		const onconfirm = vi.fn();
		const screen = render(RemoveTrackFileDialog, {
			trackTitle: 'Aria',
			removing: false,
			error: null,
			onconfirm,
			onclose: vi.fn()
		});

		await expect.element(screen.getByText('Aria')).toBeVisible();
		await screen.getByRole('button', { name: 'Remove' }).click();

		expect(onconfirm).toHaveBeenCalledTimes(1);
	});

	it('renders the failure as an alert and blocks cancel while removing', async () => {
		const screen = render(RemoveTrackFileDialog, {
			trackTitle: 'Aria',
			removing: true,
			error: "Couldn't remove this file",
			onconfirm: vi.fn(),
			onclose: vi.fn()
		});

		await expect.element(screen.getByRole('alert')).toHaveTextContent("Couldn't remove this file");
		await expect.element(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
		await expect.element(screen.getByRole('button', { name: 'Remove' })).toBeDisabled();
	});
});
