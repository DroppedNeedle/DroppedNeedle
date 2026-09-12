import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	discard: vi.fn()
}));

vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	discardLibraryManagementPreviewMutation: () => ({
		mutateAsync: h.discard,
		isPending: false
	})
}));

import LibraryManagementDiscardPreview from './LibraryManagementDiscardPreview.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
});

describe('LibraryManagementDiscardPreview', () => {
	it('keeps its completion callback when successful invalidation unmounts it', async () => {
		const complete = vi.fn();
		let unmount: (() => Promise<void>) | undefined = undefined;
		h.discard.mockImplementation(async () => {
			await unmount?.();
			return {};
		});
		const view = await render(LibraryManagementDiscardPreview, {
			jobId: 'preview-1',
			expectedRevision: 4,
			profileName: 'Picard-style Organizer',
			ondiscard: complete
		});
		unmount = view.unmount;

		await page.getByRole('button', { name: 'Discard preview...' }).click();
		await page.getByRole('button', { name: 'Discard preview', exact: true }).click();

		await vi.waitFor(() => expect(complete).toHaveBeenCalledOnce());
		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: { expected_operation_row_revision: 4 }
		});
	});
});
