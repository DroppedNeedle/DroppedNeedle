import { beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
	apiLogout: vi.fn(),
	cleanup: vi.fn(),
	goto: vi.fn()
}));

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$app/navigation', () => ({ goto: state.goto }));
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));
vi.mock('$lib/api/client', () => ({ api: { global: { post: state.apiLogout } } }));
vi.mock('$lib/constants', () => ({ API: { auth: { logout: () => '/auth/logout' } } }));
vi.mock('$lib/utils/userSessionCleanup', () => ({ clearUserSessionState: state.cleanup }));

import { logout } from './logout';

beforeEach(() => {
	state.apiLogout.mockReset();
	state.apiLogout.mockResolvedValue(undefined);
	state.cleanup.mockReset();
	state.cleanup.mockResolvedValue(undefined);
	state.goto.mockReset();
	state.goto.mockResolvedValue(undefined);
});

it('finishes logout navigation when session cleanup rejects after the revoke attempt', async () => {
	state.apiLogout.mockRejectedValueOnce(new Error('revoke unavailable'));
	state.cleanup.mockRejectedValueOnce(new Error('IndexedDB unavailable'));

	await expect(logout()).resolves.toBeUndefined();

	expect(state.apiLogout).toHaveBeenCalledWith('/auth/logout');
	expect(state.cleanup).toHaveBeenCalledOnce();
	expect(state.goto).toHaveBeenCalledWith('/login');
});
