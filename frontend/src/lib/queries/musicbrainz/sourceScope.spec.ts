import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: true }));

import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { MusicBrainzQueryKeyFactory } from './MusicBrainzQueryKeyFactory';
import {
	getMusicBrainzSourceScope,
	musicBrainzSourceKey,
	resetMusicBrainzSourceScope,
	setMusicBrainzSourceScope,
	subscribeMusicBrainzSourceScope,
	watchMusicBrainzSourceScope
} from './sourceScope.svelte';

const storage = new Map<string, string>();

beforeEach(() => {
	storage.clear();
	Object.defineProperty(globalThis, 'localStorage', {
		configurable: true,
		value: {
			getItem: (key: string) => storage.get(key) ?? null,
			setItem: (key: string, value: string) => storage.set(key, value),
			removeItem: (key: string) => storage.delete(key)
		}
	});
	authStore.clear();
	resetMusicBrainzSourceScope();
});

afterEach(() => {
	authStore.clear();
});

function user(id: string): AuthUser {
	return {
		id,
		display_name: id,
		role: 'admin',
		email: `${id}@example.test`,
		avatar_url: null,
		username: id,
		username_display: id,
		providers: []
	};
}

function persistScope(
	userId: string,
	sourceMode: string,
	sourceId: string,
	generation: number
): void {
	storage.set(
		`droppedneedle:musicbrainz-source:${encodeURIComponent(userId)}`,
		JSON.stringify({ sourceMode, sourceId, generation })
	);
}

describe('MusicBrainz source scope', () => {
	it('keeps persisted user scope reads pure and isolated from query-key mutation', () => {
		persistScope('user-a', 'mirror', 'mirror-a', 7);
		persistScope('user-b', 'community', 'community-b', 9);

		authStore.setUser(user('user-a'));
		const firstScope = getMusicBrainzSourceScope();
		expect(firstScope).toEqual({
			userId: 'user-a',
			sourceMode: 'mirror',
			sourceId: 'mirror-a',
			generation: 7
		});

		firstScope.sourceId = 'mutated';
		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-a',
			sourceMode: 'mirror',
			sourceId: 'mirror-a',
			generation: 7
		});

		authStore.setUser(user('user-b'));
		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-b',
			sourceMode: 'community',
			sourceId: 'community-b',
			generation: 9
		});

		expect(MusicBrainzQueryKeyFactory.settings()[2]).toEqual({
			user_id: 'user-b',
			source_mode: 'community',
			source_id: 'community-b',
			generation: 9
		});
	});

	it('keeps mismatched scope lookups pure while returning the authenticated user scope', () => {
		const activeScope = {
			source_mode: 'mirror' as const,
			source_id: 'mirror-a',
			generation: 7
		};
		persistScope('user-b', 'community', 'community-b', 9);
		authStore.setUser(user('user-b'));
		setMusicBrainzSourceScope(activeScope, 'user-a');

		let notificationCount = 0;
		const unsubscribe = subscribeMusicBrainzSourceScope(() => {
			notificationCount += 1;
		});
		try {
			expect(getMusicBrainzSourceScope()).toEqual({
				userId: 'user-b',
				sourceMode: 'community',
				sourceId: 'community-b',
				generation: 9
			});
			expect(MusicBrainzQueryKeyFactory.settings()[2]).toEqual({
				user_id: 'user-b',
				source_mode: 'community',
				source_id: 'community-b',
				generation: 9
			});
			expect(notificationCount).toBe(0);

			setMusicBrainzSourceScope(activeScope, 'user-a');
			expect(notificationCount).toBe(0);
		} finally {
			unsubscribe();
		}
	});

	it('uses a passed user id instead of the live auth user for source keys', () => {
		persistScope('user-a', 'mirror', 'mirror-a', 7);
		persistScope('user-b', 'community', 'community-b', 9);
		authStore.setUser(user('user-a'));
		expect(musicBrainzSourceKey('user-b')).toEqual({
			user_id: 'user-b',
			source_mode: 'community',
			source_id: 'community-b',
			generation: 9
		});
	});

	it('keeps the in-memory scope valid when localStorage persistence fails', () => {
		authStore.setUser(user('user-a'));
		Object.defineProperty(globalThis, 'localStorage', {
			configurable: true,
			value: {
				getItem: () => null,
				setItem: () => {
					throw new Error('storage unavailable');
				}
			}
		});

		expect(() =>
			setMusicBrainzSourceScope(
				{ source_mode: 'mirror', source_id: 'mirror-a', generation: 7 },
				'user-a'
			)
		).not.toThrow();
		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-a',
			sourceMode: 'mirror',
			sourceId: 'mirror-a',
			generation: 7
		});
	});

	it('refreshes the active scope when an authenticated session carries source identity', () => {
		authStore.setUser({
			...user('user-a'),
			musicbrainz_source: {
				source_mode: 'official',
				source_id: 'official-a',
				generation: 3
			}
		});

		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-a',
			sourceMode: 'official',
			sourceId: 'official-a',
			generation: 3
		});
	});
	it('refreshes from a cross-tab storage event and cleans up its listener', () => {
		authStore.setUser(user('user-a'));
		setMusicBrainzSourceScope(
			{ source_mode: 'official', source_id: 'official-a', generation: 3 },
			'user-a'
		);
		let changeCount = 0;
		const unsubscribe = subscribeMusicBrainzSourceScope(() => {
			changeCount += 1;
		});

		const listeners = new Set<(event: StorageEvent) => void>();
		const previousWindow = globalThis.window;
		const windowMock = {
			addEventListener: (_type: string, listener: EventListenerOrEventListenerObject) => {
				listeners.add(listener as (event: StorageEvent) => void);
			},
			removeEventListener: (_type: string, listener: EventListenerOrEventListenerObject) => {
				listeners.delete(listener as (event: StorageEvent) => void);
			}
		};
		Object.defineProperty(globalThis, 'window', {
			configurable: true,
			value: windowMock
		});

		try {
			const stop = watchMusicBrainzSourceScope();
			const key = `droppedneedle:musicbrainz-source:${encodeURIComponent('user-a')}`;
			const nextValue = JSON.stringify({
				sourceMode: 'mirror',
				sourceId: 'mirror-a',
				generation: 4
			});
			storage.set(key, nextValue);
			for (const listener of listeners) {
				listener({ key, newValue: nextValue } as StorageEvent);
			}
			for (const listener of listeners) {
				listener({ key, newValue: nextValue } as StorageEvent);
			}
			expect(changeCount).toBe(1);
			expect(getMusicBrainzSourceScope()).toEqual({
				userId: 'user-a',
				sourceMode: 'mirror',
				sourceId: 'mirror-a',
				generation: 4
			});

			stop();
			expect(listeners.size).toBe(0);
			const ignoredValue = JSON.stringify({
				sourceMode: 'community',
				sourceId: 'community-a',
				generation: 5
			});
			for (const listener of listeners) {
				listener({ key, newValue: ignoredValue } as StorageEvent);
			}
			expect(getMusicBrainzSourceScope().sourceId).toBe('mirror-a');
		} finally {
			unsubscribe();
			if (previousWindow === undefined) {
				Reflect.deleteProperty(globalThis, 'window');
			} else {
				Object.defineProperty(globalThis, 'window', {
					configurable: true,
					value: previousWindow
				});
			}
		}
	});

	it('derives admin and trusted guards from the authenticated role', () => {
		authStore.setUser(user('admin'));
		expect(authStore.isAdmin).toBe(true);
		expect(authStore.isTrusted).toBe(true);

		authStore.setUser({ ...user('trusted'), role: 'trusted' });
		expect(authStore.isAdmin).toBe(false);
		expect(authStore.isTrusted).toBe(true);

		authStore.setUser({ ...user('user'), role: 'user' });
		expect(authStore.isAdmin).toBe(false);
		expect(authStore.isTrusted).toBe(false);
	});
});
