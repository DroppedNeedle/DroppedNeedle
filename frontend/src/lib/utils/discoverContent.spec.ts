import { describe, it, expect } from 'vitest';
import { discoverHasContent } from './discoverContent';
import type { DiscoverResponse } from '$lib/types';

const mk = (partial: Partial<DiscoverResponse>): DiscoverResponse =>
	partial as unknown as DiscoverResponse;

describe('discoverHasContent', () => {
	it('is false for null/undefined and a refreshing-only response', () => {
		expect(discoverHasContent(null)).toBe(false);
		expect(discoverHasContent(undefined)).toBe(false);
		expect(discoverHasContent(mk({ refreshing: true }))).toBe(false);
	});

	it('is true when any renderable section is present', () => {
		expect(
			discoverHasContent(mk({ because_you_listen_to: [{ section: { items: [{}] } }] as never }))
		).toBe(true);
		expect(discoverHasContent(mk({ globally_trending: { items: [{}] } as never }))).toBe(true);
		expect(discoverHasContent(mk({ daily_mixes: [{ items: [{}] }] as never }))).toBe(true);
		expect(discoverHasContent(mk({ genre_list: { items: [{}] } as never }))).toBe(true);
		expect(discoverHasContent(mk({ weekly_exploration: { tracks: [{}] } as never }))).toBe(true);
	});

	it('is false when list-shaped sections are empty', () => {
		expect(
			discoverHasContent(mk({ because_you_listen_to: [], genre_list: { items: [] } as never }))
		).toBe(false);
		expect(discoverHasContent(mk({ globally_trending: { items: [] } as never }))).toBe(false);
		expect(discoverHasContent(mk({ daily_mixes: [{ items: [] }] as never }))).toBe(false);
	});
});
