import { afterEach, expect, it, vi } from 'vitest';
import { reloadAfterFailedModuleLoad } from './pageErrors';

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

it('reloads once for code that failed to download, never in a loop', () => {
	vi.useFakeTimers({ now: 1_000_000 });
	const storage = new Map<string, string>();
	vi.stubGlobal('sessionStorage', {
		getItem: (key: string) => storage.get(key) ?? null,
		setItem: (key: string, value: string) => storage.set(key, value)
	});
	const reload = vi.fn();
	vi.stubGlobal('location', { reload });

	reloadAfterFailedModuleLoad();
	vi.advanceTimersByTime(1_000);
	expect(reload).toHaveBeenCalledTimes(1);

	// The reloaded page fails again straight away: it stays on the error page.
	reloadAfterFailedModuleLoad();
	vi.advanceTimersByTime(60_000);
	expect(reload).toHaveBeenCalledTimes(1);

	// A later failure gets its own reload.
	reloadAfterFailedModuleLoad();
	vi.advanceTimersByTime(1_000);
	expect(reload).toHaveBeenCalledTimes(2);
});
