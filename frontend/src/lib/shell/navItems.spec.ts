import { describe, expect, it } from 'vitest';
import { navItems, servicePath, type ServiceSource } from './navItems';

const item = (id: string) => navItems.find((entry) => entry.id === id);

const SERVICES: ServiceSource[] = ['localfiles', 'youtube', 'jellyfin', 'navidrome', 'plex'];

describe('navItems', () => {
	it('marks a service page as the service, not Library', () => {
		for (const source of SERVICES) {
			const path = servicePath(source);
			expect(item('library')?.sidebar?.active?.(path), path).toBe(false);
			expect(item('library')?.bottom?.active(path), path).toBe(false);
		}

		expect(item('library')?.sidebar?.active?.('/library/albums')).toBe(true);
		expect(item('library')?.bottom?.active('/library')).toBe(true);
	});

	it('still leaves Library Management and the Recycle Bin their own entries', () => {
		expect(item('library')?.sidebar?.active?.('/library/management')).toBe(false);
		expect(item('library')?.sidebar?.active?.('/library/recycle-bin')).toBe(false);
	});

	it('gives the Listening Room a place in the phone More sheet', () => {
		expect(item('listening-room')?.href).toBe('/library/local');
		expect(item('listening-room')?.more?.active('/library/local')).toBe(true);
		expect(item('listening-room')?.more?.active('/library/albums')).toBe(false);
	});
});
