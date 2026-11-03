import { describe, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({ libraryLoads: 0, userLoads: 0, musicbrainzLoads: 0 }));

vi.mock('./SettingsLibrary.svelte', () => {
	state.libraryLoads += 1;
	const Component = function () {};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('./SettingsUsers.svelte', () => {
	state.userLoads += 1;
	const Component = function () {};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('./SettingsMusicBrainz.svelte', () => {
	state.musicbrainzLoads += 1;
	const Component = function () {};
	Component.prototype = {};
	return { default: Component };
});

import { isSettingsTabVisible, loadSettingsTab } from './settingsTabs';

describe('settings tab loader', () => {
	it('loads only the requested tab and never imports an unauthorized admin tab', async () => {
		expect(state.libraryLoads).toBe(0);
		expect(state.userLoads).toBe(0);

		await expect(loadSettingsTab('library', false)).resolves.not.toBeNull();
		expect(state.libraryLoads).toBe(1);
		expect(state.userLoads).toBe(0);

		await expect(loadSettingsTab('users', false)).resolves.toBeNull();
		expect(state.userLoads).toBe(0);

		await expect(loadSettingsTab('users', true)).resolves.not.toBeNull();
		expect(state.userLoads).toBe(1);

		await expect(loadSettingsTab('musicbrainz', false)).resolves.toBeNull();
		expect(state.musicbrainzLoads).toBe(0);

		await expect(loadSettingsTab('musicbrainz', true)).resolves.not.toBeNull();
		expect(state.musicbrainzLoads).toBe(1);
	});
});

it('hides MusicBrainz for non-admin route tabs while retaining admin visibility', () => {
	expect(isSettingsTabVisible('musicbrainz', false)).toBe(false);
	expect(isSettingsTabVisible('musicbrainz', true)).toBe(true);
});
