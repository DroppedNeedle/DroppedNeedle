import { describe, expect, it } from 'vitest';

import type { DownloadActivitySummary } from '$lib/types';

import {
	createDownloadActivityObservation,
	reconcileDownloadActivity
} from './downloadActivitySync';

function summary(
	revision: number,
	landed_release_group_mbids: string[] = []
): DownloadActivitySummary {
	return {
		revision,
		active_count: 0,
		held_count: 0,
		failed_count: 0,
		landed_release_group_mbids
	};
}

describe('download activity revision reconciliation', () => {
	it('uses historical landings only as a first-observation baseline', () => {
		const observed = createDownloadActivityObservation();
		const actions = reconcileDownloadActivity(observed, 'user-1', summary(4, ['RG-A']));

		expect(actions).toEqual({
			refreshDetails: false,
			landedMbids: [],
			invalidateLibrary: false
		});
		expect(observed.landed).toEqual(new Set(['rg-a']));
	});

	it('refreshes detailed and library prefixes only after a structural landing', () => {
		const observed = createDownloadActivityObservation();
		reconcileDownloadActivity(observed, 'user-1', summary(4));

		expect(reconcileDownloadActivity(observed, 'user-1', summary(5, ['rg-a']))).toEqual({
			refreshDetails: true,
			landedMbids: ['rg-a'],
			invalidateLibrary: true
		});
		expect(reconcileDownloadActivity(observed, 'user-1', summary(5, ['rg-a']))).toEqual({
			refreshDetails: false,
			landedMbids: [],
			invalidateLibrary: false
		});
	});

	it('refreshes details when structure changes without changing counts', () => {
		const observed = createDownloadActivityObservation();
		reconcileDownloadActivity(observed, 'user-1', summary(4));

		expect(reconcileDownloadActivity(observed, 'user-1', summary(5))).toEqual({
			refreshDetails: true,
			landedMbids: [],
			invalidateLibrary: false
		});
	});

	it('resets revision and landed observations across account switches', () => {
		const observed = createDownloadActivityObservation();
		reconcileDownloadActivity(observed, 'user-1', summary(9, ['rg-a']));

		expect(reconcileDownloadActivity(observed, 'user-2', summary(2, ['rg-b']))).toEqual({
			refreshDetails: false,
			landedMbids: [],
			invalidateLibrary: false
		});
		expect(observed.userId).toBe('user-2');
	});
});
