import { describe, expect, it, vi } from 'vitest';
import { load as settingsLoad } from './settings/+layout';
import { load as reviewLoad } from './library/review/+page';
import { load as unmatchedLoad } from './library/unmatched/+page';
import { load as legacyLibrarySettingsLoad } from './settings/library/+page';

const admin = { id: 'admin-1', role: 'admin' };
const regular = { id: 'user-1', role: 'user' };

function delayedParent(user: typeof admin | typeof regular) {
	let release: ((value: { user: typeof user }) => void) | undefined;
	const parent = vi.fn(
		() =>
			new Promise<{ user: typeof user }>((resolve) => {
				release = resolve;
			})
	);
	return {
		parent,
		release: () => release?.({ user })
	};
}

describe('role-gated loads', () => {
	it('waits for parent hydration before admitting an administrator to Settings', async () => {
		const hydration = delayedParent(admin);
		const result = Promise.resolve(
			settingsLoad({ parent: hydration.parent } as unknown as Parameters<typeof settingsLoad>[0])
		);
		let settled = false;
		void result.finally(() => {
			settled = true;
		});
		await Promise.resolve();
		expect(settled).toBe(false);

		hydration.release();
		await expect(result).resolves.toBeUndefined();
	});

	it.each([
		['Settings', settingsLoad, '/'],
		['Library Review', reviewLoad, '/library'],
		['Unmatched Library', unmatchedLoad, '/library'],
		['legacy library settings', legacyLibrarySettingsLoad, '/']
	])('redirects a regular user from %s after hydration', async (_name, load, location) => {
		const hydration = delayedParent(regular);
		const result = load({ parent: hydration.parent } as never);
		hydration.release();
		await expect(result).rejects.toMatchObject({ status: 302, location });
	});

	it('sends an administrator through the legacy library-settings redirect', async () => {
		await expect(
			legacyLibrarySettingsLoad({ parent: async () => ({ user: admin }) } as never)
		).rejects.toMatchObject({ status: 307, location: '/settings?tab=library' });
	});
});
