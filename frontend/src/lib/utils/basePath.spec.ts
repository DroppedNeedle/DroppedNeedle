import { describe, it, expect, vi, beforeEach } from 'vitest';

async function loadStripBase(base: string) {
	vi.doMock('$app/paths', () => ({ base }));
	return (await import('./basePath')).stripBase;
}

describe('stripBase', () => {
	beforeEach(() => {
		vi.resetModules();
	});

	it('returns the pathname unchanged when no base path is set', async () => {
		const stripBase = await loadStripBase('');
		expect(stripBase('/login')).toBe('/login');
		expect(stripBase('/')).toBe('/');
		expect(stripBase('/library/review?state=needs_review')).toBe(
			'/library/review?state=needs_review'
		);
	});

	it('removes the base path prefix so route checks compare app-relative paths', async () => {
		const stripBase = await loadStripBase('/droppedneedle');
		expect(stripBase('/droppedneedle/login')).toBe('/login');
		expect(stripBase('/droppedneedle/library/review')).toBe('/library/review');
	});

	it('maps the base path root to /', async () => {
		const stripBase = await loadStripBase('/droppedneedle');
		expect(stripBase('/droppedneedle')).toBe('/');
	});

	it('leaves a pathname outside the base path untouched', async () => {
		const stripBase = await loadStripBase('/droppedneedle');
		expect(stripBase('/elsewhere/login')).toBe('/elsewhere/login');
	});
});
